use {
    super::rollback_cache::RollbackCache,
    crate::{
        index::{store::Store, Settings, StoreError},
        models::{TransactionStateChange, TransactionStateChangeInput},
    },
    arch_titan_types_core::RuneId,
    arch_titan_types_core::{InscriptionId, SerializedOutPoint, SerializedTxid, SpentStatus},
    bitcoin::{BlockHash, ScriptBuf},
    rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet},
    std::sync::Arc,
    thiserror::Error,
    tracing::{info, warn},
};

#[derive(Debug, Error)]
pub enum RollbackError {
    #[error("store error {0}")]
    Store(#[from] StoreError),
    #[error("overflow in {0}")]
    Overflow(String),
}

type Result<T> = std::result::Result<T, RollbackError>;

pub struct RollbackSettings {
    pub index_addresses: bool,
}

impl From<Settings> for RollbackSettings {
    fn from(settings: Settings) -> Self {
        Self {
            index_addresses: settings.index_addresses,
        }
    }
}

pub struct Rollback<'a> {
    store: &'a Arc<dyn Store + Send + Sync>,
    settings: RollbackSettings,
    cache: RollbackCache<'a>,
    /// Inputs whose recorded spender was a reverted transaction.
    released_inputs: HashSet<SerializedOutPoint>,
}

impl<'a> Rollback<'a> {
    pub fn new(
        store: &'a Arc<dyn Store + Send + Sync>,
        settings: RollbackSettings,
        mempool: bool,
    ) -> Result<Self> {
        let cache = RollbackCache::new(store, mempool)?;
        Ok(Self {
            store,
            settings,
            cache,
            released_inputs: HashSet::default(),
        })
    }

    /// Revert the block at `height` and lower the block count to `height` in the
    /// same write, so the stored block count never covers a half-reverted block.
    pub fn revert_block(
        &mut self,
        height: u64,
        block_hash: BlockHash,
        txids: &[SerializedTxid],
    ) -> Result<()> {
        self.cache.delete_block(height, block_hash);
        self.revert_transactions(txids)
    }

    pub fn revert_transactions(&mut self, txids: &[SerializedTxid]) -> Result<()> {
        let txs_state_changes = self
            .store
            .get_txs_state_changes(txids, self.cache.mempool)?;

        self.precache_transactions(&txs_state_changes)?;

        for txid in txids.iter().rev() {
            info!("Reverting transaction {}", txid);
            let transaction = txs_state_changes.get(txid);
            // .ok_or(RollbackError::Store(StoreError::NotFound(format!(
            //     "transaction not found: {}",
            //     txid
            // ))))?;

            if let Some(transaction) = transaction {
                self.revert_transaction(txid, transaction)?;
            } else {
                warn!("Transaction to rollback not found: {}", txid);
                if self.cache.mempool {
                    self.cache.add_tx_to_delete(*txid);
                }
            }
        }

        if self.settings.index_addresses {
            self.revert_transactions_script_pubkeys_modifications(&txs_state_changes)?;
        }

        self.cache.flush()?;

        Ok(())
    }

    fn precache_transactions(
        &mut self,
        txs_state_changes: &HashMap<SerializedTxid, TransactionStateChange>,
    ) -> Result<()> {
        let mut tx_outs = Vec::new();
        let mut runes = HashSet::default();

        for (_, tx) in txs_state_changes.iter() {
            tx_outs.extend(tx.inputs.iter().map(|input| input.previous_outpoint));

            if let Some(mint) = &tx.minted {
                runes.insert(mint.id);
            }

            for (rune_id, _) in tx.burned.iter() {
                runes.insert(*rune_id);
            }

            if let Some((id, _)) = tx.etched {
                runes.insert(id);
            }
        }

        self.cache.precache_tx_outs(&tx_outs)?;
        self.cache
            .precache_runes(&runes.into_iter().collect::<Vec<_>>())?;

        Ok(())
    }

