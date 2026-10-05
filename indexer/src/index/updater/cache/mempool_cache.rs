use {
    crate::{
        index::{
            store::{Store, StoreError},
            updater::{events::Events, store_lock::StoreWithLock, transaction::TransactionStore},
            Chain, Settings,
        },
        models::{
            BatchUpdate, BlockId, Inscription, RuneEntry, TransactionStateChange,
            TransactionStateChangeInput,
        },
    },
    arch_titan_types_core::Rune,
    arch_titan_types_core::RuneId,
    arch_titan_types_core::{
        Event, InscriptionId, Location, MempoolEntry, SerializedOutPoint, SerializedTxid,
        SpenderReference, SpentStatus, TxOut,
    },
    bitcoin::{consensus, ScriptBuf, Transaction},
    rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet},
    std::{sync::Arc, time::Instant},
    tracing::debug,
};

type Result<T> = std::result::Result<T, StoreError>;

pub struct MempoolCacheSettings {
    pub chain: Chain,
    pub index_addresses: bool,
}

impl MempoolCacheSettings {
    pub fn new(settings: &Settings) -> Self {
        Self {
            chain: settings.chain,
            index_addresses: settings.index_addresses,
        }
    }
}

pub struct MempoolCache {
    db: Arc<StoreWithLock>,
    update: BatchUpdate,
    /// Each rune entry as first read from the store, to compute this cache's own
    /// change to the mempool-owned fields at flush.
    rune_bases: HashMap<RuneId, RuneEntry>,
    pub settings: MempoolCacheSettings,
}

impl MempoolCache {
    pub fn new(db: Arc<StoreWithLock>, settings: MempoolCacheSettings) -> Result<Self> {
        let (rune_count, mut block_count, purged_blocks_count) = {
            let db = db.read();
            (
                db.get_runes_count()?,
                db.get_block_count()?,
                db.get_purged_blocks_count()?,
            )
        };

        // In regtest, the first block is not accessible via RPC and for testing purposes we
        // don't need to start at block 0.
        if block_count == 0 && settings.chain == Chain::Regtest {
            block_count = 1;
        }

        Ok(Self {
            db,
            update: BatchUpdate::new(rune_count, block_count, purged_blocks_count),
            rune_bases: HashMap::default(),
            settings,
        })
    }

    pub fn get_block_count(&self) -> u64 {
        self.update.block_count
    }

    pub fn does_tx_exist(&self, txid: SerializedTxid) -> Result<bool> {
        if self.update.tx_state_changes.contains_key(&txid) {
            return Ok(true);
        }

        self.db.read().is_tx_in_mempool(&txid)
    }

    pub fn set_mempool_tx(&mut self, txid: SerializedTxid, mempool_entry: MempoolEntry) {
        self.update.mempool_txs.insert(txid, mempool_entry);
    }

    fn get_tx_out(&self, outpoint: &SerializedOutPoint) -> Result<TxOut> {
        if let Some(tx_out) = self.update.txouts.get(outpoint) {
            return Ok(tx_out.clone());
        }

        let tx_out = self.db.read().get_tx_out(outpoint, None)?;
        Ok(tx_out)
    }

    fn update_script_pubkeys(&mut self) -> Result<()> {
        let mut spk_map: HashMap<ScriptBuf, (Vec<SerializedOutPoint>, Vec<SerializedOutPoint>)> =
            HashMap::default();

        for (outpoint, script_pubkey) in &self.update.script_pubkeys_outpoints {
            let entry: &mut (Vec<SerializedOutPoint>, Vec<SerializedOutPoint>) =
                spk_map.entry(script_pubkey.clone()).or_default();

            entry.0.push(*outpoint); // new
        }

        self.update.script_pubkeys = spk_map;

        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        // Hold the write lock from the rune re-read to the write (see `StoreWithLock`).
        let store = self.db.clone();
        let db = store.write();

        if self.settings.index_addresses {
            self.update_script_pubkeys()?;
        }

        self.rebase_rune_entries(&**db)?;

        if !self.update.is_empty() {
            let start = Instant::now();
            db.batch_update(&self.update, true)?;
            debug!("Flushed update: {} in {:?}", self.update, start.elapsed());
        }

        Ok(())
    }

