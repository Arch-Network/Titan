use {
    crate::{
        db::{RocksDB, RocksDBError},
        models::{
            BatchDelete, BatchRollback, BatchUpdate, BlockId, Inscription, RuneEntry,
            TransactionStateChange,
        },
    },
    arch_titan_types_api::{Pagination, PaginationResponse},
    arch_titan_types_core::RuneId,
    arch_titan_types_core::{
        Block, InscriptionId, MempoolEntry, Rune, SerializedOutPoint, SerializedTxid,
        SpenderReference, SpentStatus, Transaction, TransactionStatus, TxOut,
    },
    bitcoin::{consensus, hex::HexToArrayError, BlockHash, ScriptBuf},
    rustc_hash::FxHashMap as HashMap,
    thiserror::Error,
};

/// The previous outputs spent by a transaction's inputs and its own outputs, in order.
pub type InputsOutputs = (Vec<Option<TxOut>>, Vec<Option<TxOut>>);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("db error {0}")]
    DB(RocksDBError),
    #[error("hex array error {0}")]
    HexToArray(#[from] HexToArrayError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("deserialize error {0}")]
    Deserialize(#[from] consensus::encode::Error),
}

impl StoreError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, StoreError::NotFound(_))
    }
}

impl From<RocksDBError> for StoreError {
    fn from(error: RocksDBError) -> Self {
        match error {
            RocksDBError::NotFound(msg) => StoreError::NotFound(msg),
            other => StoreError::DB(other),
        }
    }
}

pub trait Store {
    // settings
    fn is_index_addresses(&self) -> Result<Option<bool>, StoreError>;
    fn set_index_addresses(&self, value: bool) -> Result<(), StoreError>;
    fn is_index_bitcoin_transactions(&self) -> Result<Option<bool>, StoreError>;
    fn set_index_bitcoin_transactions(&self, value: bool) -> Result<(), StoreError>;
    fn is_index_spent_outputs(&self) -> Result<Option<bool>, StoreError>;
    fn set_index_spent_outputs(&self, value: bool) -> Result<(), StoreError>;

    // status
    fn get_is_at_tip(&self) -> Result<bool, StoreError>;
    fn set_is_at_tip(&self, value: bool) -> Result<(), StoreError>;

    // block
    fn get_block_count(&self) -> Result<u64, StoreError>;
    fn get_purged_blocks_count(&self) -> Result<u64, StoreError>;

    fn get_block_hash(&self, height: u64) -> Result<BlockHash, StoreError>;

    fn get_block_by_hash(&self, hash: &BlockHash) -> Result<Block, StoreError>;
    fn get_blocks_by_heights(
        &self,
        from_height: u64,
        to_height: u64,
    ) -> Result<HashMap<u64, Block>, StoreError>;

