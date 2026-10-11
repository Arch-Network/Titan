use {
    super::{
        transaction_update::{TransactionChangeSet, TransactionUpdate},
        *,
    },
    crate::{
        bitcoin_rpc::{RpcClientError, RpcClientPool, RpcClientPoolError},
        index::{
            metrics::Metrics,
            store::Store,
            updater::{
                cache::{
                    BgWriteError, BlockCache, BlockCacheSettings, MempoolCache,
                    MempoolCacheSettings,
                },
                events::Events,
                transaction::{TransactionParser, TransactionUpdater, TxContext},
            },
            Chain, Settings, StoreError,
        },
        models::{BatchUpdate, BlockId, RuneEntry},
    },
    arch_titan_types_core::{Block, Event, MempoolEntry, RuneId, SerializedTxid},
    arch_titan_types_core::{Rune, SpacedRune},
    bitcoin::{
        constants::SUBSIDY_HALVING_INTERVAL, hex::HexToArrayError, Block as BitcoinBlock,
        BlockHash, Transaction,
    },
    bitcoincore_rpc::{
        json::{GetBlockchainInfoResult, GetMempoolEntryResult},
        Client, RpcApi,
    },
    fetcher::{block_fetcher::fetch_blocks_from, mempool_fetcher::MempoolError},
    indicatif::{ProgressBar, ProgressStyle},
    ordinals::Terms,
    prometheus::HistogramVec,
    rollback::{Rollback, RollbackError},
    rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet},
    std::{
        fmt::{self, Display, Formatter},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex, PoisonError, RwLock,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    store_lock::StoreWithLock,
    thiserror::Error,
    tokio::sync::mpsc::{error::SendError, Sender},
    tracing::{debug, error, info, warn},
};

#[derive(Debug, Error)]
pub enum ReorgError {
    /// `depth` blocks below block count `height` were reverted.
    Recoverable {
        height: u64,
        depth: u64,
    },
    Unrecoverable,
}

impl Display for ReorgError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Recoverable { height, depth } => {
                write!(f, "{depth} block deep reorg detected at height {height}")
            }
            Self::Unrecoverable => write!(f, "unrecoverable reorg detected"),
        }
    }
}