    /// Mempool indexing owns only `pending_mints`/`pending_burns`. Re-read every
    /// rune entry this cache changed and apply only its own change to those fields,
    /// so `mints`/`burned` written by block indexing (and pending counts written by
    /// other mempool writers) since the entry was read are kept.
    fn rebase_rune_entries(&mut self, db: &dyn Store) -> Result<()> {
        let rune_ids: Vec<RuneId> = self
            .update
            .runes
            .keys()
            .filter(|rune_id| self.rune_bases.contains_key(rune_id))
            .copied()
            .collect();

        if rune_ids.is_empty() {
            return Ok(());
        }

        let mut stored = db.get_runes_by_ids(&rune_ids)?;

        for rune_id in rune_ids {
            let entry = self.update.runes.remove(&rune_id).expect("rune in update");

            // Gone from the store: a reorg reverted the etching. Don't resurrect it.
            let Some(mut current) = stored.remove(&rune_id) else {
                self.rune_bases.remove(&rune_id);
                continue;
            };

            let base = &self.rune_bases[&rune_id];
            current.pending_mints = apply_change(
                current.pending_mints,
                base.pending_mints,
                entry.pending_mints,
            );
            current.pending_burns = apply_change(
                current.pending_burns,
                base.pending_burns,
                entry.pending_burns,
            );

            // Writing `current` is now this cache's state: a second flush adds nothing.
            self.rune_bases.insert(rune_id, current.clone());
            self.update.runes.insert(rune_id, current);
        }

        Ok(())
    }

    /// Emits one `AddressModified` per script touched in the current update. Reads
    /// `script_pubkeys_outpoints`, which is filled while indexing; `script_pubkeys` is only
    /// built at flush time, after the events have been collected.
    pub fn add_address_events(&mut self, events: &mut Events, chain: Chain) {
        let scripts: HashSet<&ScriptBuf> = self.update.script_pubkeys_outpoints.values().collect();
        for script_pubkey in scripts {
            if let Ok(address) = chain.address_from_script(script_pubkey) {
                events.add_event(Event::AddressModified {
                    address: address.to_string(),
                    location: Location::mempool(),
                });
            }
        }
    }
}

impl TransactionStore for MempoolCache {
    fn get_tx_outs(
        &mut self,
        outpoints: &[SerializedOutPoint],
    ) -> Result<HashMap<SerializedOutPoint, TxOut>> {
        let mut results = HashMap::default();
        let mut to_fetch = HashSet::default();

        for outpoint in outpoints {
            if let Some(tx_out) = self.update.txouts.get(outpoint) {
                results.insert(*outpoint, tx_out.clone());
                continue;
            }

            to_fetch.insert(*outpoint);
        }

        if !to_fetch.is_empty() {
            let tx_outs = self
                .db
                .read()
                .get_tx_outs(&to_fetch.iter().cloned().collect::<Vec<_>>(), None)?;

            results.extend(tx_outs);
        }

        Ok(results)
    }

    fn set_tx_out(
        &mut self,
        outpoint: SerializedOutPoint,
        tx_out: TxOut,
        script_pubkey: ScriptBuf,
    ) {
        self.update.txouts.insert(outpoint, tx_out);

        if self.settings.index_addresses {
            self.update
                .script_pubkeys_outpoints
                .insert(outpoint, script_pubkey);
        }
    }

    fn set_spent_tx_out(
        &mut self,
        outpoint: &TransactionStateChangeInput,
        spent: SpenderReference,
    ) -> Result<()> {
        match self.get_tx_out(&outpoint.previous_outpoint) {
            Ok(tx_out) => {
                let mut tx_out = tx_out;
                tx_out.spent = SpentStatus::Spent(spent.clone());
                self.update
                    .txouts
                    .insert(outpoint.previous_outpoint, tx_out);
            }
            Err(e) => return Err(e),
        }

        if self.settings.index_addresses {
            self.update
                .spent_outpoints_in_mempool
                .insert(outpoint.previous_outpoint, spent);
        }

        Ok(())
    }

    fn get_transaction(
        &self,
        txid: &SerializedTxid,
    ) -> std::result::Result<Transaction, StoreError> {
        // 1. Check current in-memory update.
        if let Some(transaction) = self.update.transactions.get(txid) {
            return Ok(transaction.clone());
        }

        // 4. Fallback to DB.
        let transaction_raw = self.db.read().get_transaction_raw(txid, None)?;
        let tx: Transaction = consensus::deserialize(&transaction_raw)?;
        Ok(tx)
    }