    // mempool
    fn is_tx_in_mempool(&self, txid: &SerializedTxid) -> Result<bool, StoreError>;
    fn get_mempool_txids(&self) -> Result<HashMap<SerializedTxid, MempoolEntry>, StoreError>;
    fn get_mempool_count(&self) -> Result<usize, StoreError>;
    /// Mempool txids in txid byte order.
    fn get_mempool_txids_page(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<SerializedTxid>, StoreError>;
    /// Mempool entries in txid byte order.
    fn get_mempool_entries_page(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<(SerializedTxid, MempoolEntry)>, StoreError>;
    fn get_mempool_entry(&self, txid: &SerializedTxid) -> Result<MempoolEntry, StoreError>;
    fn get_mempool_entries(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Option<MempoolEntry>>, StoreError>;
    fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, MempoolEntry>, StoreError>;

    // outpoint
    fn get_tx_out(
        &self,
        outpoint: &SerializedOutPoint,
        mempool: Option<bool>,
    ) -> Result<TxOut, StoreError>;
    fn get_tx_out_with_mempool_spent_update(
        &self,
        outpoint: &SerializedOutPoint,
        mempool: Option<bool>,
    ) -> Result<TxOut, StoreError>;
    fn get_tx_outs(
        &self,
        outpoints: &[SerializedOutPoint],
        mempool: Option<bool>,
    ) -> Result<HashMap<SerializedOutPoint, TxOut>, StoreError>;
    fn get_tx_outs_with_mempool_spent_update(
        &self,
        outpoints: &[SerializedOutPoint],
        mempool: Option<bool>,
    ) -> Result<HashMap<SerializedOutPoint, TxOut>, StoreError>;

    // transaction changes
    fn get_txs_state_changes(
        &self,
        txids: &[SerializedTxid],
        mempool: bool,
    ) -> Result<HashMap<SerializedTxid, TransactionStateChange>, StoreError>;

    // bitcoin transactions
    fn get_transaction_raw(
        &self,
        txid: &SerializedTxid,
        mempool: Option<bool>,
    ) -> Result<Vec<u8>, StoreError>;
    fn get_transaction(
        &self,
        txid: &SerializedTxid,
        mempool: Option<bool>,
    ) -> Result<Transaction, StoreError>;
    fn get_transaction_confirming_block(
        &self,
        txid: &SerializedTxid,
    ) -> Result<BlockId, StoreError>;
    fn get_transaction_confirming_blocks(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Option<BlockId>>, StoreError>;
    fn get_inputs_outputs_from_transaction(
        &self,
        transaction: &bitcoin::Transaction,
        txid: &SerializedTxid,
    ) -> Result<InputsOutputs, StoreError>;
    fn partition_transactions_by_existence(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<(Vec<SerializedTxid>, Vec<SerializedTxid>), StoreError>;

    // rune transactions
    fn get_last_rune_transactions(
        &self,
        rune_id: &RuneId,
        pagination: Option<Pagination>,
        mempool: Option<bool>,
    ) -> Result<PaginationResponse<SerializedTxid>, StoreError>;

    // runes
    fn get_runes_count(&self) -> Result<u64, StoreError>;
    fn get_rune(&self, rune_id: &RuneId) -> Result<RuneEntry, StoreError>;
    fn get_rune_id(&self, rune: &Rune) -> Result<RuneId, StoreError>;
    fn get_runes_by_ids(
        &self,
        rune_ids: &[RuneId],
    ) -> Result<HashMap<RuneId, RuneEntry>, StoreError>;
    fn get_runes(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<(RuneId, RuneEntry)>, StoreError>;

    // inscription
    fn get_inscription(&self, inscription_id: &InscriptionId) -> Result<Inscription, StoreError>;

    // address
    fn get_script_pubkey_outpoints(
        &self,
        script_pubkey: &ScriptBuf,
        mempool: Option<bool>,
    ) -> Result<Vec<SerializedOutPoint>, StoreError>;
    /// One page of `get_script_pubkey_outpoints(script_pubkey, None)`, same set and order.
    fn get_script_pubkey_outpoints_page(
        &self,
        script_pubkey: &ScriptBuf,
        pagination: Pagination,
    ) -> Result<PaginationResponse<SerializedOutPoint>, StoreError>;

    // batch
    fn batch_update(&self, update: &BatchUpdate, mempool: bool) -> Result<(), StoreError>;
    fn batch_delete(&self, delete: &BatchDelete) -> Result<(), StoreError>;
    fn batch_rollback(&self, rollback: &BatchRollback, mempool: bool) -> Result<(), StoreError>;

    /// Called once the indexer reaches tip so the underlying database can
    /// switch from bulk-load settings to normal online mode. Default
    /// implementation is a no-op.
    fn finish_bulk_load(&self) -> Result<(), StoreError>;
}

impl Store for RocksDB {
    fn is_index_addresses(&self) -> Result<Option<bool>, StoreError> {
        Ok(self.is_index_addresses()?)
    }

    fn set_index_addresses(&self, value: bool) -> Result<(), StoreError> {
        Ok(self.set_index_addresses(value)?)
    }

    fn is_index_bitcoin_transactions(&self) -> Result<Option<bool>, StoreError> {
        Ok(self.is_index_bitcoin_transactions()?)
    }

    fn set_index_bitcoin_transactions(&self, value: bool) -> Result<(), StoreError> {
        Ok(self.set_index_bitcoin_transactions(value)?)
    }

    fn is_index_spent_outputs(&self) -> Result<Option<bool>, StoreError> {
        Ok(self.is_index_spent_outputs()?)
    }

    fn set_index_spent_outputs(&self, value: bool) -> Result<(), StoreError> {
        Ok(self.set_index_spent_outputs(value)?)
    }

    fn get_is_at_tip(&self) -> Result<bool, StoreError> {
        Ok(self.get_is_at_tip()?)
    }

    fn set_is_at_tip(&self, value: bool) -> Result<(), StoreError> {
        Ok(self.set_is_at_tip(value)?)
    }

    fn get_block_count(&self) -> Result<u64, StoreError> {
        Ok(self.get_block_count()?)
    }

    fn get_purged_blocks_count(&self) -> Result<u64, StoreError> {
        Ok(self.get_purged_blocks_count()?)
    }

    fn get_block_hash(&self, height: u64) -> Result<BlockHash, StoreError> {
        Ok(self.get_block_hash(height)?)
    }

    fn get_block_by_hash(&self, hash: &BlockHash) -> Result<Block, StoreError> {
        Ok(self.get_block_by_hash(hash)?)
    }

    fn get_blocks_by_heights(
        &self,
        from_height: u64,
        to_height: u64,
    ) -> Result<HashMap<u64, Block>, StoreError> {
        let block_hashes = self.get_block_hashes_by_height(from_height, to_height)?;
        let blocks = self.get_blocks_by_hashes(&block_hashes)?;
        let mut result = HashMap::default();
        for (_, block) in blocks {
            let height = block.height;
            result.insert(height, block);
        }

        Ok(result)
    }

    fn get_runes_by_ids(
        &self,
        rune_ids: &[RuneId],
    ) -> Result<HashMap<RuneId, RuneEntry>, StoreError> {
        Ok(self.get_runes_by_ids(rune_ids)?)
    }

    fn get_runes(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<(RuneId, RuneEntry)>, StoreError> {
        let runes_count = self.get_runes_count()?;
        let (skip, limit) = pagination.into();

        let start = runes_count.saturating_sub(skip);
        let end = runes_count.saturating_sub(skip).saturating_sub(limit);

        let mut runes = Vec::new();
        for i in (end..start).rev() {
            let rune_id = self.get_rune_id_by_number(i)?;
            let rune_entry = self.get_rune(&rune_id)?;
            runes.push((rune_id, rune_entry));
        }

        let offset = skip + runes.len() as u64;
        Ok(PaginationResponse {
            items: runes,
            offset,
        })
    }

    fn get_mempool_txids(&self) -> Result<HashMap<SerializedTxid, MempoolEntry>, StoreError> {
        Ok(self.get_mempool_txids()?)
    }

    fn get_mempool_count(&self) -> Result<usize, StoreError> {
        Ok(self.get_mempool_count()?)
    }

    fn get_mempool_txids_page(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<SerializedTxid>, StoreError> {
        let items = self.get_mempool_txids_page(pagination.skip, pagination.limit)?;
        Ok(page_response(pagination, items))
    }

    fn get_mempool_entries_page(
        &self,
        pagination: Pagination,
    ) -> Result<PaginationResponse<(SerializedTxid, MempoolEntry)>, StoreError> {
        let items = self.get_mempool_entries_page(pagination.skip, pagination.limit)?;
        Ok(page_response(pagination, items))
    }

    fn is_tx_in_mempool(&self, txid: &SerializedTxid) -> Result<bool, StoreError> {
        Ok(self.is_tx_in_mempool(txid)?)
    }

    fn get_mempool_entry(&self, txid: &SerializedTxid) -> Result<MempoolEntry, StoreError> {
        Ok(self.get_mempool_entry(txid)?)
    }

    fn get_mempool_entries(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Option<MempoolEntry>>, StoreError> {
        Ok(self.get_mempool_entries(txids)?)
    }

    fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, MempoolEntry>, StoreError> {
        Ok(self.get_mempool_entries_with_ancestors(txids)?)
    }

    fn get_tx_out(
        &self,
        outpoint: &SerializedOutPoint,
        mempool: Option<bool>,
    ) -> Result<TxOut, StoreError> {
        if let Some(mempool) = mempool {
            Ok(self.get_tx_out(outpoint, mempool)?)
        } else {
            match self.get_tx_out(outpoint, false) {
                Ok(tx_out) => Ok(tx_out),
                Err(err) => match err {
                    RocksDBError::NotFound(_) => Ok(self.get_tx_out(outpoint, true)?),
                    other => Err(StoreError::DB(other)),
                },
            }
        }
    }

    fn get_tx_out_with_mempool_spent_update(
        &self,
        outpoint: &SerializedOutPoint,
        mempool: Option<bool>,
    ) -> Result<TxOut, StoreError> {
        let mut tx_out = if let Some(mempool) = mempool {
            self.get_tx_out(outpoint, mempool)?
        } else {
            match self.get_tx_out(outpoint, false) {
                Ok(tx_out) => tx_out,
                Err(err) => match err {
                    RocksDBError::NotFound(_) => self.get_tx_out(outpoint, true)?,
                    other => return Err(StoreError::DB(other)),
                },
            }
        };

        let spent_outpoints: HashMap<SerializedOutPoint, Option<SpenderReference>> =
            self.get_spent_outpoints_in_mempool(&[*outpoint])?;

        let spent_in_mempool = spent_outpoints.get(outpoint);

        if let Some(Some(spent)) = spent_in_mempool {
            tx_out.spent = SpentStatus::Spent(spent.clone());
        }

        Ok(tx_out)
    }

    fn get_tx_outs(
        &self,
        outpoints: &[SerializedOutPoint],
        mempool: Option<bool>,
    ) -> Result<HashMap<SerializedOutPoint, TxOut>, StoreError> {
        Ok(self.get_tx_outs(outpoints, mempool)?)
    }

    fn get_tx_outs_with_mempool_spent_update(
        &self,
        outpoints: &[SerializedOutPoint],
        mempool: Option<bool>,
    ) -> Result<HashMap<SerializedOutPoint, TxOut>, StoreError> {
        let mut tx_outs = self.get_tx_outs(outpoints, mempool)?;
        let spent_outpoints: HashMap<SerializedOutPoint, Option<SpenderReference>> =
            self.get_spent_outpoints_in_mempool(outpoints)?;

        for (outpoint, output) in tx_outs.iter_mut() {
            let spent_in_mempool = spent_outpoints.get(outpoint);

            if let Some(Some(spent)) = spent_in_mempool {
                output.spent = SpentStatus::Spent(spent.clone());
            }
        }

        Ok(tx_outs)
    }

    fn get_txs_state_changes(
        &self,
        txids: &[SerializedTxid],
        mempool: bool,
    ) -> Result<HashMap<SerializedTxid, TransactionStateChange>, StoreError> {
        Ok(self.get_txs_state_changes(txids, mempool)?)
    }

    fn get_transaction(
        &self,
        txid: &SerializedTxid,
        mempool: Option<bool>,
    ) -> Result<Transaction, StoreError> {
        let (transaction, status, _mempool) = if let Some(mempool) = mempool {
            let status = if !mempool {
                self.get_transaction_confirming_block(txid)?
                    .into_transaction_status()
            } else {
                TransactionStatus {
                    confirmed: false,
                    block_height: None,
                    block_hash: None,
                }
            };

            (self.get_transaction(txid, mempool)?, status, mempool)
        } else {
            match self.get_transaction(txid, false) {
                Ok(transaction) => {
                    let status = self
                        .get_transaction_confirming_block(txid)?
                        .into_transaction_status();

                    (transaction, status, false)
                }
                Err(err) => match err {
                    RocksDBError::NotFound(_) => {
                        let status = TransactionStatus {
                            confirmed: false,
                            block_height: None,
                            block_hash: None,
                        };

                        (self.get_transaction(txid, true)?, status, true)
                    }
                    other => return Err(StoreError::DB(other)),
                },
            }
        };

        let (inputs, outputs) = self.get_inputs_outputs_from_transaction(&transaction, txid)?;

        Ok(Transaction::from((transaction, status, inputs, outputs)))
    }

    fn get_inputs_outputs_from_transaction(
        &self,
        transaction: &bitcoin::Transaction,
        txid: &SerializedTxid,
    ) -> Result<InputsOutputs, StoreError> {
        let prev_outpoints = transaction
            .input
            .iter()
            .map(|tx_in| tx_in.previous_output.into())
            .collect::<Vec<SerializedOutPoint>>();

        let outpoints = transaction
            .output
            .iter()
            .enumerate()
            .map(|(vout, _)| SerializedOutPoint::from_txid_vout(txid, vout as u32))
            .collect::<Vec<_>>();

        let all_outpoints = prev_outpoints
            .iter()
            .chain(outpoints.iter())
            .cloned()
            .collect::<Vec<_>>();

        let mut outputs_map = self.get_tx_outs_with_mempool_spent_update(&all_outpoints, None)?;

        let inputs = prev_outpoints
            .iter()
            .map(|outpoint| outputs_map.remove(outpoint))
            .collect::<Vec<_>>();

        let outputs = outpoints
            .iter()
            .map(|outpoint| outputs_map.remove(outpoint))
            .collect::<Vec<_>>();

        Ok((inputs, outputs))
    }

    fn get_transaction_raw(
        &self,
        txid: &SerializedTxid,
        mempool: Option<bool>,
    ) -> Result<Vec<u8>, StoreError> {
        if let Some(mempool) = mempool {
            Ok(self.get_transaction_raw(txid, mempool)?)
        } else {
            match self.get_transaction_raw(txid, false) {
                Ok(transaction) => Ok(transaction),
                Err(err) => match err {
                    RocksDBError::NotFound(_) => Ok(self.get_transaction_raw(txid, true)?),
                    other => Err(StoreError::DB(other)),
                },
            }
        }
    }

    fn partition_transactions_by_existence(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<(Vec<SerializedTxid>, Vec<SerializedTxid>), StoreError> {
        Ok(self.partition_transactions_by_existence(txids)?)
    }

    fn get_transaction_confirming_block(
        &self,
        txid: &SerializedTxid,
    ) -> Result<BlockId, StoreError> {
        Ok(self.get_transaction_confirming_block(txid)?)
    }

    fn get_transaction_confirming_blocks(
        &self,
        txids: &[SerializedTxid],
    ) -> Result<HashMap<SerializedTxid, Option<BlockId>>, StoreError> {
        Ok(self.get_transaction_confirming_blocks(txids)?)
    }

    fn get_runes_count(&self) -> Result<u64, StoreError> {
        Ok(self.get_runes_count()?)
    }

    fn get_rune(&self, rune_id: &RuneId) -> Result<RuneEntry, StoreError> {
        Ok(self.get_rune(rune_id)?)
    }

    fn get_rune_id(&self, rune: &Rune) -> Result<RuneId, StoreError> {
        Ok(self.get_rune_id(&rune.0)?)
    }

    fn get_inscription(&self, inscription_id: &InscriptionId) -> Result<Inscription, StoreError> {
        Ok(self.get_inscription(inscription_id)?)
    }

    fn get_last_rune_transactions(
        &self,
        rune_id: &RuneId,
        pagination: Option<Pagination>,
        mempool: Option<bool>,
    ) -> Result<PaginationResponse<SerializedTxid>, StoreError> {
        if let Some(mempool) = mempool {
            return Ok(self.get_last_rune_transactions(rune_id, pagination, mempool)?);
        }

        // Mempool transactions are newer than confirmed ones, so the merged list is the mempool
        // list followed by the confirmed list. `offset` is the `skip` of the next page.
        let Pagination { skip, limit } = pagination.unwrap_or_default();

        let (mut items, mempool_skipped) =
            self.rune_transactions_page(rune_id, skip, limit, true)?;

        // Whatever the mempool list could not absorb is skipped in the confirmed list.
        let confirmed_skip = skip - mempool_skipped;
        let confirmed_limit = limit - items.len() as u64;
        if confirmed_limit > 0 {
            let (confirmed_items, _) =
                self.rune_transactions_page(rune_id, confirmed_skip, confirmed_limit, false)?;
            items.extend(confirmed_items);
        }

        let offset = skip.saturating_add(items.len() as u64);
        Ok(PaginationResponse { items, offset })
    }

    fn get_script_pubkey_outpoints(
        &self,
        script_pubkey: &ScriptBuf,
        mempool: Option<bool>,
    ) -> Result<Vec<SerializedOutPoint>, StoreError> {
        if let Some(mempool) = mempool {
            Ok(self.get_script_pubkey_outpoints(script_pubkey, mempool)?)
        } else {
            let mut ledger_entry = self.get_script_pubkey_outpoints(script_pubkey, false)?;
            let mempool_entry = self.get_script_pubkey_outpoints(script_pubkey, true)?;

            ledger_entry.extend(mempool_entry);

            // Get spent outpoints in mempool.
            let spent_outpoints = self.get_spent_outpoints_in_mempool(&ledger_entry)?;
            ledger_entry.retain(|outpoint| {
                let spent = spent_outpoints.get(outpoint);
                match spent {
                    Some(Some(_)) => false,
                    Some(None) => true,
                    None => true,
                }
            });

            Ok(ledger_entry)
        }
    }

    fn get_script_pubkey_outpoints_page(
        &self,
        script_pubkey: &ScriptBuf,
        pagination: Pagination,
    ) -> Result<PaginationResponse<SerializedOutPoint>, StoreError> {
        let items = self.get_script_pubkey_outpoints_page(
            script_pubkey,
            pagination.skip,
            pagination.limit,
        )?;
        Ok(page_response(pagination, items))
    }

    fn batch_update(&self, update: &BatchUpdate, mempool: bool) -> Result<(), StoreError> {
        Ok(self.batch_update(update, mempool)?)
    }

    fn batch_delete(&self, delete: &BatchDelete) -> Result<(), StoreError> {
        Ok(self.batch_delete(delete)?)
    }

    fn batch_rollback(&self, rollback: &BatchRollback, mempool: bool) -> Result<(), StoreError> {
        Ok(self.batch_rollback(rollback, mempool)?)
    }

    fn finish_bulk_load(&self) -> Result<(), StoreError> {
        self.switch_to_online_mode().map_err(StoreError::DB)
    }
}

/// `offset` follows the rune-transactions convention: the skip to send for the next page.
fn page_response<T>(pagination: Pagination, items: Vec<T>) -> PaginationResponse<T> {
    let offset = pagination.skip.saturating_add(items.len() as u64);
    PaginationResponse { items, offset }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::db::testing::{add_rune_txs, remove_txs, txid, TempDb},
    };

    fn merged_page(
        db: &RocksDB,
        rune_id: RuneId,
        skip: u64,
        limit: u64,
    ) -> PaginationResponse<SerializedTxid> {
        Store::get_last_rune_transactions(db, &rune_id, Some(Pagination { skip, limit }), None)
            .unwrap()
    }

    #[test]
    fn merged_rune_transactions_paginate_mempool_then_confirmed() {
        let db = TempDb::new();
        let rune_id = RuneId::new(840_000, 1);

        add_rune_txs(&db, &[(rune_id, (1..=300).map(txid).collect())], false);
        add_rune_txs(&db, &[(rune_id, (1_001..=1_300).map(txid).collect())], true);
        // Transactions leaving the mempool leave holes in its list.
        let left: Vec<_> = (1_001..=1_300).filter(|n| n % 4 == 0).map(txid).collect();
        remove_txs(&db, &left, true, vec![]);

        let mempool: Vec<_> = (1_001..=1_300)
            .rev()
            .filter(|n| n % 4 != 0)
            .map(txid)
            .collect();
        let confirmed: Vec<_> = (1..=300).rev().map(txid).collect();
        let expected: Vec<_> = mempool.iter().chain(confirmed.iter()).cloned().collect();
        let mempool_len = mempool.len() as u64;

        // Following `offset` walks the whole merged list exactly once.
        let mut items = Vec::new();
        let mut skip = 0;
        loop {
            let page = merged_page(&db, rune_id, skip, 50);
            assert!(page.items.len() <= 50);
            assert_eq!(page.offset, skip + page.items.len() as u64);
            if page.items.is_empty() {
                break;
            }
            skip = page.offset;
            items.extend(page.items);
        }
        assert_eq!(items, expected);

        // A page straddling both lists.
        let page = merged_page(&db, rune_id, mempool_len - 10, 50);
        assert_eq!(page.items, expected[mempool_len as usize - 10..][..50]);

        // Skipping past the mempool list skips into the confirmed one.
        let page = merged_page(&db, rune_id, mempool_len + 5, 50);
        assert_eq!(page.items, confirmed[5..55]);
        assert_eq!(page.offset, mempool_len + 55);

        let page = merged_page(&db, rune_id, 20, 0);
        assert!(page.items.is_empty());
        assert_eq!(page.offset, 20);

        let page = Store::get_last_rune_transactions(&*db, &rune_id, None, None).unwrap();
        assert_eq!(page.items, expected[..50]);
        assert_eq!(page.offset, 50);
    }

    fn all_pages<T>(
        limit: u64,
        mut fetch: impl FnMut(Pagination) -> PaginationResponse<T>,
    ) -> Vec<T> {
        let mut items = Vec::new();
        let mut skip = 0;
        loop {
            let page = fetch(Pagination { skip, limit });
            assert!(page.items.len() as u64 <= limit);
            assert_eq!(page.offset, skip + page.items.len() as u64);
            if page.items.is_empty() {
                return items;
            }
            skip = page.offset;
            items.extend(page.items);
        }
    }

    fn outpoint(n: u64) -> SerializedOutPoint {
        SerializedOutPoint::from_txid_vout(&txid(n), 0)
    }

    fn mempool_entry(vsize: u64) -> MempoolEntry {
        MempoolEntry {
            vsize,
            weight: None,
            descendant_count: 1,
            descendant_size: vsize,
            ancestor_count: 1,
            ancestor_size: vsize,
            fees: arch_titan_types_core::MempoolEntryFee {
                base: 1,
                descendant: 1,
                ancestor: 1,
            },
            depends: vec![],
            spent_by: vec![],
        }
    }

    #[test]
    fn address_outpoint_pages_walk_the_merged_list() {
        let db = TempDb::new();
        let script = ScriptBuf::from_bytes(vec![0x51]);

        // More confirmed outputs than one spent-check chunk, then mempool-created ones.
        let mut confirmed = BatchUpdate::new(0, 0, 0);
        confirmed
            .script_pubkeys
            .insert(script.clone(), ((0..300).map(outpoint).collect(), vec![]));
        db.batch_update(&confirmed, false).unwrap();

        let mut mempool = BatchUpdate::new(0, 0, 0);
        mempool.script_pubkeys.insert(
            script.clone(),
            ((1_000..1_005).map(outpoint).collect(), vec![]),
        );
        // Outputs spent in the mempool are left out, on both sides of the chunk boundary.
        for n in [10, 299, 1_002] {
            mempool.spent_outpoints_in_mempool.insert(
                outpoint(n),
                SpenderReference {
                    txid: txid(9_000 + n),
                    vin: 0,
                },
            );
        }
        db.batch_update(&mempool, true).unwrap();

        let expected = Store::get_script_pubkey_outpoints(&*db, &script, None).unwrap();
        assert_eq!(expected.len(), 302);
        assert!(!expected.contains(&outpoint(10)));
        assert!(!expected.contains(&outpoint(1_002)));
        let mempool_created: Vec<_> = (1_000..1_005)
            .filter(|n| *n != 1_002)
            .map(outpoint)
            .collect();
        assert_eq!(expected[298..], mempool_created[..]);

        for limit in [1, 7, 50] {
            let paged = all_pages(limit, |pagination| {
                Store::get_script_pubkey_outpoints_page(&*db, &script, pagination).unwrap()
            });
            assert_eq!(paged, expected, "limit {limit}");
        }

        let page = Store::get_script_pubkey_outpoints_page(
            &*db,
            &script,
            Pagination {
                skip: 295,
                limit: 5,
            },
        )
        .unwrap();
        assert_eq!(page.items, expected[295..300]);
        assert_eq!(page.offset, 300);
    }

    #[test]
    fn mempool_pages_walk_all_entries_in_txid_order() {
        let db = TempDb::new();
        let mut update = BatchUpdate::new(0, 0, 0);
        for n in 0..120 {
            update.mempool_txs.insert(txid(n), mempool_entry(100 + n));
        }
        db.batch_update(&update, true).unwrap();

        let txids = all_pages(50, |pagination| {
            Store::get_mempool_txids_page(&*db, pagination).unwrap()
        });
        let mut expected: Vec<_> = (0..120).map(txid).collect();
        expected.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        assert_eq!(txids, expected);

        let entries = all_pages(7, |pagination| {
            Store::get_mempool_entries_page(&*db, pagination).unwrap()
        });
        assert_eq!(
            entries.iter().map(|(txid, _)| *txid).collect::<Vec<_>>(),
            expected
        );
        for (txid, entry) in &entries {
            assert_eq!(*entry, Store::get_mempool_entry(&*db, txid).unwrap());
        }

        let page = Store::get_mempool_txids_page(&*db, Pagination { skip: 5, limit: 0 }).unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.offset, 5);
    }
}