    fn revert_transaction(
        &mut self,
        txid: &SerializedTxid,
        transaction: &TransactionStateChange,
    ) -> Result<()> {
        // Make spendable the inputs again.
        for tx_in in transaction.inputs.iter() {
            self.release_input(txid, tx_in)?;
        }

        // Remove tx_outs
        for (vout, _tx_out) in transaction.outputs.iter().enumerate() {
            let outpoint = SerializedOutPoint::from_txid_vout(txid, vout as u32);
            self.cache.add_outpoint_to_delete(outpoint);
        }

        // Remove etched rune if any.
        // if a reorg has happened and a rune was etched but it doesn't exist now,
        // we need to remove all mints and transfer edicts from other transactions
        // that might have happened.
        if !self.cache.mempool {
            if let Some((id, rune)) = transaction.etched {
                let rune_entry = self.cache.get_rune(&id);
                if let Some(rune_entry) = rune_entry {
                    self.cache.add_rune_to_delete(id);
                    self.cache.add_rune_id_to_delete(rune);
                    self.cache.add_inscription_to_delete(InscriptionId {
                        txid: *txid,
                        index: 0,
                    });

                    // Decrease rune count
                    self.cache.decrement_runes_count();
                    self.cache.add_rune_number_to_delete(rune_entry.number);

                    // self.store.delete_rune_id_number(rune_entry.number)?;
                    // self.update_rune_numbers_after_revert(rune_entry.number, self.runes)?;

                    // Delete all rune transactions
                    self.cache.add_delete_all_rune_transactions(id);
                }
            }
        }

        // Remove mints if any.
        if let Some(mint) = transaction.minted.as_ref() {
            self.decrement_mint(&mint.id)?;
        }

        // Remove burned if any.
        for (rune_id, amount) in transaction.burned.iter() {
            self.update_burn_balance(rune_id, -(amount.n() as i128))?;
        }

        // Finally remove the transaction.
        self.cache.add_tx_to_delete(*txid);

        Ok(())
    }

    /// Mark the input's previous output unspent again, unless a different
    /// transaction is recorded as its spender: with RBF the replacement may already
    /// be indexed, and its spend must stay.
    fn release_input(
        &mut self,
        txid: &SerializedTxid,
        outpoint: &TransactionStateChangeInput,
    ) -> Result<()> {
        match self.cache.get_tx_out(&outpoint.previous_outpoint) {
            Ok(mut tx_out) => {
                if matches!(&tx_out.spent, SpentStatus::Spent(spender) if spender.txid != *txid) {
                    return Ok(());
                }

                tx_out.spent = SpentStatus::Unspent;
                self.cache.set_tx_out(outpoint.previous_outpoint, tx_out);
            }
            Err(StoreError::NotFound(_)) => {}
            Err(e) => return Err(RollbackError::Store(e)),
        }

        self.released_inputs.insert(outpoint.previous_outpoint);
        Ok(())
    }

    fn decrement_mint(&mut self, rune_id: &RuneId) -> Result<()> {
        let rune_entry = self.cache.get_rune(rune_id);

        if let Some(mut rune_entry) = rune_entry {
            if self.cache.mempool {
                let result = rune_entry.pending_mints.saturating_add_signed(-1);
                rune_entry.pending_mints = result;
            } else {
                let result = rune_entry.mints.saturating_add_signed(-1);

                rune_entry.mints = result;
            }

            self.cache.set_rune(*rune_id, rune_entry);
        }

        Ok(())
    }

    fn update_burn_balance(&mut self, rune_id: &RuneId, amount: i128) -> Result<()> {
        let rune_entry = self.cache.get_rune(rune_id);

        if let Some(mut rune_entry) = rune_entry {
            if self.cache.mempool {
                let result = rune_entry
                    .pending_burns
                    .checked_add_signed(amount)
                    .ok_or(RollbackError::Overflow("burn".to_string()))?;
                rune_entry.pending_burns = result;
            } else {
                let result = rune_entry
                    .burned
                    .checked_add_signed(amount)
                    .ok_or(RollbackError::Overflow("burn".to_string()))?;

                rune_entry.burned = result;
            }

            self.cache.set_rune(*rune_id, rune_entry);
        }

        Ok(())
    }