#[derive(Error, Debug)]
pub enum UpdaterError {
    #[error("db error {0}")]
    DB(#[from] StoreError),
    #[error("bitcoin rpc error {0}")]
    BitcoinRpc(#[from] bitcoincore_rpc::Error),
    #[error("bitcoin reorg error {0}")]
    BitcoinReorg(#[from] ReorgError),
    #[error("bitcoin rpc client error {0}")]
    BitcoinRpcClient(#[from] RpcClientError),
    #[error("transaction updater error {0}")]
    TransactionUpdater(#[from] TransactionUpdaterError),
    #[error("rollback error {0}")]
    Rollback(#[from] RollbackError),
    #[error("rune parser error {0}")]
    RuneParser(TransactionParserError),
    #[error("txid error {0}")]
    Txid(#[from] HexToArrayError),
    #[error("mempool error {0}")]
    Mempool(#[from] MempoolError),
    #[error("mutex error")]
    Mutex,
    #[error("event sender error {0}")]
    EventSender(#[from] Box<SendError<Event>>),
    #[error("invalid main chain tip")]
    InvalidMainChainTip,
    #[error("bitcoin rpc pool error {0}")]
    BitcoinRpcPool(#[from] RpcClientPoolError),
    #[error("{0}")]
    BackgroundWrite(#[from] BgWriteError),
}

impl From<SendError<Event>> for UpdaterError {
    fn from(error: SendError<Event>) -> Self {
        Self::EventSender(Box::new(error))
    }
}

impl From<TransactionParserError> for UpdaterError {
    fn from(error: TransactionParserError) -> Self {
        match error {
            // Retryable like any other RPC failure (see `Index::index`).
            TransactionParserError::BitcoinRpc(e) => Self::BitcoinRpc(e),
            other => Self::RuneParser(other),
        }
    }
}

type Result<T, E = UpdaterError> = std::result::Result<T, E>;

/// Where the indexed tip stands relative to bitcoind's best chain.
#[derive(Debug, PartialEq)]
enum IndexTip {
    /// Same chain, same height.
    Synced,
    /// Same chain, bitcoind has more blocks.
    Behind,
    /// Bitcoind's best block is an indexed block below the tip: it is behind the
    /// index on the same chain (restarted, still syncing, invalidated blocks...).
    NodeBehind,
    /// The top `depth` indexed blocks are not on bitcoind's best chain.
    Reorged { depth: u64 },
}

/// Removes the txid from `pre_index_submitted_txs` when dropped, so mempool
/// indexing stops skipping it however broadcast indexing ends.
struct PreIndexedTx<'a> {
    txs: &'a RwLock<HashSet<SerializedTxid>>,
    txid: SerializedTxid,
}

impl Drop for PreIndexedTx<'_> {
    fn drop(&mut self) {
        self.txs
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.txid);
    }
}

pub struct Updater {
    db: Arc<StoreWithLock>,
    settings: Settings,
    is_at_tip: AtomicBool,

    bitcoin_rpc_pool: RpcClientPool,

    shutdown_flag: Arc<AtomicBool>,

    broadcast_lock: Mutex<()>,
    pre_index_submitted_txs: RwLock<HashSet<SerializedTxid>>,

    zmq_received_txs: RwLock<HashMap<SerializedTxid, Transaction>>,

    transaction_update: RwLock<TransactionUpdate>,

    sender: Option<Sender<Event>>,

    // monitoring
    latency: HistogramVec,
}

impl Updater {
    pub fn new(
        db: Arc<dyn Store + Send + Sync>,
        bitcoin_rpc_pool: RpcClientPool,
        settings: Settings,
        metrics: &Metrics,
        shutdown_flag: Arc<AtomicBool>,
        sender: Option<Sender<Event>>,
    ) -> Self {
        Self {
            db: Arc::new(StoreWithLock::new(db)),
            settings,
            bitcoin_rpc_pool,
            is_at_tip: AtomicBool::new(false),
            broadcast_lock: Mutex::new(()),
            pre_index_submitted_txs: RwLock::new(HashSet::default()),
            zmq_received_txs: RwLock::new(HashMap::default()),
            shutdown_flag,
            transaction_update: RwLock::new(TransactionUpdate::default()),
            sender,
            latency: metrics.histogram_vec(
                prometheus::HistogramOpts::new("indexer_latency", "Indexer latency"),
                &["method"],
            ),
        }
    }

    pub fn is_at_tip(&self) -> bool {
        self.is_at_tip.load(Ordering::Relaxed)
    }

    /// Compare the indexed tip with bitcoind's best chain.
    fn check_index_tip(
        &self,
        cache: &mut BlockCache,
        chain_info: &GetBlockchainInfoResult,
        client: &Client,
    ) -> Result<IndexTip> {
        let Some(tip_height) = cache.get_block_count().checked_sub(1) else {
            // Nothing indexed yet.
            return Ok(IndexTip::Behind);
        };

        index_tip(
            tip_height,
            chain_info.blocks,
            self.settings.max_recoverable_reorg_depth(),
            |height| Ok(cache.get_block_hash(height)?),
            |height| Ok(client.get_block_hash(height)?),
        )
    }

    /// Whether `block` builds on the indexed tip.
    fn extends_index_tip(&self, block: &BitcoinBlock, cache: &mut BlockCache) -> Result<bool> {
        let Some(tip_height) = cache.get_block_count().checked_sub(1) else {
            return Ok(true);
        };

        // Regtest doesn't index its genesis block.
        if tip_height == 0 && self.settings.chain == Chain::Regtest {
            return Ok(true);
        }

        Ok(cache.get_block_hash(tip_height)? == block.header.prev_blockhash)
    }

    fn is_already_at_tip(
        &self,
        chain_info: &bitcoincore_rpc::json::GetBlockchainInfoResult,
    ) -> Result<bool> {
        let db_read = self.db.read();
        let db_block_count = db_read.get_block_count()?;
        let db_tip_height = db_block_count.saturating_sub(1);

        let heights_match =
            db_tip_height == chain_info.blocks || (db_tip_height == 0 && chain_info.blocks == 0);

        if !heights_match {
            return Ok(false);
        }

        if db_block_count == 0 {
            return Ok(true);
        }

        Ok(db_read.get_block_hash(db_tip_height)? == chain_info.best_block_hash)
    }

    fn mark_as_at_tip(&self) {
        if !self.shutdown_flag.load(Ordering::SeqCst) {
            let was_at_tip = self.is_at_tip.swap(true, Ordering::AcqRel);
            let db = self.db.write();
            if !was_at_tip {
                if let Err(e) = db.finish_bulk_load() {
                    warn!("Failed to switch database to online mode: {:?}", e);
                }
            }

            if let Err(e) = db.set_is_at_tip(true) {
                warn!("Failed to persist is_at_tip=true: {:?}", e);
            }
        }
    }

    pub fn mark_as_not_at_tip(&self) {
        use std::sync::atomic::Ordering;
        self.is_at_tip.store(false, Ordering::Release);
        let db = self.db.write();
        if let Err(e) = db.set_is_at_tip(false) {
            warn!("Failed to persist is_at_tip=false: {:?}", e);
        }
    }

    /// Lightweight entry-point that performs a quick tip check and, if needed, runs a full sync.
    pub fn update_to_tip(&self) -> Result<()> {
        debug!("Updating to tip");

        let chain_info = self.bitcoin_rpc_pool.get()?.get_blockchain_info()?;

        if self.is_already_at_tip(&chain_info)? {
            self.mark_as_at_tip();
            return Ok(());
        }

        self.perform_full_sync()?;
        Ok(())
    }

    fn perform_full_sync(&self) -> Result<()> {
        debug!("Updating to tip");

        // Every 5000 blocks, commit the changes to the database
        let commit_interval = self.settings.commit_interval as usize;

        // Get RPC client and current blockchain info before doing any heavy work
        let bitcoin_block_client = self.bitcoin_rpc_pool.get()?;
        let mut chain_info = bitcoin_block_client.get_blockchain_info()?;

        // Not at tip – proceed with full indexing workflow.
        let mut cache = BlockCache::new(self.db.clone(), BlockCacheSettings::new(&self.settings))?;
        let mut events = Events::new();

        let mut indexing_first_block = true;
        let mut synced = false;

        while !self.shutdown_flag.load(Ordering::SeqCst) {
            match self.check_index_tip(&mut cache, &chain_info, &bitcoin_block_client)? {
                IndexTip::Synced => {
                    synced = true;
                    break;
                }
                IndexTip::Behind => (),
                IndexTip::NodeBehind => {
                    // Nothing to fetch and nothing to revert: wait for bitcoind to
                    // catch up or switch chains.
                    self.mark_as_not_at_tip();
                    warn!(
                        "Bitcoin node is at height {}, below the indexed tip {}; waiting",
                        chain_info.blocks,
                        cache.get_block_height_tip()
                    );
                    thread::sleep(Duration::from_millis(self.settings.main_loop_interval));
                    chain_info = bitcoin_block_client.get_blockchain_info()?;
                    continue;
                }
                IndexTip::Reorged { depth } => {
                    return self.handle_reorg(cache, &mut events, depth);
                }
            }

            self.mark_as_not_at_tip();

            let progress_bar =
                self.open_progress_bar(cache.get_block_height_tip(), chain_info.blocks);

            let current_block_count = cache.get_block_count();

            let (rx, block_fetch_stats) = fetch_blocks_from(
                self.bitcoin_rpc_pool.clone(),
                current_block_count,
                chain_info.blocks + 1,
                self.shutdown_flag.clone(),
            )?;

            while let Ok(block) = rx.recv() {
                // We have consumed a block from the final channel, reflect that in the stats
                block_fetch_stats.decrement_final();

                let _timer = self
                    .latency
                    .with_label_values(&["full_block_indexing"])
                    .start_timer();

                if self.shutdown_flag.load(Ordering::SeqCst) {
                    info!("Updater received shutdown signal, stopping...");
                    break;
                }

                // bitcoind's chain changed since the fetch started: re-check the tip
                // (and revert to the common ancestor if needed) before indexing more.
                if !self.extends_index_tip(&block, &mut cache)? {
                    info!(
                        "Block {} does not extend the indexed tip, re-checking the chain",
                        block.header.block_hash()
                    );
                    break;
                }

                let block = self.index_block(
                    block,
                    cache.get_block_count(),
                    &bitcoin_block_client,
                    &mut cache,
                    &mut events,
                )?;

                events.add_event(Event::NewBlock {
                    block_height: cache.get_block_count(),
                    block_hash: block.header.block_hash(),
                });

                cache.set_new_block(block);

                if cache.should_flush(commit_interval) {
                    info!("Flushing cache");

                    let _timer = self
                        .latency
                        .with_label_values(&["flush_cache"])
                        .start_timer();

                    cache.add_address_events(&mut events, self.settings.chain);

                    // Flush asynchronously, and emit events only after persistence completes
                    self.emit_events_after_persist(&mut cache, &mut events)?;
                }

                indexing_first_block = false;
                progress_bar.inc(1);

                if chain_info
                    .blocks
                    .saturating_sub(cache.get_block_height_tip())
                    > self.settings.max_recoverable_reorg_depth()
                {
                    // clear notifications.
                    self.notify_tx_updates(true)?;
                }
            }

            progress_bar.finish_and_clear();
            chain_info = bitcoin_block_client.get_blockchain_info()?;
        }

        if self.shutdown_flag.load(Ordering::SeqCst) {
            info!("Updater received shutdown signal, stopping...");
        }

        if !indexing_first_block {
            info!("Flushing cache");

            if self.settings.index_addresses {
                cache.add_address_events(&mut events, self.settings.chain);
            }

            self.emit_events_after_persist(&mut cache, &mut events)?;
        }

        // Wait until everything is persisted: a failed background write surfaces
        // here, and the tip is only marked once it is on disk.
        cache.close()?;

        if synced {
            info!("Synced to tip {}", chain_info.blocks);
            self.mark_as_at_tip();
        }

        Ok(())
    }

    fn emit_events_after_persist(&self, cache: &mut BlockCache, events: &mut Events) -> Result<()> {
        let rx = cache.flush_with_notify()?;
        let mut pending_events = events.take_all();
        let sender_clone = self.sender.clone();
        std::thread::spawn(move || {
            if rx.recv().is_err() {
                // The batch was not persisted; the next flush or close reports why.
                error!("emit_events_after_persist: batch not persisted, dropping its events");
                return;
            }

            info!("emit_events_after_persist: events emitted");
            if let Some(sender) = sender_clone {
                for event in pending_events.drain(..) {
                    let result = sender.blocking_send(event);
                    if let Err(e) = result {
                        error!("Failed to send event: {:?} in emit_events_after_persist", e);
                    }
                }
            }
        });
        Ok(())
    }

    pub fn index_mempool(&self) -> Result<()> {
        let _timer = self
            .latency
            .with_label_values(&["index_mempool"])
            .start_timer();

        let client = self.bitcoin_rpc_pool.get()?;

        // Get current mempool transactions
        let lock = self
            .broadcast_lock
            .lock()
            .map_err(|_| UpdaterError::Mutex)?;
        let current_mempool = {
            let current_mempool = client.get_raw_mempool_verbose()?;
            current_mempool
                .into_iter()
                .map(|(txid, mempool_entry)| (txid.into(), mempool_entry))
                .collect::<HashMap<SerializedTxid, GetMempoolEntryResult>>()
        };

        // Get our previously indexed mempool transactions
        let stored_mempool = {
            let db = self.db.read();
            db.get_mempool_txids()?
        };

        drop(lock);

        // Find new transactions to index
        let (new_txs, new_txs_with_mempool_entry): (
            Vec<SerializedTxid>,
            Vec<(SerializedTxid, MempoolEntry)>,
        ) = current_mempool
            .iter()
            .filter(|(txid, _)| !stored_mempool.contains_key(txid))
            .map(|(txid, mempool_entry)| (*txid, (*txid, MempoolEntry::from(mempool_entry))))
            .unzip();

        // Find transactions to remove (they're no longer in mempool)
        let removed_txs: Vec<SerializedTxid> = stored_mempool
            .keys()
            .filter(|txid| !current_mempool.contains_key(txid))
            .cloned()
            .collect();

        // Revert the transactions that left the mempool before indexing the new ones:
        // an RBF replacement spends the outputs of the transaction it replaced, and
        // reverting that one afterwards would mark them unspent again.
        let removed_len = removed_txs.len();
        if removed_len > 0 {
            self.remove_txs(&removed_txs, true)?;
        }

        // Index new transactions
        let new_txs_len = new_txs.len();

        let mut cache =
            MempoolCache::new(self.db.clone(), MempoolCacheSettings::new(&self.settings))?;

        let mut events = Events::new();

        let updated_txids =
            self.update_mempool_entries(&mut cache, &stored_mempool, &current_mempool);

        self.send_mempool_events(
            &mut events,
            new_txs_with_mempool_entry,
            removed_txs.clone(),
            updated_txids,
        )?;

        if new_txs_len > 0 {
            let tx_map = self.choose_mempool_transactions_to_index(&new_txs)?;

            let tx_order =
                fetcher::mempool_fetcher::sort_transaction_order(&current_mempool, &tx_map)?;

            // Store mempool entries for new transactions
            for txid in &tx_order {
                let tx = tx_map.get(txid).unwrap();
                let mempool_entry = current_mempool.get(txid).unwrap();

                self.index_tx(
                    &client,
                    txid,
                    tx,
                    MempoolEntry::from(mempool_entry),
                    &mut cache,
                    &mut events,
                )?;
            }

            if self.settings.index_addresses {
                cache.add_address_events(&mut events, self.settings.chain);
            }
        }

        cache.flush()?;

        if let Err(e) = events.send_events(&self.sender) {
            if !self.shutdown_flag.load(Ordering::SeqCst) {
                error!("Failed to send events: {:?}", e);
            }
        }

        if new_txs_len > 0 || removed_len > 0 {
            debug!(
                "Mempool: New txs: {}. Removed txs: {}",
                new_txs_len, removed_len
            );
        }

        self.transaction_update
            .write()
            .map_err(|_| UpdaterError::Mutex)?
            .update_mempool(TransactionChangeSet {
                added: new_txs.into_iter().collect(),
                removed: removed_txs.into_iter().collect(),
            });

        self.zmq_received_txs
            .write()
            .map_err(|_| UpdaterError::Mutex)?
            .clear();

        Ok(())
    }

    fn update_mempool_entries(
        &self,
        cache: &mut MempoolCache,
        stored_mempool: &HashMap<SerializedTxid, MempoolEntry>,
        current_mempool: &HashMap<SerializedTxid, GetMempoolEntryResult>,
    ) -> Vec<(SerializedTxid, MempoolEntry)> {
        let mut updated_txids = Vec::new();
        for (stored_txid, stored_mempool_entry) in stored_mempool {
            if let Some(mempool_entry_result) = current_mempool.get(stored_txid) {
                let mempool_entry = MempoolEntry::from(mempool_entry_result);

                if *stored_mempool_entry != mempool_entry {
                    cache.set_mempool_tx(*stored_txid, mempool_entry.clone());
                    updated_txids.push((*stored_txid, mempool_entry.clone()));
                }
            }
        }

        updated_txids
    }

    fn send_mempool_events(
        &self,
        events: &mut Events,
        new_txids: Vec<(SerializedTxid, MempoolEntry)>,
        removed_txids: Vec<SerializedTxid>,
        updated_txids: Vec<(SerializedTxid, MempoolEntry)>,
    ) -> Result<()> {
        if !new_txids.is_empty() {
            events.add_event(Event::MempoolTransactionsAdded { txids: new_txids });
        }

        if !removed_txids.is_empty() {
            events.add_event(Event::MempoolTransactionsReplaced {
                txids: removed_txids,
            });
        }

        if !updated_txids.is_empty() {
            events.add_event(Event::MempoolEntriesUpdated {
                txids: updated_txids,
            });
        }

        Ok(())
    }

    fn choose_mempool_transactions_to_index(
        &self,
        new_txs: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Transaction>> {
        // Filter out pre-indexed transactions
        let new_txs = {
            let pre_indexed_txs = self
                .pre_index_submitted_txs
                .read()
                .map_err(|_| UpdaterError::Mutex)?
                .clone();

            new_txs
                .iter()
                .filter(|txid| !pre_indexed_txs.contains(*txid))
                .cloned()
                .collect::<Vec<_>>()
        };

        // Get all zmq transactions that we have received.
        let mut zmq_txns = {
            let zmq_txns = self
                .zmq_received_txs
                .read()
                .map_err(|_| UpdaterError::Mutex)?
                .clone();

            zmq_txns
        };

        // Partition the new transactions into those that we already have and those that we need to fetch.
        let (to_index, to_fetch): (Vec<SerializedTxid>, Vec<SerializedTxid>) = new_txs
            .iter()
            .partition(|txid| zmq_txns.contains_key(*txid));

        // If we don't have all the transactions that we received, remove the ones that we don't have.
        if to_index.len() != zmq_txns.len() {
            zmq_txns.retain(|txid, _| to_index.contains(txid));
        }

        // Fetch the transactions that we need to index.
        if !to_fetch.is_empty() {
            let fetched = fetcher::mempool_fetcher::fetch_transactions(
                self.bitcoin_rpc_pool.clone(),
                &to_fetch,
                self.shutdown_flag.clone(),
            );

            zmq_txns.extend(fetched);
        }

        // Return the transactions that we need to index.
        Ok(zmq_txns)
    }

    fn index_block(
        &self,
        bitcoin_block: BitcoinBlock,
        height: u64,
        rpc_client: &Client,
        cache: &mut BlockCache,
        events: &mut Events,
    ) -> Result<Block> {
        let _timer = self
            .latency
            .with_label_values(&["index_block"])
            .start_timer();

        let mut transaction_parser =
            TransactionParser::new(rpc_client, self.settings.chain, height, false)?;

        let block_header: bitcoin::block::Header = bitcoin_block.header;
        let block_height = height;

        let _block_tx_timer = self
            .latency
            .with_label_values(&["parse_block&index_block_txs"])
            .start_timer();

        let mut transaction_updater = TransactionUpdater::new(self.settings.clone().into(), false)?;

        let mut block = Block::empty_block(height, bitcoin_block.header);

        for (i, tx) in bitcoin_block.txdata.iter().enumerate() {
            let txid = tx.compute_txid().into();
            let result = transaction_parser.parse(cache, u32::try_from(i).unwrap(), tx)?;

            debug!("Indexing tx {} in block {}", txid, block_height);
            transaction_updater.save(
                cache,
                events,
                TxContext {
                    block_time: block_header.time,
                    block_id: Some(BlockId {
                        hash: bitcoin_block.header.block_hash(),
                        height: block_height,
                    }),
                    txid,
                    transaction: &bitcoin_block.txdata[i],
                },
                &result,
            )?;
            block.tx_ids.push(txid);
            if let Some((id, ..)) = result.etched {
                block.etched_runes.push(id);
            }
        }

        // Only a fully indexed block counts: on error the block is indexed again.
        let mut transaction_update = self
            .transaction_update
            .write()
            .map_err(|_| UpdaterError::Mutex)?;

        for txid in &block.tx_ids {
            transaction_update.add_block_tx(*txid);
        }

        Ok(block)
    }

    pub fn pre_index_new_submitted_transaction(&self, txid: &SerializedTxid) -> Result<()> {
        let mut pre_index_submitted_txs = self
            .pre_index_submitted_txs
            .write()
            .map_err(|_| UpdaterError::Mutex)?;
        pre_index_submitted_txs.insert(*txid);
        Ok(())
    }

    pub fn remove_pre_index_new_submitted_transaction(&self, txid: &SerializedTxid) -> Result<()> {
        let mut pre_index_submitted_txs = self
            .pre_index_submitted_txs
            .write()
            .map_err(|_| UpdaterError::Mutex)?;
        pre_index_submitted_txs.remove(txid);
        Ok(())
    }

    /// Index a transaction this node just broadcast, with the broadcasting request's
    /// `rpc_client`: getting one from the pool under `broadcast_lock` could deadlock
    /// once concurrent broadcasts hold every pooled client.
    pub fn index_new_submitted_tx(
        &self,
        rpc_client: &Client,
        txid: &SerializedTxid,
        tx: &Transaction,
        mempool_entry: MempoolEntry,
    ) -> Result<()> {
        let _pre_indexed = PreIndexedTx {
            txs: &self.pre_index_submitted_txs,
            txid: *txid,
        };

        let mut events = Events::new();

        {
            let _lock = self
                .broadcast_lock
                .lock()
                .map_err(|_| UpdaterError::Mutex)?;

            let mut cache =
                MempoolCache::new(self.db.clone(), MempoolCacheSettings::new(&self.settings))?;

            let indexed = self.index_tx(
                rpc_client,
                txid,
                tx,
                mempool_entry.clone(),
                &mut cache,
                &mut events,
            )?;
            if !indexed {
                return Ok(());
            }

            events.add_event(Event::TransactionSubmitted {
                txid: *txid,
                entry: mempool_entry,
            });

            if self.settings.index_addresses {
                cache.add_address_events(&mut events, self.settings.chain);
            }

            cache.flush()?;
        }

        // Deliver outside `broadcast_lock` so a slow subscriber can't stall mempool indexing.
        if let Err(e) = events.send_events(&self.sender) {
            if !self.shutdown_flag.load(Ordering::SeqCst) {
                error!("Failed to send events: {:?}", e);
            }
        }

        Ok(())
    }

    pub fn index_zmq_tx(&self, txid: SerializedTxid, tx: Transaction) -> Result<()> {
        self.zmq_received_txs
            .write()
            .map_err(|_| UpdaterError::Mutex)?
            .insert(txid, tx);
        Ok(())
    }

    /// The caller supplies `rpc_client`, so no pool client is taken while holding
    /// `broadcast_lock` (see `index_new_submitted_tx`).
    fn index_tx(
        &self,
        rpc_client: &Client,
        txid: &SerializedTxid,
        tx: &Transaction,
        mempool_entry: MempoolEntry,
        cache: &mut MempoolCache,
        events: &mut Events,
    ) -> Result<bool> {
        if cache.does_tx_exist(*txid)? {
            debug!("Skipping tx {} in mempool because it already exists", txid);
            return Ok(false);
        }

        let _timer = self.latency.with_label_values(&["index_tx"]).start_timer();

        let height = cache.get_block_count();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut transaction_parser =
            TransactionParser::new(rpc_client, self.settings.chain, height, true)?;

        let result = transaction_parser.parse(cache, 0, tx)?;
        debug!("Indexing tx {}", txid);

        let mut transaction_updater = TransactionUpdater::new(self.settings.clone().into(), true)?;
        transaction_updater.save(
            cache,
            events,
            TxContext {
                block_time: now as u32,
                block_id: None,
                txid: *txid,
                transaction: tx,
            },
            &result,
        )?;

        cache.set_mempool_tx(*txid, mempool_entry);

        Ok(true)
    }

    fn remove_txs(&self, txids: &[SerializedTxid], mempool: bool) -> Result<()> {
        let db = self.db.write();
        let mut rollback_updater = Rollback::new(&db, self.settings.clone().into(), mempool)?;
        rollback_updater.revert_transactions(txids)?;
        Ok(())
    }

    fn open_progress_bar(&self, current_height: u64, total_height: u64) -> ProgressBar {
        let progress_bar: ProgressBar = ProgressBar::new(total_height);
        progress_bar.set_position(current_height);
        progress_bar.set_style(
            ProgressStyle::with_template("[indexing blocks] {wide_bar} {pos}/{len}").unwrap(),
        );

        progress_bar
    }

    /// Revert the top `depth` indexed blocks, which are no longer on bitcoind's best
    /// chain, and return `ReorgError::Recoverable` so the sync restarts from the
    /// common ancestor with fresh caches.
    fn handle_reorg(&self, mut cache: BlockCache, events: &mut Events, depth: u64) -> Result<()> {
        // we're not at tip anymore.
        self.mark_as_not_at_tip();

        // Persist and announce what was indexed so far: the revert works on the store
        // alone, and subscribers must have seen every block a Reorg event rolls back.
        if self.settings.index_addresses {
            cache.add_address_events(events, self.settings.chain);
        }
        let block_count = cache.get_block_count();
        cache.flush_sync()?;
        cache.close()?;

        if let Err(e) = events.send_events(&self.sender) {
            error!("Failed to send events before reorg: {:?}", e);
        }

        info!(
            "Reorg detected at height {}, rolling back {} blocks",
            block_count, depth
        );

        // Top-down, one block per write, each lowering the block count with it: if
        // this stops halfway, the store is consistent and the next sync resumes.
        for height in (block_count - depth..block_count).rev() {
            let block = self.get_block_by_height(height)?;
            self.revert_block(height, &block)?;
        }

        if let Some(sender) = &self.sender {
            let event = Event::Reorg {
                height: block_count,
                depth,
            };
            if let Err(e) = sender.blocking_send(event) {
                error!("Failed to send reorg event: {:?}", e);
            }
        }

        Err(ReorgError::Recoverable {
            height: block_count,
            depth,
        }
        .into())
    }

    fn get_block_by_height(&self, height: u64) -> Result<Block> {
        let db = self.db.read();
        let block_hash = db.get_block_hash(height)?;
        let block = db.get_block_by_hash(&block_hash)?;
        Ok(block)
    }

    fn revert_block(&self, height: u64, block: &Block) -> Result<()> {
        {
            let db = self.db.write();
            let mut rollback_updater = Rollback::new(&db, self.settings.clone().into(), false)?;
            rollback_updater.revert_block(height, block.header.block_hash(), &block.tx_ids)?;
        }

        {
            let mut transaction_update = self
                .transaction_update
                .write()
                .map_err(|_| UpdaterError::Mutex)?;

            for txid in block.tx_ids.iter().rev() {
                transaction_update.remove_block_tx(*txid);
            }
        }

        info!(
            "Reverted block {}:{} with {} txs",
            height,
            block.header.block_hash(),
            block.tx_ids.len()
        );

        Ok(())
    }

    pub fn notify_tx_updates(&self, flush: bool) -> Result<()> {
        let _timer = self
            .latency
            .with_label_values(&["notify_tx_updates"])
            .start_timer();

        let (categorized, block_added_after_mined_and_reorg) = {
            let transaction_update = self
                .transaction_update
                .read()
                .map_err(|_| UpdaterError::Mutex)?;

            let change_set = transaction_update.categorize_to_change_set();
            let block_added_after_mined_and_reorg =
                transaction_update.block_added_after_mined_and_reorg();
            (change_set, block_added_after_mined_and_reorg)
        };

        if let Some(sender) = &self.sender {
            if !flush && (!categorized.removed.is_empty() || !categorized.added.is_empty()) {
                let client = self.bitcoin_rpc_pool.get()?;

                let chain_info = client.get_blockchain_info()?;

                let block = {
                    let db = self.db.read();
                    db.get_block_by_hash(&chain_info.best_block_hash)
                };

                let Ok(block) = block else {
                    return Err(UpdaterError::InvalidMainChainTip);
                };

                if block.height != chain_info.blocks {
                    return Err(UpdaterError::InvalidMainChainTip);
                }
            }

            if !categorized.removed.is_empty() {
                let (_, not_exists) = {
                    let db = self.db.read();
                    db.partition_transactions_by_existence(
                        &categorized.removed.into_iter().collect::<Vec<_>>(),
                    )?
                };

                sender.blocking_send(Event::TransactionsReplaced { txids: not_exists })?;
            }

            // Final added set: normal newly-added UNION
            // the txs that were mined, reorged out, and re-mined (appeared in block_added more than in block_removed),
            let mut added = categorized.added;
            if !block_added_after_mined_and_reorg.is_empty() {
                for txid in block_added_after_mined_and_reorg {
                    added.insert(txid);
                }
            }

            if !added.is_empty() {
                sender.blocking_send(Event::TransactionsAdded {
                    txids: added.into_iter().collect(),
                })?;
            }
        }

        self.transaction_update
            .write()
            .map_err(|_| UpdaterError::Mutex)?
            .reset();

        Ok(())
    }

    /// On mainnet, seed a fresh index with the UNCOMMON•GOODS rune, as ord does when
    /// it creates its database. Does nothing once anything was indexed.
    pub fn insert_genesis_rune(&self) -> Result<()> {
        if self.settings.chain != Chain::Mainnet {
            return Ok(());
        }

        let db = self.db.write();
        if insert_genesis_rune(&**db)? {
            info!("Inserted genesis rune UNCOMMON•GOODS (1:0)");
        }

        Ok(())
    }
}

/// Store ord's genesis rune 1:0 (UNCOMMON•GOODS, rune number 0) if nothing is
/// indexed yet. Returns whether it was inserted.
fn insert_genesis_rune(db: &dyn Store) -> Result<bool, StoreError> {
    if db.get_block_count()? != 0 || db.get_runes_count()? != 0 {
        return Ok(false);
    }

    let rune = Rune(2055900680524219742);
    let id = RuneId::new(1, 0);

    let entry = RuneEntry {
        block: id.block,
        burned: 0,
        divisibility: 0,
        etching: SerializedTxid::all_zeros(),
        terms: Some(Terms {
            amount: Some(1),
            cap: Some(u128::MAX),
            height: (
                Some((SUBSIDY_HALVING_INTERVAL * 4).into()),
                Some((SUBSIDY_HALVING_INTERVAL * 5).into()),
            ),
            offset: (None, None),
        }),
        mints: 0,
        number: 0,
        premine: 0,
        spaced_rune: SpacedRune { rune, spacers: 128 },
        symbol: Some('\u{29C9}'),
        timestamp: 0,
        turbo: true,
        pending_burns: 0,
        pending_mints: 0,
        inscription_id: None,
    };

    // One block batch: the entry, its name and number mappings and the rune count
    // are written together.
    let mut update = BatchUpdate::new(1, 0, 0);
    update.runes.insert(id, entry);
    update.rune_ids.insert(rune.0, id);
    update.rune_numbers.insert(0, id);
    db.batch_update(&update, false)?;

    Ok(true)
}

/// Where the index tip at `tip_height` stands relative to the node's best chain
/// of height `node_height`. `index_hash` and `node_hash` give the block hash at a
/// height in the index and in the node's best chain.
fn index_tip(
    tip_height: u64,
    node_height: u64,
    max_reorg_depth: u64,
    mut index_hash: impl FnMut(u64) -> Result<BlockHash>,
    mut node_hash: impl FnMut(u64) -> Result<BlockHash>,
) -> Result<IndexTip> {
    // The highest indexed block, at most `node_height`, that is also on the node's
    // best chain. More than `max_reorg_depth` blocks below the tip it can't be
    // reverted: older blocks are purged.
    let lowest = tip_height.saturating_sub(max_reorg_depth);
    let mut ancestor = tip_height.min(node_height);

    // Every chain shares the genesis block, which regtest doesn't index.
    while ancestor > 0 && index_hash(ancestor)? != node_hash(ancestor)? {
        if ancestor <= lowest {
            return Err(ReorgError::Unrecoverable.into());
        }
        ancestor -= 1;
    }

    Ok(if ancestor == tip_height {
        if node_height > tip_height {
            IndexTip::Behind
        } else {
            IndexTip::Synced
        }
    } else if ancestor == node_height {
        IndexTip::NodeBehind
    } else {
        IndexTip::Reorged {
            depth: tip_height - ancestor,
        }
    })
}

#[cfg(test)]
mod tests {
    use {super::*, crate::index::updater::testing::TempStore, bitcoin::hashes::Hash};

    fn hash(chain: u8, height: u64) -> BlockHash {
        let mut bytes = [chain; 32];
        bytes[..8].copy_from_slice(&height.to_le_bytes());
        BlockHash::from_byte_array(bytes)
    }

    /// The index has blocks 0..=`tip` of chain 0. The node's best chain has
    /// `node_height + 1` blocks and leaves chain 0 above `fork_height`.
    fn tip(tip: u64, node_height: u64, fork_height: u64) -> Result<IndexTip> {
        index_tip(
            tip,
            node_height,
            10,
            |height| {
                assert!(height <= tip, "index has no block {height}");
                Ok(hash(0, height))
            },
            |height| {
                assert!(height <= node_height, "node has no block {height}");
                Ok(hash(if height > fork_height { 1 } else { 0 }, height))
            },
        )
    }

    #[test]
    fn index_tip_on_node_chain() {
        assert_eq!(tip(100, 100, u64::MAX).unwrap(), IndexTip::Synced);
        assert_eq!(tip(100, 105, u64::MAX).unwrap(), IndexTip::Behind);
        assert_eq!(tip(100, 90, u64::MAX).unwrap(), IndexTip::NodeBehind);
    }

    #[test]
    fn reorg_depth_counts_blocks_above_common_ancestor() {
        // Same height, different tip: only the tip is reverted, block 99 is kept.
        assert_eq!(tip(100, 100, 99).unwrap(), IndexTip::Reorged { depth: 1 });
        // The node's chain is longer and forked below the tip.
        assert_eq!(tip(100, 103, 97).unwrap(), IndexTip::Reorged { depth: 3 });
        // The node is behind on another branch: revert down to the fork.
        assert_eq!(tip(100, 98, 95).unwrap(), IndexTip::Reorged { depth: 5 });
    }

    #[test]
    fn reorg_deeper_than_max_depth_is_unrecoverable() {
        // Exactly max depth: block 90 is still retained and the common ancestor.
        assert_eq!(tip(100, 100, 90).unwrap(), IndexTip::Reorged { depth: 10 });
        assert!(matches!(
            tip(100, 100, 89),
            Err(UpdaterError::BitcoinReorg(ReorgError::Unrecoverable))
        ));
    }

    #[test]
    fn genesis_is_always_a_common_ancestor() {
        assert_eq!(tip(3, 3, 0).unwrap(), IndexTip::Reorged { depth: 3 });
        assert_eq!(tip(0, 0, 0).unwrap(), IndexTip::Synced);
    }

    #[test]
    fn genesis_rune_is_inserted_once_into_a_fresh_index() {
        let store = TempStore::new();
        let db = store.db();

        assert!(insert_genesis_rune(db).unwrap());
        assert!(!insert_genesis_rune(db).unwrap());

        let id = RuneId::new(1, 0);
        let rune = Rune(2055900680524219742);
        assert_eq!(Store::get_runes_count(db).unwrap(), 1);
        assert_eq!(Store::get_rune_id(db, &rune).unwrap(), id);
        assert_eq!(db.get_rune_id_by_number(0).unwrap(), id);

        let entry = Store::get_rune(db, &id).unwrap();
        assert_eq!(entry.spaced_rune.to_string(), "UNCOMMON•GOODS");
        assert_eq!(entry.symbol, Some('⧉'));
        assert_eq!((entry.block, entry.number, entry.mints), (1, 0, 0));
        assert!(entry.turbo);
        let terms = entry.terms.unwrap();
        assert_eq!(terms.amount, Some(1));
        assert_eq!(terms.cap, Some(u128::MAX));
        assert_eq!(terms.height, (Some(840_000), Some(1_050_000)));
        assert_eq!(entry.mintable(840_000), Ok(1));
    }

    #[test]
    fn genesis_rune_is_not_inserted_after_indexing_started() {
        let store = TempStore::new();
        let mut update = BatchUpdate::new(0, 1, 0);
        update.block_hashes.insert(0, hash(0, 0));
        store.db().batch_update(&update, false).unwrap();

        assert!(!insert_genesis_rune(store.db()).unwrap());
        assert_eq!(Store::get_runes_count(store.db()).unwrap(), 0);
    }
}