    fn set_transaction(&mut self, txid: SerializedTxid, transaction: Transaction) {
        self.update.transactions.insert(txid, transaction);
    }

    fn get_transaction_confirming_block(
        &self,
        txid: &SerializedTxid,
    ) -> std::result::Result<BlockId, StoreError> {
        // 1. Check current in-memory update.
        if let Some(block_id) = self.update.transaction_confirming_block.get(txid) {
            return Ok(*block_id);
        }

        // 2. Fallback to DB.
        let block_id = self.db.read().get_transaction_confirming_block(txid)?;
        Ok(block_id)
    }

    fn set_transaction_confirming_block(&mut self, _txid: SerializedTxid, _block_id: BlockId) {
        panic!("set_transaction_confirming_block shouldn't be called in mempool cache");
    }

    fn get_rune(&mut self, rune_id: &RuneId) -> std::result::Result<RuneEntry, StoreError> {
        // 1. Current update.
        if let Some(rune) = self.update.runes.get(rune_id) {
            return Ok(rune.clone());
        }

        // 2. DB.
        let rune = self.db.read().get_rune(rune_id)?;
        self.rune_bases.insert(*rune_id, rune.clone());
        Ok(rune)
    }

    fn does_rune_exist(&mut self, rune: &Rune) -> std::result::Result<(), StoreError> {
        // 1. Current update.
        if self.update.rune_ids.contains_key(&rune.0) {
            return Ok(());
        }

        // 2. DB.
        let _ = self.db.read().get_rune_id(rune)?;
        Ok(())
    }

    fn get_runes_count(&self) -> u64 {
        self.update.rune_count
    }

    fn set_rune_id(&mut self, rune: Rune, id: RuneId) {
        self.update.rune_ids.insert(rune.0, id);
    }

    fn set_rune(&mut self, id: RuneId, entry: RuneEntry) {
        self.update.runes.insert(id, entry);
    }

    fn set_rune_id_number(&mut self, number: u64, id: RuneId) {
        self.update.rune_numbers.insert(number, id);
    }

    fn increment_runes_count(&mut self) {
        self.update.rune_count += 1;
    }

    fn set_inscription(&mut self, id: InscriptionId, inscription: Inscription) {
        self.update.inscriptions.insert(id, inscription);
    }

    fn set_tx_state_changes(
        &mut self,
        txid: SerializedTxid,
        tx_state_changes: TransactionStateChange,
    ) {
        self.update.tx_state_changes.insert(txid, tx_state_changes);
    }

    fn add_rune_transaction(&mut self, rune_id: RuneId, txid: SerializedTxid) {
        self.update
            .rune_transactions
            .entry(rune_id)
            .or_insert(vec![])
            .push(txid);
    }
}

/// `current` moved by the difference between `new` and `base`.
fn apply_change(current: u128, base: u128, new: u128) -> u128 {
    if new >= base {
        current.saturating_add(new - base)
    } else {
        current.saturating_sub(base - new)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::index::updater::testing::{rune_entry, TempStore},
    };

    #[test]
    fn flush_keeps_mints_and_pending_mints_stored_since_rune_was_read() {
        let store = TempStore::new();
        let id = RuneId::new(840_000, 1);
        store.put_rune(id, &rune_entry(0, 0));

        let settings = MempoolCacheSettings {
            chain: Chain::Regtest,
            index_addresses: false,
        };
        let mut cache = MempoolCache::new(store.locked(), settings).unwrap();

        let mut entry = cache.get_rune(&id).unwrap();
        entry.pending_mints += 1;
        cache.set_rune(id, entry);

        // Meanwhile a block mints twice and another mempool writer adds a pending mint.
        store.put_rune(id, &rune_entry(2, 1));

        cache.flush().unwrap();
        let stored = store.db().get_rune(&id).unwrap();
        assert_eq!((stored.mints, stored.pending_mints), (2, 2));

        // Flushing again adds nothing.
        cache.flush().unwrap();
        let stored = store.db().get_rune(&id).unwrap();
        assert_eq!((stored.mints, stored.pending_mints), (2, 2));
    }
}