    fn revert_transactions_script_pubkeys_modifications(
        &mut self,
        tx_to_state_changes: &HashMap<SerializedTxid, TransactionStateChange>,
    ) -> Result<()> {
        // Update script pubkey entries.
        let mut script_pubkey_entries: HashMap<
            ScriptBuf,
            (Vec<SerializedOutPoint>, Vec<SerializedOutPoint>),
        > = HashMap::default();

        // Delete outpoints.
        let mut spent_outpoints = HashSet::default();
        for (txid, tx) in tx_to_state_changes.iter() {
            for (vout, output) in tx.outputs.iter().enumerate() {
                let script_pubkey = output.script_pubkey.clone();
                let entry = script_pubkey_entries.entry(script_pubkey).or_default();

                let outpoint = SerializedOutPoint::from_txid_vout(txid, vout as u32);
                // Add "spent" outpoint.
                entry.1.push(outpoint);
                spent_outpoints.insert(outpoint);
            }

            if !self.cache.mempool {
                for input in tx.inputs.iter() {
                    if let Some(script_pubkey) = input.script_pubkey.clone() {
                        let entry = script_pubkey_entries.entry(script_pubkey).or_default();

                        if !spent_outpoints.contains(&input.previous_outpoint) {
                            entry.0.push(input.previous_outpoint);
                        }
                    }
                }
            }
        }

        if self.cache.mempool {
            // Remove the spent-in-mempool markers of the inputs the reverted
            // transactions were still the spenders of.
            let prev_outpoints = tx_to_state_changes
                .values()
                .filter(|tx| !tx.is_coinbase)
                .flat_map(|tx| tx.inputs.iter().map(|input| input.previous_outpoint))
                .filter(|outpoint| self.released_inputs.contains(outpoint))
                .collect::<Vec<_>>();

            self.cache.add_prev_outpoint_to_delete(&prev_outpoints);
        }

        self.cache.set_script_pubkey_entries(script_pubkey_entries);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{index::updater::testing::TempStore, models::BatchUpdate},
        arch_titan_types_core::{SpenderReference, TxOut},
    };

    fn txid(n: u8) -> SerializedTxid {
        SerializedTxid::from([n; 32])
    }

    fn tx_out(spent: SpentStatus) -> TxOut {
        TxOut {
            runes: vec![],
            risky_runes: vec![],
            value: 1_000,
            spent,
            script_pubkey: ScriptBuf::new(),
        }
    }

    fn spending(outpoint: SerializedOutPoint) -> TransactionStateChange {
        TransactionStateChange {
            inputs: vec![TransactionStateChangeInput {
                previous_outpoint: outpoint,
                script_pubkey: None,
            }],
            outputs: vec![],
            etched: None,
            minted: None,
            burned: HashMap::default(),
            is_coinbase: false,
        }
    }

    #[test]
    fn mempool_revert_keeps_the_spend_of_an_rbf_replacement() {
        let store = TempStore::new();
        let db = store.shared();
        let (replaced, replacement) = (txid(1), txid(2));
        let outpoint = SerializedOutPoint::from_txid_vout(&txid(9), 0);
        let spent_by_replacement = SpenderReference {
            txid: replacement,
            vin: 0,
        };

        // A confirmed output, spent in the mempool first by `replaced`, then by
        // `replacement`, which is already indexed.
        let mut confirmed = BatchUpdate::new(0, 0, 0);
        confirmed
            .txouts
            .insert(outpoint, tx_out(SpentStatus::Unspent));
        db.batch_update(&confirmed, false).unwrap();

        let mut mempool = BatchUpdate::new(0, 0, 0);
        mempool
            .tx_state_changes
            .insert(replaced, spending(outpoint));
        mempool
            .tx_state_changes
            .insert(replacement, spending(outpoint));
        mempool.txouts.insert(
            outpoint,
            tx_out(SpentStatus::Spent(spent_by_replacement.clone())),
        );
        mempool
            .spent_outpoints_in_mempool
            .insert(outpoint, spent_by_replacement.clone());
        db.batch_update(&mempool, true).unwrap();

        let revert = |txid: SerializedTxid| {
            Rollback::new(
                &db,
                RollbackSettings {
                    index_addresses: true,
                },
                true,
            )
            .unwrap()
            .revert_transactions(&[txid])
            .unwrap();
        };

        revert(replaced);
        assert_eq!(
            db.get_tx_out_with_mempool_spent_update(&outpoint, None)
                .unwrap()
                .spent,
            SpentStatus::Spent(spent_by_replacement)
        );

        revert(replacement);
        assert_eq!(
            db.get_tx_out_with_mempool_spent_update(&outpoint, None)
                .unwrap()
                .spent,
            SpentStatus::Unspent
        );
    }
}
