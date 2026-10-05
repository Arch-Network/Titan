use {
    super::{
        metrics::Metrics,
        settings::Settings,
        store::{InputsOutputs, Store, StoreError},
        updater::Updater,
        zmq::ZmqManager,
    },
    crate::{
        bitcoin_rpc::{RpcClientError, RpcClientPool},
        index::updater::{ReorgError, UpdaterError},
        models::{block_id_to_transaction_status, Inscription, RuneEntry},
    },
    arch_titan_types_api::{AddressData, AddressTxOut, Pagination, PaginationResponse},
    arch_titan_types_core::{
        Block, Event, InscriptionId, MempoolEntry, Rune, RuneAmount, RuneId, SerializedOutPoint,
        SerializedTxid, Transaction, TransactionStatus, TxOut,
    },
    bitcoin::{Address, BlockHash, Transaction as BitcoinTransaction},
    bitcoincore_rpc::Client,
    rustc_hash::FxHashMap as HashMap,
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread::{self},
        time::Duration,
    },
    tokio::{runtime::Runtime, sync::mpsc::Sender},
    tracing::{error, info, warn},
};

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("store error: {0}")]
    StoreError(#[from] StoreError),
    #[error("invalid index: {0}")]
    InvalidIndex(String),
    #[error("invalid best block hash: {0}")]
    InvalidBestBlockHash(String),
    #[error("rpc client error: {0}")]
    RpcClientError(#[from] RpcClientError),
    #[error("rpc api error: {0}")]
    RpcApiError(#[from] bitcoincore_rpc::Error),
    #[error("updater error: {0}")]
    UpdaterError(#[from] UpdaterError),
}

type Result<T> = std::result::Result<T, IndexError>;

pub struct Index {
    db: Arc<dyn Store + Send + Sync>,
    settings: Settings,
    updater: Arc<Updater>,

    shutdown_flag: Arc<AtomicBool>,

    zmq_manager: Arc<ZmqManager>,
}

impl Index {
    pub fn new(
        db: Arc<dyn Store + Send + Sync>,
        bitcoin_rpc_pool: RpcClientPool,
        settings: Settings,
        sender: Option<Sender<Event>>,
    ) -> Self {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let metrics = Metrics::new();
        metrics.start(shutdown_flag.clone());

        let zmq_manager = ZmqManager::new(settings.zmq_endpoint.clone());

        Self {
            db: db.clone(),
            settings: settings.clone(),
            updater: Arc::new(Updater::new(
                db.clone(),
                bitcoin_rpc_pool,
                settings.clone(),
                &metrics,
                shutdown_flag.clone(),
                sender,
            )),
            shutdown_flag,
            zmq_manager: Arc::new(zmq_manager),
        }
    }

    /// Check the index settings against the database, recording them (and seeding
    /// the mainnet genesis rune) in a fresh database.
    pub fn validate_index(&self) -> Result<()> {
        let db_index_addresses = self.db.is_index_addresses()?;
        match (self.settings.index_addresses, db_index_addresses) {
            (true, Some(false)) => {
                return Err(IndexError::InvalidIndex(
                    "index_addresses is not set. Disable index_addresses in settings or clean up the database".to_string(),
                ));
            }
            (true, None) => {
                self.db.set_index_addresses(true)?;
            }
            (false, Some(true)) | (false, None) => {
                self.db.set_index_addresses(false)?;
            }
            _ => {}
        }

        let db_index_bitcoin_transactions = self.db.is_index_bitcoin_transactions()?;
        match (
            self.settings.index_bitcoin_transactions,
            db_index_bitcoin_transactions,
        ) {
            (true, Some(false)) => {
                return Err(IndexError::InvalidIndex(
                    "index_bitcoin_transactions is not set. Disable index_bitcoin_transactions in settings or clean up the database".to_string(),
                ));
            }
            (true, None) => {
                self.db.set_index_bitcoin_transactions(true)?;
            }
            (false, Some(true)) | (false, None) => {
                self.db.set_index_bitcoin_transactions(false)?;
            }
            _ => {}
        }

        let db_index_spent_outputs = self.db.is_index_spent_outputs()?;
        match (self.settings.index_spent_outputs, db_index_spent_outputs) {
            (true, Some(false)) => {
                return Err(IndexError::InvalidIndex("index_spent_outputs is not set. Disable index_spent_outputs in settings or clean up the database".to_string()));
            }
            (true, None) => {
                self.db.set_index_spent_outputs(true)?;
            }
            (false, Some(true)) | (false, None) => {
                self.db.set_index_spent_outputs(false)?;
            }
            _ => {}
        }

        self.updater.insert_genesis_rune()?;

        Ok(())
    }

    pub fn shutdown(&self) {
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.zmq_manager.shutdown();
    }

    pub fn index(&self) {
        loop {
            if self.shutdown_flag.load(Ordering::SeqCst) {
                info!("Indexer received shutdown signal, stopping...");
                break;
            }

            match self.updater.update_to_tip() {
                Ok(()) => (),
                Err(UpdaterError::BitcoinReorg(ReorgError::Unrecoverable)) => {
                    error!("Unrecoverable reorg detected. stopping indexer loop.");
                    break;
                }
                Err(UpdaterError::BitcoinReorg(ReorgError::Recoverable {
                    height: _,
                    depth: _,
                })) => {
                    continue;
                }
                Err(UpdaterError::BitcoinRpc(e)) => {
                    warn!(
                        "We're getting network connection issues, retrying... {}",
                        e.to_string()
                    );
                    self.sleep_main_loop_interval();
                    continue;
                }
                Err(e) => {
                    error!("Failed to update to tip: {}", e);
                    // Signal shutdown so that background workers (e.g. block fetcher, bg_writer,
                    // ZMQ listener) can terminate gracefully instead of lingering and giving the
                    // impression that the indexer is still running.
                    self.shutdown();
                    break;
                }
            }

            if self.shutdown_flag.load(Ordering::SeqCst) {
                info!("Indexer received shutdown signal, stopping...");
                break;
            }

            match self.updater.index_mempool() {
                Ok(_) => (),
                Err(UpdaterError::BitcoinRpc(e)) => {
                    warn!(
                        "We're getting network connection issues, retrying... {}",
                        e.to_string()
                    );
                    self.sleep_main_loop_interval();
                    continue;
                }
                Err(e) => {
                    error!("Failed to index mempool: {}", e);
                    // Ensure we also trigger a graceful shutdown here.
                    self.shutdown();
                    break;
                }
            }

            match self.updater.notify_tx_updates(false) {
                Ok(_) => (),
                Err(UpdaterError::InvalidMainChainTip) => {
                    warn!("Invalid main chain tip, when sending tx updates...");
                    self.sleep_main_loop_interval();
                    continue;
                }
                Err(e) => {
                    error!("Failed to notify tx updates: {}", e);
                }
            }

            self.sleep_main_loop_interval();
        }

        let rt = Runtime::new().expect("Failed to create runtime");
        rt.block_on(self.zmq_manager.join_zmq_listener());
        info!("Closing indexer");
    }

    /// Waits one main-loop interval. Retry paths use it too, so a persistent failure
    /// cannot turn the loop into a busy spin.
    fn sleep_main_loop_interval(&self) {
        thread::sleep(Duration::from_millis(self.settings.main_loop_interval));
    }

    pub async fn start_zmq_listener(&self) {
        self.zmq_manager
            .start_zmq_listener(self.updater.clone())
            .await;
    }

    pub fn get_block_count(&self) -> Result<u64> {
        Ok(self.db.get_block_count()?)
    }

    pub fn get_block_hash(&self, height: u64) -> Result<BlockHash> {
        Ok(self.db.get_block_hash(height)?)
    }

    pub fn get_is_at_tip(&self) -> Result<bool> {
        Ok(self.db.get_is_at_tip()?)
    }

    pub fn get_block_by_hash(&self, hash: &BlockHash) -> Result<Block> {
        Ok(self.db.get_block_by_hash(hash)?)
    }

    pub fn get_mempool_txids(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<SerializedTxid>> {
        Ok(self.db.get_mempool_txids_page(pagination)?)
    }

    pub fn get_mempool_entry(&self, txid: &SerializedTxid) -> Result<MempoolEntry> {
        Ok(self.db.get_mempool_entry(txid)?)
    }

    pub fn get_mempool_entries(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Option<MempoolEntry>>> {
        Ok(self.db.get_mempool_entries(txids)?)
    }

    pub fn get_mempool_entries_page(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<(SerializedTxid, MempoolEntry)>> {
        Ok(self.db.get_mempool_entries_page(pagination)?)
    }

    pub fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, MempoolEntry>> {
        Ok(self.db.get_mempool_entries_with_ancestors(txids)?)
    }

    pub fn get_tx_out(&self, outpoint: &SerializedOutPoint) -> Result<TxOut> {
        Ok(self
            .db
            .get_tx_out_with_mempool_spent_update(outpoint, None)?)
    }

    pub fn get_rune(&self, rune_id: &RuneId) -> Result<RuneEntry> {
        Ok(self.db.get_rune(rune_id)?)
    }

    pub fn get_runes(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<(RuneId, RuneEntry)>> {
        Ok(self.db.get_runes(pagination)?)
    }

    pub fn get_rune_id(&self, rune: &Rune) -> Result<RuneId> {
        Ok(self.db.get_rune_id(rune)?)
    }

    pub fn get_runes_count(&self) -> Result<u64> {
        Ok(self.db.get_runes_count()?)
    }

    pub fn get_inscription(&self, inscription_id: &InscriptionId) -> Result<Inscription> {
        Ok(self.db.get_inscription(inscription_id)?)
    }

    pub fn get_last_rune_transactions(
        &self,
        rune_id: &RuneId,
        pagination: Option<Pagination>,
        mempool: Option<bool>,
    ) -> Result<PaginationResponse<SerializedTxid>> {
        Ok(self
            .db
            .get_last_rune_transactions(rune_id, pagination, mempool)?)
    }

    /// Balance summary over all of the address's outputs (confirmed plus mempool-created,
    /// minus those spent in the mempool). Reads every output's `TxOut` but no transactions.
    pub fn get_address_data(&self, address: &Address) -> Result<AddressData> {
        let script_pubkey = address.script_pubkey();
        let outpoints = self.db.get_script_pubkey_outpoints(&script_pubkey, None)?;
        let tx_outs = self
            .db
            .get_tx_outs_with_mempool_spent_update(&outpoints, None)?;

        let mut runes = HashMap::default();
        let mut value = 0;
        for tx_out in tx_outs.values() {
            for rune in tx_out.runes.iter().chain(tx_out.risky_runes.iter()) {
                runes
                    .entry(rune.id)
                    .and_modify(|amount| *amount += rune.amount)
                    .or_insert(rune.amount);
            }
            value += tx_out.value;
        }

        Ok(AddressData {
            value,
            runes: runes
                .into_iter()
                .map(|(rune_id, amount)| RuneAmount::from((rune_id, amount)))
                .collect(),
            output_count: tx_outs.len() as u64,
        })
    }

    /// One page of the outputs summarized by `get_address_data`: confirmed outputs in
    /// outpoint order, then mempool-created ones.
    pub fn get_address_outputs(
        &self,
        address: &Address,
        pagination: Pagination,
    ) -> Result<PaginationResponse<AddressTxOut>> {
        let script_pubkey = address.script_pubkey();
        let page = self
            .db
            .get_script_pubkey_outpoints_page(&script_pubkey, pagination)?;
        let mut tx_outs = self
            .db
            .get_tx_outs_with_mempool_spent_update(&page.items, None)?;

        let txids: Vec<SerializedTxid> = page
            .items
            .iter()
            .map(|outpoint| outpoint.to_serialized_txid())
            .collect();
        let confirming_blocks = self.db.get_transaction_confirming_blocks(&txids)?;

        let mut tx_sizes: HashMap<SerializedTxid, (u64, u64)> = HashMap::default();
        let mut outputs = Vec::with_capacity(page.items.len());
        for outpoint in &page.items {
            let Some(tx_out) = tx_outs.remove(outpoint) else {
                continue;
            };

            // Size and weight only need the raw transaction; loading the full API
            // transaction would also fetch every input's and output's TxOut.
            let txid = outpoint.to_serialized_txid();
            let (size, weight) = match tx_sizes.get(&txid) {
                Some(sizes) => *sizes,
                None => {
                    let raw = self.db.get_transaction_raw(&txid, None)?;
                    let tx: bitcoin::Transaction =
                        bitcoin::consensus::deserialize(&raw).map_err(|e| {
                            IndexError::InvalidIndex(format!("corrupt transaction {txid}: {e}"))
                        })?;
                    let sizes = (raw.len() as u64, tx.weight().to_wu());
                    tx_sizes.insert(txid, sizes);
                    sizes
                }
            };

            let status = block_id_to_transaction_status(
                confirming_blocks.get(&txid).and_then(|x| x.as_ref()),
            );
            outputs.push(AddressTxOut {
                txid: outpoint.to_txid(),
                vout: outpoint.vout(),
                value: tx_out.value,
                runes: tx_out.runes,
                risky_runes: tx_out.risky_runes,
                spent: tx_out.spent,
                status,
                size,
                weight,
            });
        }

        Ok(PaginationResponse {
            items: outputs,
            offset: page.offset,
        })
    }

    pub fn is_indexing_bitcoin_transactions(&self) -> bool {
        self.settings.index_bitcoin_transactions
    }

    pub fn get_transaction_raw(&self, txid: &SerializedTxid) -> Result<Vec<u8>> {
        Ok(self.db.get_transaction_raw(txid, None)?)
    }

    pub fn get_transaction(&self, txid: &SerializedTxid) -> Result<Transaction> {
        Ok(self.db.get_transaction(txid, None)?)
    }

    pub fn get_inputs_outputs_from_transaction(
        &self,
        transaction: &BitcoinTransaction,
        txid: &SerializedTxid,
    ) -> Result<InputsOutputs> {
        Ok(self
            .db
            .get_inputs_outputs_from_transaction(transaction, txid)?)
    }

    pub fn get_transaction_status(&self, txid: &SerializedTxid) -> Result<TransactionStatus> {
        let result = self.db.get_transaction_confirming_block(txid);
        match result {
            Ok(block_id) => Ok(block_id.into_transaction_status()),
            Err(StoreError::NotFound(_)) => {
                // If the transaction is not found, we will return a not found error.
                self.db.get_transaction_raw(txid, None)?;

                // If it's found, then it's unconfirmed.
                Ok(TransactionStatus::unconfirmed())
            }
            Err(e) => Err(IndexError::StoreError(e)),
        }
    }

    pub fn get_transactions_statuses(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Option<TransactionStatus>>> {
        let (exist, _) = self.db.partition_transactions_by_existence(txids)?;
        let result = self.db.get_transaction_confirming_blocks(&exist)?;

        Ok(txids
            .iter()
            .map(|txid| {
                let block_id = result.get(txid);
                match block_id {
                    Some(Some(block_id)) => (*txid, Some(block_id.into_transaction_status())),
                    Some(None) => (*txid, Some(TransactionStatus::unconfirmed())),
                    None => (*txid, None),
                }
            })
            .collect())
    }

    pub fn pre_index_new_submitted_transaction(&self, txid: &SerializedTxid) -> Result<()> {
        Ok(self.updater.pre_index_new_submitted_transaction(txid)?)
    }

    pub fn remove_pre_index_new_submitted_transaction(&self, txid: &SerializedTxid) -> Result<()> {
        Ok(self
            .updater
            .remove_pre_index_new_submitted_transaction(txid)?)
    }

    /// Index a transaction this node just broadcast. Uses the caller's `rpc_client`:
    /// taking another one from the pool while waiting for the updater's broadcast
    /// lock could exhaust the pool and deadlock concurrent broadcasts.
    pub fn index_new_submitted_transaction(
        &self,
        rpc_client: &Client,
        txid: &SerializedTxid,
        tx: &BitcoinTransaction,
        mempool_entry: MempoolEntry,
    ) {
        match self
            .updater
            .index_new_submitted_tx(rpc_client, txid, tx, mempool_entry)
        {
            Ok(_) => (),
            Err(e) => {
                error!("Failed to index new transaction after broadcast: {}", e);
            }
        }
    }

    pub fn get_mempool_count(&self) -> Result<usize> {
        Ok(self.db.get_mempool_count()?)
    }
}
