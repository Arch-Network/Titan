use {
    super::{
        entry::Entry,
        util::{
            parse_outpoint_from_script_pubkey_key, rune_index_key, rune_transaction_index,
            rune_transaction_key, rune_transaction_prefix, rune_transaction_prefix_end,
            script_pubkey_outpoint_to_bytes, script_pubkey_search_key,
        },
        *,
    },
    crate::models::{
        BatchDelete, BatchRollback, BatchUpdate, BlockId, Inscription, RuneEntry,
        TransactionStateChange, TxRuneIndexRef,
    },
    arch_titan_types_api::{Pagination, PaginationResponse, Subscription},
    arch_titan_types_core::{
        Block, InscriptionId, MempoolEntry, RuneId, SerializedOutPoint, SerializedTxid,
        SpenderReference, TxOut,
    },
    bitcoin::{consensus, hashes::Hash, BlockHash, ScriptBuf, Transaction},
    mapper::DBResultMapper,
    rocksdb::{
        BlockBasedOptions, BoundColumnFamily, Cache, ColumnFamilyDescriptor, DBWithThreadMode,
        Direction, FlushOptions, IteratorMode, MultiThreaded, Options, WriteBatch,
    },
    rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet},
    std::{
        collections::VecDeque,
        sync::{Arc, RwLock},
    },
    uuid::Uuid,
};

/// All writes go through the WAL (the default `WriteOptions`), so every `WriteBatch` is
/// atomic and durable across column families: after a crash RocksDB replays the WAL to a
/// consistent point in time. Counters (block count, rune counters, ...) are always written in
/// the same batch as the data they describe.
pub struct RocksDB {
    db: DBWithThreadMode<MultiThreaded>,
    mempool_cache: RwLock<HashMap<SerializedTxid, MempoolEntry>>,
}

pub type DBResult<T> = Result<T, RocksDBError>;

const BLOCKS_CF: &str = "blocks";
const BLOCK_HEIGHT_TO_HASH_CF: &str = "block_height_to_hash";

const OUTPOINTS_CF: &str = "outpoints";
const OUTPOINTS_MEMPOOL_CF: &str = "mempool_outpoints";

const TRANSACTIONS_STATE_CHANGE_CF: &str = "transactions_state_change";
const TRANSACTIONS_STATE_CHANGE_MEMPOOL_CF: &str = "mempool_transactions_state_change";

const RUNE_TRANSACTIONS_CF: &str = "rune_transactions";
const RUNE_TRANSACTIONS_MEMPOOL_CF: &str = "rune_transactions_mempool";

const TRANSACTION_RUNE_INDEX_CF: &str = "transaction_rune_index";
const TRANSACTION_RUNE_INDEX_MEMPOOL_CF: &str = "transaction_rune_index_mempool";

const RUNES_COUNT_KEY: &str = "runes_count";
const RUNES_CF: &str = "runes";
const RUNE_IDS_CF: &str = "rune_ids";
const RUNE_NUMBER_CF: &str = "rune_number";

const INSCRIPTIONS_CF: &str = "inscriptions";

const SCRIPT_PUBKEYS_CF: &str = "script_pubkeys";
const SCRIPT_PUBKEYS_MEMPOOL_CF: &str = "script_pubkeys_mempool";

const OUTPOINT_TO_SCRIPT_PUBKEY_CF: &str = "outpoint_to_script_pubkey";
const OUTPOINT_TO_SCRIPT_PUBKEY_MEMPOOL_CF: &str = "outpoint_to_script_pubkey_mempool";
const SPENT_OUTPOINTS_MEMPOOL_CF: &str = "spent_outpoints_mempool";

const TRANSACTIONS_CF: &str = "transactions";
const TRANSACTIONS_MEMPOOL_CF: &str = "transactions_mempool";
const TRANSACTION_CONFIRMING_BLOCK_CF: &str = "transaction_confirming_block";

const MEMPOOL_CF: &str = "mempool";

const STATS_CF: &str = "stats";

const SETTINGS_CF: &str = "settings";

const SUBSCRIPTIONS_CF: &str = "subscriptions";

const INDEX_ADDRESSES_KEY: &str = "index_addresses";
const INDEX_BITCOIN_TRANSACTIONS_KEY: &str = "index_bitcoin_transactions";
const INDEX_SPENT_OUTPUTS_KEY: &str = "index_spent_outputs";

const BLOCK_COUNT_KEY: &str = "block_count";
const PURGED_BLOCKS_COUNT_KEY: &str = "purged_blocks_count";
const DB_SCHEMA_VERSION_KEY: &str = "db_schema_version";
const IS_AT_TIP_KEY: &str = "is_at_tip";

/// Increment this when the on-disk schema changes in a backward-incompatible way.
///
/// v2: rune transaction keys encode their index big-endian; the mainnet genesis rune is stored.
const EXPECTED_DB_SCHEMA_VERSION: u64 = 2;

/// Every column family the indexer uses. The default column family stores nothing.
const COLUMN_FAMILIES: [&str; 26] = [
    BLOCKS_CF,
    BLOCK_HEIGHT_TO_HASH_CF,
    OUTPOINTS_CF,
    OUTPOINTS_MEMPOOL_CF,
    TRANSACTIONS_STATE_CHANGE_CF,
    TRANSACTIONS_STATE_CHANGE_MEMPOOL_CF,
    RUNES_CF,
    RUNE_IDS_CF,
    RUNE_NUMBER_CF,
    INSCRIPTIONS_CF,
    MEMPOOL_CF,
    STATS_CF,
    RUNE_TRANSACTIONS_CF,
    RUNE_TRANSACTIONS_MEMPOOL_CF,
    TRANSACTION_RUNE_INDEX_CF,
    TRANSACTION_RUNE_INDEX_MEMPOOL_CF,
    SCRIPT_PUBKEYS_CF,
    SCRIPT_PUBKEYS_MEMPOOL_CF,
    OUTPOINT_TO_SCRIPT_PUBKEY_CF,
    OUTPOINT_TO_SCRIPT_PUBKEY_MEMPOOL_CF,
    SPENT_OUTPOINTS_MEMPOOL_CF,
    TRANSACTIONS_CF,
    TRANSACTIONS_MEMPOOL_CF,
    TRANSACTION_CONFIRMING_BLOCK_CF,
    SETTINGS_CF,
    SUBSCRIPTIONS_CF,
];

/// Block cache shared by every column family.
const BLOCK_CACHE_SIZE: usize = 1024 * 1024 * 1024; // 1 GiB
/// Cap on the memtable memory of all column families together.
const TOTAL_WRITE_BUFFER_SIZE: usize = 4 * 1024 * 1024 * 1024; // 4 GiB
/// Per column family memtable settings while catching up with the chain tip.
const SYNC_WRITE_BUFFER_SIZE: usize = 256 * 1024 * 1024; // 256 MiB
const SYNC_MAX_WRITE_BUFFER_NUMBER: i32 = 4;
/// Per column family memtable settings once the indexer is at the chain tip.
const ONLINE_WRITE_BUFFER_SIZE: usize = 128 * 1024 * 1024; // 128 MiB
const ONLINE_MAX_WRITE_BUFFER_NUMBER: i32 = 4;

impl RocksDB {
    pub fn open(file_path: &str) -> DBResult<Self> {
        let block_cache = Cache::new_lru_cache(BLOCK_CACHE_SIZE);
        let cf_opts = Self::column_family_options(&block_cache);

        // DB-wide options. Starting from the column-family options keeps the (unused) default
        // column family consistent with the others.
        let mut db_opts = cf_opts.clone();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        db_opts.set_max_log_file_size(64 * 1024 * 1024); // 64 MB info LOG files
        db_opts.set_keep_log_file_num(10);
        db_opts.set_recycle_log_file_num(5);
        db_opts.set_max_total_wal_size(1024 * 1024 * 1024); // 1 GB WAL budget
        db_opts.set_db_write_buffer_size(TOTAL_WRITE_BUFFER_SIZE);

        // Parallel background jobs / pipelined WAL
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        db_opts.increase_parallelism(cpus as i32);
        db_opts.set_enable_pipelined_write(true);

        // Keep flush/compaction I/O out of the page cache. Reads stay buffered: the indexer
        // does random point lookups and benefits from the OS page cache on top of the block
        // cache.
        db_opts.set_use_direct_io_for_flush_and_compaction(true);

        let descriptors = COLUMN_FAMILIES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, cf_opts.clone()));

        let db = DBWithThreadMode::<MultiThreaded>::open_cf_descriptors(
            &db_opts,
            file_path,
            descriptors,
        )?;

        let rocks_db = RocksDB {
            db,
            mempool_cache: RwLock::new(HashMap::default()),
        };

        // Verify that the on-disk schema is compatible with the running binary before reading
        // anything else.
        rocks_db.verify_schema_version()?;

        // Load initial state from DB
        *rocks_db
            .mempool_cache
            .write()
            .map_err(|_| RocksDBError::LockPoisoned)? = Self::read_all_mempool_txids(&rocks_db.db)?;

        Ok(rocks_db)
    }

    /// Options shared by every column family: one sized block cache, bloom filters for point
    /// lookups, LZ4/Zstd compression and sync-sized memtables.
    fn column_family_options(block_cache: &Cache) -> Options {
        let mut block_based_options = BlockBasedOptions::default();
        block_based_options.set_block_size(16 * 1024); // 16 KB
        block_based_options.set_block_cache(block_cache);
        block_based_options.set_bloom_filter(10.0, false);
        block_based_options.set_cache_index_and_filter_blocks(true);
        block_based_options.set_pin_l0_filter_and_index_blocks_in_cache(true);

        let mut opts = Options::default();
        opts.set_block_based_table_factory(&block_based_options);
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        opts.set_bottommost_compression_type(rocksdb::DBCompressionType::Zstd);
        opts.set_periodic_compaction_seconds(86400); // Run compaction every 24 hours
        opts.set_write_buffer_size(SYNC_WRITE_BUFFER_SIZE);
        opts.set_max_write_buffer_number(SYNC_MAX_WRITE_BUFFER_NUMBER);
        opts
    }

    /// Switch the column families from the initial-sync memtable sizes to the smaller online
    /// ones and flush everything written so far. Called whenever the indexer reaches the tip.
    pub fn switch_to_online_mode(&self) -> DBResult<()> {
        let write_buffer_size = ONLINE_WRITE_BUFFER_SIZE.to_string();
        let max_write_buffer_number = ONLINE_MAX_WRITE_BUFFER_NUMBER.to_string();
        for name in COLUMN_FAMILIES {
            let cf_handle = self.cf_handle(name)?;
            self.db.set_options_cf(
                &cf_handle,
                &[
                    ("write_buffer_size", write_buffer_size.as_str()),
                    ("max_write_buffer_number", max_write_buffer_number.as_str()),
                ],
            )?;
        }

        self.flush()?;
        Ok(())
    }

    fn cf_handle(&self, name: &str) -> DBResult<Arc<BoundColumnFamily<'_>>> {
        match self.db.cf_handle(name) {
            None => Err(RocksDBError::InvalidHandle(name.to_string())),
            Some(handle) => Ok(handle),
        }
    }

    fn get_option_vec_data<K: AsRef<[u8]>>(
        &self,
        cf_handle: &Arc<BoundColumnFamily>,
        key: K,
    ) -> DBResult<Option<Vec<u8>>> {
        match self.db.get_cf(cf_handle, key) {
            Ok(val) => Ok(val),
            Err(e) => Err(e)?,
        }
    }

    /// Multi-get on one column family. Values come back in key order; read errors are
    /// propagated instead of being treated as missing keys.
    fn multi_get_values<K: AsRef<[u8]>>(
        &self,
        cf_handle: &Arc<BoundColumnFamily>,
        keys: impl IntoIterator<Item = K>,
    ) -> DBResult<Vec<Option<Vec<u8>>>> {
        self.db
            .multi_get_cf(keys.into_iter().map(|key| (cf_handle, key)))
            .into_iter()
            .map(|value| value.map_err(RocksDBError::from))
            .collect()
    }

    /// Ensures the database schema version on disk matches the one compiled into the
    /// binary. If the key is missing (fresh database) the current version is written.
    /// If a mismatch is detected we return `RocksDBError::SchemaMismatch`.
    fn verify_schema_version(&self) -> DBResult<()> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;

        // Try to read existing version
        let stored_version: Option<u64> = self
            .get_option_vec_data(&cf_handle, DB_SCHEMA_VERSION_KEY)
            .mapped()?;

        match stored_version {
            Some(v) if v == EXPECTED_DB_SCHEMA_VERSION => Ok(()),
            Some(v) => Err(RocksDBError::SchemaMismatch(format!(
                "found version {v}, expected {EXPECTED_DB_SCHEMA_VERSION}. The on-disk format is not \
                 compatible with this binary and cannot be migrated in place: stop the indexer, \
                 delete the data directory and re-index from scratch."
            ))),
            None => {
                // Fresh DB – store the expected version for future runs.
                self.db.put_cf(
                    &cf_handle,
                    DB_SCHEMA_VERSION_KEY,
                    EXPECTED_DB_SCHEMA_VERSION.to_le_bytes(),
                )?;
                Ok(())
            }
        }
    }

    pub fn is_index_addresses(&self) -> DBResult<Option<bool>> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;
        let val: Option<u64> = self
            .get_option_vec_data(&cf_handle, INDEX_ADDRESSES_KEY)
            .mapped()?;

        Ok(val.map(|v| v == 1))
    }

    pub fn set_index_addresses(&self, value: bool) -> DBResult<()> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;
        self.db.put_cf(
            &cf_handle,
            INDEX_ADDRESSES_KEY,
            (value as u64).to_le_bytes(),
        )?;
        Ok(())
    }

    pub fn is_index_bitcoin_transactions(&self) -> DBResult<Option<bool>> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;
        let val: Option<u64> = self
            .get_option_vec_data(&cf_handle, INDEX_BITCOIN_TRANSACTIONS_KEY)
            .mapped()?;

        Ok(val.map(|v| v == 1))
    }

    pub fn set_index_bitcoin_transactions(&self, value: bool) -> DBResult<()> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;
        self.db.put_cf(
            &cf_handle,
            INDEX_BITCOIN_TRANSACTIONS_KEY,
            (value as u64).to_le_bytes(),
        )?;
        Ok(())
    }

    pub fn is_index_spent_outputs(&self) -> DBResult<Option<bool>> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;
        let val: Option<u64> = self
            .get_option_vec_data(&cf_handle, INDEX_SPENT_OUTPUTS_KEY)
            .mapped()?;

        Ok(val.map(|v| v == 1))
    }

    pub fn set_index_spent_outputs(&self, value: bool) -> DBResult<()> {
        let cf_handle = self.cf_handle(SETTINGS_CF)?;
        self.db.put_cf(
            &cf_handle,
            INDEX_SPENT_OUTPUTS_KEY,
            (value as u64).to_le_bytes(),
        )?;
        Ok(())
    }

    pub fn get_block_count(&self) -> DBResult<u64> {
        let cf_handle = self.cf_handle(STATS_CF)?;
        Ok(self
            .get_option_vec_data(&cf_handle, BLOCK_COUNT_KEY)
            .mapped()?
            .unwrap_or(0))
    }

    pub fn get_purged_blocks_count(&self) -> DBResult<u64> {
        let cf_handle = self.cf_handle(STATS_CF)?;
        Ok(self
            .get_option_vec_data(&cf_handle, PURGED_BLOCKS_COUNT_KEY)
            .mapped()?
            .unwrap_or(0))
    }

    pub fn get_is_at_tip(&self) -> DBResult<bool> {
        let cf_handle = self.cf_handle(STATS_CF)?;
        let val: Option<u64> = self
            .get_option_vec_data(&cf_handle, IS_AT_TIP_KEY)
            .mapped()?;
        Ok(val.map(|v| v == 1).unwrap_or(false))
    }

    pub fn set_is_at_tip(&self, value: bool) -> DBResult<()> {
        let cf_handle = self.cf_handle(STATS_CF)?;
        self.db
            .put_cf(&cf_handle, IS_AT_TIP_KEY, (value as u64).to_le_bytes())?;
        Ok(())
    }

    pub fn get_block_hash(&self, height: u64) -> DBResult<BlockHash> {
        let cf_handle = self.cf_handle(BLOCK_HEIGHT_TO_HASH_CF)?;
        self.get_option_vec_data(&cf_handle, height.to_le_bytes())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "block hash not found: {}",
                height
            )))
    }

    pub fn get_block_by_hash(&self, hash: &BlockHash) -> DBResult<Block> {
        let cf_handle = self.cf_handle(BLOCKS_CF)?;
        self.get_option_vec_data(&cf_handle, hash.as_raw_hash().to_byte_array())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!("block not found: {}", hash)))
    }

    pub fn get_block_hashes_by_height(
        &self,
        from_height: u64,
        to_height: u64,
    ) -> DBResult<Vec<BlockHash>> {
        let cf_handle = self.cf_handle(BLOCK_HEIGHT_TO_HASH_CF)?;
        let values =
            self.multi_get_values(&cf_handle, (from_height..to_height).map(u64::to_le_bytes))?;

        let mut result = Vec::new();
        for value in values.into_iter().flatten() {
            result.push(BlockHash::from_slice(&value).map_err(|_| RocksDBError::InvalidBlockHash)?);
        }

        Ok(result)
    }

    pub fn get_blocks_by_hashes(
        &self,
        hashes: &[BlockHash],
    ) -> DBResult<HashMap<BlockHash, Block>> {
        let cf_handle = self.cf_handle(BLOCKS_CF)?;
        let values = self.multi_get_values(
            &cf_handle,
            hashes.iter().map(|hash| hash.as_raw_hash().to_byte_array()),
        )?;

        let mut result = HashMap::default();
        for (hash, value) in hashes.iter().zip(values) {
            if let Some(value) = value {
                result.insert(*hash, Block::load(&value)?);
            }
        }

        Ok(result)
    }

    pub fn get_rune(&self, rune_id: &RuneId) -> DBResult<RuneEntry> {
        let cf_handle = self.cf_handle(RUNES_CF)?;
        self.get_option_vec_data(&cf_handle, rune_id.to_bytes())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "rune not found: {}",
                rune_id
            )))
    }

    pub fn get_runes_by_ids(&self, rune_ids: &[RuneId]) -> DBResult<HashMap<RuneId, RuneEntry>> {
        let cf_handle = self.cf_handle(RUNES_CF)?;
        let values = self.multi_get_values(&cf_handle, rune_ids.iter().map(RuneId::to_bytes))?;

        let mut result = HashMap::default();
        for (rune_id, value) in rune_ids.iter().zip(values) {
            if let Some(value) = value {
                result.insert(*rune_id, RuneEntry::load(&value)?);
            }
        }

        Ok(result)
    }

    pub fn get_rune_id_by_number(&self, number: u64) -> DBResult<RuneId> {
        let cf_handle = self.cf_handle(RUNE_NUMBER_CF)?;
        let rune_id: RuneId = self
            .get_option_vec_data(&cf_handle, number.to_le_bytes())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "rune id not found: {}",
                number
            )))?;

        Ok(rune_id)
    }

    pub fn get_rune_ids_by_numbers(&self, numbers: &[u64]) -> DBResult<HashMap<u64, RuneId>> {
        let cf_handle = self.cf_handle(RUNE_NUMBER_CF)?;
        let values = self.multi_get_values(&cf_handle, numbers.iter().map(|n| n.to_le_bytes()))?;

        let mut result = HashMap::default();
        for (number, value) in numbers.iter().zip(values) {
            if let Some(value) = value {
                result.insert(*number, RuneId::load(&value)?);
            }
        }

        Ok(result)
    }

    pub fn get_tx_out(&self, outpoint: &SerializedOutPoint, mempool: bool) -> DBResult<TxOut> {
        let cf_handle = if mempool {
            self.cf_handle(OUTPOINTS_MEMPOOL_CF)?
        } else {
            self.cf_handle(OUTPOINTS_CF)?
        };

        self.get_option_vec_data(&cf_handle, outpoint.as_ref())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "outpoint not found: {}",
                outpoint
            )))
    }

    pub fn get_tx_outs<T: AsRef<[u8]> + Clone + Eq + std::hash::Hash>(
        &self,
        outpoints: &[T],
        mempool: Option<bool>,
    ) -> DBResult<HashMap<T, TxOut>> {
        if let Some(mempool) = mempool {
            Ok(self.get_tx_outs_with_mempool(outpoints, mempool)?)
        } else {
            let mut ledger_outpoints = self.get_tx_outs_with_mempool(outpoints, false)?;

            let remaining: Vec<T> = outpoints
                .iter()
                .filter_map(|outpoint| {
                    (!ledger_outpoints.contains_key(outpoint)).then_some(outpoint.clone())
                })
                .collect();

            let mempool_outpoints = self.get_tx_outs_with_mempool(&remaining, true)?;

            ledger_outpoints.extend(mempool_outpoints);

            Ok(ledger_outpoints)
        }
    }

    fn get_tx_outs_with_mempool<T: AsRef<[u8]> + Clone + Eq + std::hash::Hash>(
        &self,
        outpoints: &[T],
        mempool: bool,
    ) -> DBResult<HashMap<T, TxOut>> {
        let cf_handle = if mempool {
            self.cf_handle(OUTPOINTS_MEMPOOL_CF)?
        } else {
            self.cf_handle(OUTPOINTS_CF)?
        };

        let values = self.multi_get_values(&cf_handle, outpoints.iter().map(|o| o.as_ref()))?;

        let mut result = HashMap::default();
        for (outpoint, value) in outpoints.iter().zip(values) {
            if let Some(value) = value {
                result.insert(outpoint.clone(), TxOut::load(&value)?);
            }
        }

        Ok(result)
    }

    pub fn get_txs_state_changes(
        &self,
        txids: &[SerializedTxid],
        mempool: bool,
    ) -> DBResult<HashMap<SerializedTxid, TransactionStateChange>> {
        let cf_handle = if mempool {
            self.cf_handle(TRANSACTIONS_STATE_CHANGE_MEMPOOL_CF)?
        } else {
            self.cf_handle(TRANSACTIONS_STATE_CHANGE_CF)?
        };

        let values = self.multi_get_values(&cf_handle, txids.iter().map(|txid| txid.as_ref()))?;

        let mut result = HashMap::default();
        for (txid, value) in txids.iter().zip(values) {
            if let Some(value) = value {
                result.insert(*txid, TransactionStateChange::load(&value)?);
            }
        }

        Ok(result)
    }

    pub fn get_runes_count(&self) -> DBResult<u64> {
        let cf_handle = self.cf_handle(STATS_CF)?;
        Ok(self
            .get_option_vec_data(&cf_handle, RUNES_COUNT_KEY)
            .mapped()?
            .unwrap_or(0))
    }

    pub fn get_rune_id(&self, rune: &u128) -> DBResult<RuneId> {
        let cf_handle = self.cf_handle(RUNE_IDS_CF)?;
        let rune_id: RuneId = self
            .get_option_vec_data(&cf_handle, rune.to_le_bytes())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "rune id not found: {}",
                rune
            )))?;

        Ok(rune_id)
    }

    pub fn get_inscription(&self, id: &InscriptionId) -> DBResult<Inscription> {
        let cf_handle = self.cf_handle(INSCRIPTIONS_CF)?;
        let inscription: Inscription = self
            .get_option_vec_data(&cf_handle, id.as_bytes())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "inscription not found: {}",
                id
            )))?;

        Ok(inscription)
    }

    /// Newest-first page of the transactions of `rune_id` in one column family.
    ///
    /// The response `offset` is always `skip + items.len()`: the `skip` to request the next page.
    pub fn get_last_rune_transactions(
        &self,
        rune_id: &RuneId,
        pagination: Option<Pagination>,
        mempool: bool,
    ) -> DBResult<PaginationResponse<SerializedTxid>> {
        let (skip, limit) = pagination.unwrap_or_default().into();
        let (items, _) = self.rune_transactions_page(rune_id, skip, limit, mempool)?;
        let offset = skip.saturating_add(items.len() as u64);
        Ok(PaginationResponse { items, offset })
    }

    /// Returns up to `limit` transactions of `rune_id`, newest first, after skipping the `skip`
    /// newest ones, together with the number of entries actually skipped (less than `skip` only
    /// when the list holds fewer than `skip` entries).
    pub fn rune_transactions_page(
        &self,
        rune_id: &RuneId,
        skip: u64,
        limit: u64,
        mempool: bool,
    ) -> DBResult<(Vec<SerializedTxid>, u64)> {
        let cf_handle = if mempool {
            self.cf_handle(RUNE_TRANSACTIONS_MEMPOOL_CF)?
        } else {
            self.cf_handle(RUNE_TRANSACTIONS_CF)?
        };

        let prefix = rune_transaction_prefix(rune_id);
        let mut iter = self.db.raw_iterator_cf(&cf_handle);

        // Position on the newest entry.
        iter.seek_for_prev(rune_transaction_key(rune_id, u64::MAX));
        let top = match iter.key() {
            Some(key) if key.starts_with(&prefix) => rune_transaction_index(key, prefix.len())
                .ok_or_else(|| RocksDBError::Deserialize("rune transaction key".to_string()))?,
            _ => {
                iter.status()?;
                return Ok((Vec::new(), 0));
            }
        };

        let skipped = if mempool {
            // Mempool lists have holes (transactions leave the mempool in any order), so walk
            // over the entries to skip.
            let mut skipped = 0;
            while skipped < skip && iter.key().is_some_and(|key| key.starts_with(&prefix)) {
                iter.prev();
                skipped += 1;
            }
            skipped
        } else {
            // Confirmed lists are dense: entries are appended with consecutive indices and only
            // ever removed from the top (reorgs), so indices 1..=top are all present.
            let skipped = skip.min(top);
            if skipped > 0 {
                iter.seek_for_prev(rune_transaction_key(rune_id, top - skipped));
            }
            skipped
        };

        let mut items = Vec::new();
        while (items.len() as u64) < limit {
            match (iter.key(), iter.value()) {
                (Some(key), Some(value)) if key.starts_with(&prefix) => {
                    items.push(txid_from_bytes(value)?);
                }
                _ => break,
            }
            iter.prev();
        }
        iter.status()?;

        Ok((items, skipped))
    }

    /// Appends `rune_tx_map` to the rune transaction lists and updates the per-rune counters in
    /// `batch`.
    ///
    /// This replaces the secondary (txid -> rune list positions) index of each txid, so all the
    /// rune transactions of a txid must be included.
    fn put_rune_transactions(
        &self,
        batch: &mut WriteBatch,
        rune_tx_map: &HashMap<RuneId, Vec<SerializedTxid>>,
        mempool: bool,
    ) -> DBResult<()> {
        let (primary_cf, secondary_cf) = self.rune_transaction_cfs(mempool)?;

        // For each txid, the positions it gets in the rune lists.
        let mut sec_index_acc: HashMap<SerializedTxid, Vec<TxRuneIndexRef>> = HashMap::default();

        for (rune_id, txids) in rune_tx_map {
            let last_index_key = rune_index_key(rune_id);
            let mut current_index: u64 = self
                .get_option_vec_data(&primary_cf, &last_index_key)
                .mapped()?
                .unwrap_or(0);

            for txid in txids {
                current_index = current_index.checked_add(1).ok_or(RocksDBError::Overflow)?;

                batch.put_cf(
                    &primary_cf,
                    rune_transaction_key(rune_id, current_index),
                    txid.as_ref(),
                );

                sec_index_acc
                    .entry(*txid)
                    .or_default()
                    .push(TxRuneIndexRef {
                        rune_id: rune_id.to_bytes().to_vec(),
                        index: current_index,
                    });
            }

            batch.put_cf(&primary_cf, last_index_key, current_index.to_le_bytes());
        }

        for (txid, new_refs) in sec_index_acc {
            batch.put_cf(&secondary_cf, txid.as_ref(), new_refs.store());
        }

        Ok(())
    }

    fn rune_transaction_cfs(
        &self,
        mempool: bool,
    ) -> DBResult<(Arc<BoundColumnFamily<'_>>, Arc<BoundColumnFamily<'_>>)> {
        if mempool {
            Ok((
                self.cf_handle(RUNE_TRANSACTIONS_MEMPOOL_CF)?,
                self.cf_handle(TRANSACTION_RUNE_INDEX_MEMPOOL_CF)?,
            ))
        } else {
            Ok((
                self.cf_handle(RUNE_TRANSACTIONS_CF)?,
                self.cf_handle(TRANSACTION_RUNE_INDEX_CF)?,
            ))
        }
    }

    fn get_txs_index_refs(
        &self,
        txids: &[SerializedTxid],
        mempool: bool,
    ) -> DBResult<HashMap<SerializedTxid, Vec<TxRuneIndexRef>>> {
        let cf_handle = if mempool {
            self.cf_handle(TRANSACTION_RUNE_INDEX_MEMPOOL_CF)?
        } else {
            self.cf_handle(TRANSACTION_RUNE_INDEX_CF)?
        };

        let values = self.multi_get_values(&cf_handle, txids.iter().map(|txid| txid.as_ref()))?;

        let mut result = HashMap::default();
        for (txid, value) in txids.iter().zip(values) {
            if let Some(value) = value {
                result.insert(*txid, Vec::<TxRuneIndexRef>::load(&value)?);
            }
        }

        Ok(result)
    }

    /// Removes `txids` from *all* rune lists (in `batch`).
    ///
    /// A list entry is only removed if it still holds the txid (an etching revert may have
    /// cleared the list and the position may have been reused since). When the newest entries
    /// of a list are removed its counter is lowered, so confirmed lists stay dense and new
    /// entries continue right after the remaining ones. Lists of runes in `cleared_runes` are
    /// deleted wholesale by the caller, so their counters are left alone.
    fn delete_rune_transactions(
        &self,
        batch: &mut WriteBatch,
        txids: &[SerializedTxid],
        mempool: bool,
        cleared_runes: &[RuneId],
    ) -> DBResult<()> {
        let idx_refs = self.get_txs_index_refs(txids, mempool)?;
        if idx_refs.is_empty() {
            return Ok(());
        }

        let (primary_cf, secondary_cf) = self.rune_transaction_cfs(mempool)?;

        let mut entries: Vec<(RuneId, u64, SerializedTxid)> = Vec::new();
        for (txid, refs) in &idx_refs {
            for TxRuneIndexRef { rune_id, index } in refs {
                entries.push((rune_id_from_bytes(rune_id)?, *index, *txid));
            }
            batch.delete_cf(&secondary_cf, txid.as_ref());
        }

        let stored = self.multi_get_values(
            &primary_cf,
            entries
                .iter()
                .map(|(rune_id, index, _)| rune_transaction_key(rune_id, *index)),
        )?;

        let mut deleted: HashMap<RuneId, HashSet<u64>> = HashMap::default();
        for ((rune_id, index, txid), stored) in entries.into_iter().zip(stored) {
            if stored.as_deref() == Some(txid.as_ref()) {
                batch.delete_cf(&primary_cf, rune_transaction_key(&rune_id, index));
                deleted.entry(rune_id).or_default().insert(index);
            }
        }

        for (rune_id, deleted_indices) in deleted {
            if cleared_runes.contains(&rune_id) {
                continue;
            }

            let last_index_key = rune_index_key(&rune_id);
            let last_index: u64 = self
                .get_option_vec_data(&primary_cf, &last_index_key)
                .mapped()?
                .unwrap_or(0);
            if !deleted_indices.contains(&last_index) {
                continue;
            }

            // Find the newest entry that survives this batch.
            let prefix = rune_transaction_prefix(&rune_id);
            let mut iter = self.db.raw_iterator_cf(&primary_cf);
            iter.seek_for_prev(rune_transaction_key(&rune_id, last_index));
            let mut new_last_index = 0;
            while let Some(key) = iter.key() {
                if !key.starts_with(&prefix) {
                    break;
                }
                let index = rune_transaction_index(key, prefix.len())
                    .ok_or_else(|| RocksDBError::Deserialize("rune transaction key".to_string()))?;
                if !deleted_indices.contains(&index) {
                    new_last_index = index;
                    break;
                }
                iter.prev();
            }
            iter.status()?;

            if new_last_index == 0 {
                batch.delete_cf(&primary_cf, last_index_key);
            } else {
                batch.put_cf(&primary_cf, last_index_key, new_last_index.to_le_bytes());
            }
        }

        Ok(())
    }

    /// Deletes every transaction of `rune_id` (and its counter) from the rune list in `cf`.
    fn clear_rune_transactions(
        batch: &mut WriteBatch,
        cf: &Arc<BoundColumnFamily>,
        rune_id: &RuneId,
    ) {
        batch.delete_range_cf(
            cf,
            rune_transaction_prefix(rune_id),
            rune_transaction_prefix_end(rune_id),
        );
        batch.delete_cf(cf, rune_index_key(rune_id));
    }

    pub fn get_mempool_txids(&self) -> DBResult<HashMap<SerializedTxid, MempoolEntry>> {
        Ok(self
            .mempool_cache
            .read()
            .map_err(|_| RocksDBError::LockPoisoned)?
            .clone())
    }

    pub fn get_mempool_count(&self) -> DBResult<usize> {
        Ok(self
            .mempool_cache
            .read()
            .map_err(|_| RocksDBError::LockPoisoned)?
            .len())
    }

    pub fn is_tx_in_mempool(&self, txid: &SerializedTxid) -> DBResult<bool> {
        let exists = self
            .mempool_cache
            .read()
            .map_err(|_| RocksDBError::LockPoisoned)?
            .contains_key(txid);

        Ok(exists)
    }

    pub fn get_mempool_entry(&self, txid: &SerializedTxid) -> DBResult<MempoolEntry> {
        let cf_handle = self.cf_handle(MEMPOOL_CF)?;
        self.get_option_vec_data(&cf_handle, txid.as_ref())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "mempool entry not found: {}",
                txid
            )))
    }

    pub fn get_mempool_entries(
        &self,
        txids: &[SerializedTxid],
    ) -> DBResult<HashMap<SerializedTxid, Option<MempoolEntry>>> {
        let cf_handle = self.cf_handle(MEMPOOL_CF)?;
        let values = self.multi_get_values(&cf_handle, txids.iter().map(|txid| txid.as_ref()))?;

        let mut mempool_entries = HashMap::default();
        for (txid, value) in txids.iter().zip(values) {
            let entry = value.map(|bytes| MempoolEntry::load(&bytes)).transpose()?;
            mempool_entries.insert(*txid, entry);
        }

        Ok(mempool_entries)
    }

    pub fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[SerializedTxid],
    ) -> DBResult<HashMap<SerializedTxid, MempoolEntry>> {
        let mut result: HashMap<SerializedTxid, MempoolEntry> = HashMap::default();
        let mut queue: VecDeque<SerializedTxid> = VecDeque::new();
        let mut visited: HashSet<SerializedTxid> = HashSet::default();

        // Initialize the queue with all provided txids
        for txid in txids {
            queue.push_back(*txid);
        }

        while !queue.is_empty() {
            let mut batch_txids: Vec<SerializedTxid> = Vec::new();
            while let Some(current_txid) = queue.pop_front() {
                if visited.contains(&current_txid) {
                    continue;
                }
                batch_txids.push(current_txid);
                visited.insert(current_txid);
            }

            if batch_txids.is_empty() {
                continue; // Nothing new to process in this iteration
            }

            let entries = self.get_mempool_entries(&batch_txids)?;

            for (current_txid, entry_option) in entries {
                if let Some(entry) = entry_option {
                    // Add ancestors to the queue if not already visited
                    for ancestor_txid in &entry.depends {
                        if !visited.contains(ancestor_txid) {
                            queue.push_back(*ancestor_txid);
                        }
                    }
                    // Store the fetched entry
                    result.insert(current_txid, entry);
                }
                // If entry_option is None, it means the txid (or an ancestor)
                // was not found in the mempool CF, likely confirmed or invalid.
                // We simply stop traversing that path.
            }
        }

        Ok(result)
    }

    pub fn _validate_mempool_cache(&self) -> DBResult<()> {
        let db_txids: HashMap<SerializedTxid, MempoolEntry> =
            Self::read_all_mempool_txids(&self.db)?;

        *self
            .mempool_cache
            .write()
            .map_err(|_| RocksDBError::LockPoisoned)? = db_txids;
        Ok(())
    }

    fn read_all_mempool_txids(
        db: &DBWithThreadMode<MultiThreaded>,
    ) -> DBResult<HashMap<SerializedTxid, MempoolEntry>> {
        let mut db_txids: HashMap<SerializedTxid, MempoolEntry> = HashMap::default();

        let cf_handle: Arc<BoundColumnFamily<'_>> = db
            .cf_handle(MEMPOOL_CF)
            .ok_or(RocksDBError::InvalidHandle(MEMPOOL_CF.to_string()))?;

        let iter = db.iterator_cf(&cf_handle, IteratorMode::Start);
        for item in iter {
            let (key, value) = item?;
            if let Ok(txid) = SerializedTxid::try_from(key) {
                db_txids.insert(txid, MempoolEntry::load(&value)?);
            }
        }

        Ok(db_txids)
    }

    pub fn get_script_pubkey_outpoints(
        &self,
        script_pubkey: &ScriptBuf,
        mempool: bool,
    ) -> DBResult<Vec<SerializedOutPoint>> {
        let cf_handle = if mempool {
            self.cf_handle(SCRIPT_PUBKEYS_MEMPOOL_CF)?
        } else {
            self.cf_handle(SCRIPT_PUBKEYS_CF)?
        };

        let search_key = script_pubkey_search_key(script_pubkey);
        let iter = self.db.iterator_cf(
            &cf_handle,
            IteratorMode::From(&search_key, Direction::Forward),
        );

        let mut outpoints = Vec::new();
        for item in iter {
            let (key, _) = item?;
            if !key.starts_with(&search_key) {
                break;
            }

            // Keys of longer scripts that start with `script_pubkey || ':'` share the prefix;
            // only keys of exactly `prefix + outpoint` length belong to this script.
            if let Some(outpoint) = parse_outpoint_from_script_pubkey_key(&key, search_key.len()) {
                outpoints.push(outpoint);
            }
        }

        Ok(outpoints)
    }

    /// One page of an address's outputs, in the same set and order the store's merged
    /// `get_script_pubkey_outpoints(script, None)` returns: confirmed outputs in key order,
    /// then mempool-created ones, leaving out outputs spent in the mempool. Reads only the
    /// keys up to `skip + limit` (plus one spent-check chunk), not the whole set.
    pub fn get_script_pubkey_outpoints_page(
        &self,
        script_pubkey: &ScriptBuf,
        skip: u64,
        limit: u64,
    ) -> DBResult<Vec<SerializedOutPoint>> {
        const CHUNK: usize = 256;

        let mut page = Vec::new();
        if limit == 0 {
            return Ok(page);
        }

        let search_key = script_pubkey_search_key(script_pubkey);
        let mut to_skip = skip;
        let mut chunk = Vec::with_capacity(CHUNK);

        for cf_name in [SCRIPT_PUBKEYS_CF, SCRIPT_PUBKEYS_MEMPOOL_CF] {
            let cf_handle = self.cf_handle(cf_name)?;
            let mut iter = self.db.iterator_cf(
                &cf_handle,
                IteratorMode::From(&search_key, Direction::Forward),
            );

            let mut exhausted = false;
            while !exhausted {
                chunk.clear();
                while chunk.len() < CHUNK {
                    let Some(item) = iter.next() else {
                        exhausted = true;
                        break;
                    };
                    let (key, _) = item?;
                    if !key.starts_with(&search_key) {
                        exhausted = true;
                        break;
                    }
                    if let Some(outpoint) =
                        parse_outpoint_from_script_pubkey_key(&key, search_key.len())
                    {
                        chunk.push(outpoint);
                    }
                }

                if chunk.is_empty() {
                    break;
                }

                let spent = self.get_spent_outpoints_in_mempool(&chunk)?;
                for outpoint in chunk.drain(..) {
                    if matches!(spent.get(&outpoint), Some(Some(_))) {
                        continue;
                    }
                    if to_skip > 0 {
                        to_skip -= 1;
                        continue;
                    }
                    page.push(outpoint);
                    if page.len() as u64 == limit {
                        return Ok(page);
                    }
                }
            }
        }

        Ok(page)
    }

    /// One page of mempool txids in key (txid byte) order.
    pub fn get_mempool_txids_page(&self, skip: u64, limit: u64) -> DBResult<Vec<SerializedTxid>> {
        self.mempool_page(skip, limit, |key, _| txid_from_bytes(key))
    }

    /// One page of mempool entries in key (txid byte) order.
    pub fn get_mempool_entries_page(
        &self,
        skip: u64,
        limit: u64,
    ) -> DBResult<Vec<(SerializedTxid, MempoolEntry)>> {
        self.mempool_page(skip, limit, |key, value| {
            Ok((txid_from_bytes(key)?, MempoolEntry::load(value)?))
        })
    }

    /// Walks MEMPOOL_CF in key order, decoding only the `limit` items after `skip`.
    fn mempool_page<T>(
        &self,
        skip: u64,
        limit: u64,
        decode: impl Fn(&[u8], &[u8]) -> DBResult<T>,
    ) -> DBResult<Vec<T>> {
        let mut page = Vec::new();
        if limit == 0 {
            return Ok(page);
        }

        let cf_handle = self.cf_handle(MEMPOOL_CF)?;
        let mut to_skip = skip;
        for item in self.db.iterator_cf(&cf_handle, IteratorMode::Start) {
            let (key, value) = item?;
            if to_skip > 0 {
                to_skip -= 1;
                continue;
            }
            page.push(decode(&key, &value)?);
            if page.len() as u64 == limit {
                break;
            }
        }

        Ok(page)
    }

    pub fn get_spent_outpoints_in_mempool(
        &self,
        outpoints: &[SerializedOutPoint],
    ) -> DBResult<HashMap<SerializedOutPoint, Option<SpenderReference>>> {
        let cf_handle = self.cf_handle(SPENT_OUTPOINTS_MEMPOOL_CF)?;
        let values = self.multi_get_values(&cf_handle, outpoints.iter().map(|o| o.as_ref()))?;

        let mut spent_outpoints = HashMap::default();
        for (outpoint, value) in outpoints.iter().zip(values) {
            let spender = value
                .map(|bytes| SpenderReference::load(&bytes))
                .transpose()?;
            spent_outpoints.insert(*outpoint, spender);
        }

        Ok(spent_outpoints)
    }

    pub fn get_transaction_raw(&self, txid: &SerializedTxid, mempool: bool) -> DBResult<Vec<u8>> {
        let cf_handle = if mempool {
            self.cf_handle(TRANSACTIONS_MEMPOOL_CF)?
        } else {
            self.cf_handle(TRANSACTIONS_CF)?
        };

        self.get_option_vec_data(&cf_handle, txid.as_ref())
            .transpose()
            .ok_or(RocksDBError::NotFound(format!(
                "transaction not found: {}",
                txid
            )))?
    }

    pub fn get_transaction(&self, txid: &SerializedTxid, mempool: bool) -> DBResult<Transaction> {
        let cf_handle = if mempool {
            self.cf_handle(TRANSACTIONS_MEMPOOL_CF)?
        } else {
            self.cf_handle(TRANSACTIONS_CF)?
        };

        let transaction = self
            .get_option_vec_data(&cf_handle, txid.as_ref())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "transaction not found: {}",
                txid
            )))?;

        Ok(transaction)
    }

    pub fn partition_transactions_by_existence<'a, I>(
        &self,
        txids: I,
    ) -> DBResult<(Vec<SerializedTxid>, Vec<SerializedTxid>)>
    where
        I: IntoIterator<Item = &'a SerializedTxid>,
    {
        let (mut exists, mut not_exists): (Vec<SerializedTxid>, Vec<SerializedTxid>) = {
            let mempool = self
                .mempool_cache
                .read()
                .map_err(|_| RocksDBError::LockPoisoned)?;

            let (in_mempool, not_in_mempool) = txids
                .into_iter()
                .partition(|txid| mempool.contains_key(*txid));

            (in_mempool, not_in_mempool)
        };

        let cf_handle = self.cf_handle(TRANSACTIONS_CF)?;
        let keys: Vec<_> = not_exists
            .iter()
            .map(|txid| (&cf_handle, txid.as_ref()))
            .collect();

        let results = self.db.multi_get_cf(keys);

        let mut to_remove = Vec::new();
        for (i, result) in results.iter().enumerate() {
            match result {
                Ok(Some(_)) => {
                    exists.push(not_exists[i]);
                    to_remove.push(i);
                }
                Ok(None) => {}
                Err(e) => return Err(e.clone().into()),
            }
        }

        for i in to_remove.iter().rev() {
            not_exists.remove(*i);
        }

        Ok((exists, not_exists))
    }

    pub fn get_transaction_confirming_block(&self, txid: &SerializedTxid) -> DBResult<BlockId> {
        let cf_handle = self.cf_handle(TRANSACTION_CONFIRMING_BLOCK_CF)?;
        self.get_option_vec_data(&cf_handle, txid.as_ref())
            .mapped()?
            .ok_or(RocksDBError::NotFound(format!(
                "transaction confirming block not found: {}",
                txid
            )))
    }

    pub fn get_transaction_confirming_blocks(
        &self,
        txids: &[SerializedTxid],
    ) -> DBResult<HashMap<SerializedTxid, Option<BlockId>>> {
        let cf_handle = self.cf_handle(TRANSACTION_CONFIRMING_BLOCK_CF)?;
        let values = self.multi_get_values(&cf_handle, txids.iter().map(|txid| txid.as_ref()))?;

        let mut confirming_blocks = HashMap::default();
        for (txid, value) in txids.iter().zip(values) {
            let block_id = value.map(|bytes| BlockId::load(&bytes)).transpose()?;
            confirming_blocks.insert(*txid, block_id);
        }

        Ok(confirming_blocks)
    }

    pub fn batch_update(&self, update: &BatchUpdate, mempool: bool) -> DBResult<()> {
        let mut batch = WriteBatch::default();

        // 1. Update blocks
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(BLOCKS_CF)?;
            for (block_hash, block) in update.blocks.iter() {
                batch.put_cf(
                    &cf_handle,
                    block_hash.as_raw_hash().to_byte_array(),
                    block.store_ref(),
                );
            }
        }

        // 2. Update block_hashes
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(BLOCK_HEIGHT_TO_HASH_CF)?;
            for (block_height, block_hash) in update.block_hashes.iter() {
                batch.put_cf(
                    &cf_handle,
                    block_height.to_le_bytes(),
                    block_hash.as_raw_hash().to_byte_array(),
                );
            }
        }

        // 3. Update txouts
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(OUTPOINTS_MEMPOOL_CF)?
            } else {
                self.cf_handle(OUTPOINTS_CF)?
            };

            for (outpoint, txout) in update.txouts.iter() {
                batch.put_cf(&cf_handle, outpoint, txout.store_ref());
            }
        }

        // 4. Update tx_state_changes
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(TRANSACTIONS_STATE_CHANGE_MEMPOOL_CF)?
            } else {
                self.cf_handle(TRANSACTIONS_STATE_CHANGE_CF)?
            };

            for (txid, tx_state_change) in update.tx_state_changes.iter() {
                batch.put_cf(&cf_handle, txid.as_ref(), tx_state_change.store_ref());
            }
        }

        // 5. Update runes
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNES_CF)?;

            for (rune_id, rune) in update.runes.iter() {
                batch.put_cf(&cf_handle, rune_id.to_bytes(), rune.store_ref());
            }
        }

        // 6. Update rune_ids
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNE_IDS_CF)?;

            for (rune, rune_id) in update.rune_ids.iter() {
                batch.put_cf(&cf_handle, rune.to_le_bytes(), rune_id.store_ref());
            }
        }

        // 7. Update rune_numbers
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNE_NUMBER_CF)?;

            for (number, rune_id) in update.rune_numbers.iter() {
                batch.put_cf(&cf_handle, number.to_le_bytes(), rune_id.store_ref());
            }
        }

        // 8. Update inscriptions
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(INSCRIPTIONS_CF)?;

            for (inscription_id, inscription) in update.inscriptions.iter() {
                batch.put_cf(
                    &cf_handle,
                    inscription_id.as_bytes(),
                    inscription.store_ref(),
                );
            }
        }

        // 9. Update mempool_txs (the in-memory cache is updated after the write succeeds)
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(MEMPOOL_CF)?;
            for (txid, mempool_entry) in update.mempool_txs.iter() {
                batch.put_cf(&cf_handle, txid.as_ref(), mempool_entry.store_ref());
            }
        }

        // 10. Update runes_count
        if !mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(STATS_CF)?;
            batch.put_cf(&cf_handle, RUNES_COUNT_KEY, update.rune_count.to_le_bytes());
        }

        // 11. Update block_count
        if !mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(STATS_CF)?;
            batch.put_cf(
                &cf_handle,
                BLOCK_COUNT_KEY,
                update.block_count.to_le_bytes(),
            );
        }

        // 12. Update purged_blocks_count
        if !mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(STATS_CF)?;
            batch.put_cf(
                &cf_handle,
                PURGED_BLOCKS_COUNT_KEY,
                update.purged_blocks_count.to_le_bytes(),
            );
        }

        // 13. Update addresses
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(SCRIPT_PUBKEYS_MEMPOOL_CF)?
            } else {
                self.cf_handle(SCRIPT_PUBKEYS_CF)?
            };

            for (script_pubkey, (new_ops, spent_ops)) in update.script_pubkeys.iter() {
                for outpoint in new_ops.iter() {
                    batch.put_cf(
                        &cf_handle,
                        script_pubkey_outpoint_to_bytes(script_pubkey, outpoint),
                        vec![1],
                    );
                }

                for outpoint in spent_ops.iter() {
                    batch.delete_cf(
                        &cf_handle,
                        script_pubkey_outpoint_to_bytes(script_pubkey, outpoint),
                    );
                }
            }
        }

        // 14. Update address_outpoints
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(OUTPOINT_TO_SCRIPT_PUBKEY_MEMPOOL_CF)?
            } else {
                self.cf_handle(OUTPOINT_TO_SCRIPT_PUBKEY_CF)?
            };

            for (outpoint, script_pubkey) in update.script_pubkeys_outpoints.iter() {
                batch.put_cf(&cf_handle, outpoint, script_pubkey);
            }
        }

        // 15. Update spent_outpoints_in_mempool
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> =
                self.cf_handle(SPENT_OUTPOINTS_MEMPOOL_CF)?;
            for (outpoint, spender_reference) in update.spent_outpoints_in_mempool.iter() {
                batch.put_cf(&cf_handle, outpoint.as_ref(), spender_reference.store_ref());
            }
        }

        // 16. Update transactions
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(TRANSACTIONS_MEMPOOL_CF)?
            } else {
                self.cf_handle(TRANSACTIONS_CF)?
            };

            for (txid, transaction) in update.transactions.iter() {
                batch.put_cf(&cf_handle, txid.as_ref(), consensus::serialize(transaction));
            }
        }

        // 17. Update transaction_confirming_block
        if !mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> =
                self.cf_handle(TRANSACTION_CONFIRMING_BLOCK_CF)?;

            for (txid, block_id) in update.transaction_confirming_block.iter() {
                batch.put_cf(&cf_handle, txid.as_ref(), block_id.store_ref());
            }
        }

        // 18. Update rune_transactions (lists and their counters)
        if !update.rune_transactions.is_empty() {
            self.put_rune_transactions(&mut batch, &update.rune_transactions, mempool)?;
        }

        // Publish new mempool entries to the in-memory cache only once they are persisted. The
        // cache lock is held across the write so concurrent writers apply their cache changes in
        // the same order as their DB writes.
        let mut mempool_cache = if update.mempool_txs.is_empty() {
            None
        } else {
            Some(
                self.mempool_cache
                    .write()
                    .map_err(|_| RocksDBError::LockPoisoned)?,
            )
        };

        self.db.write(batch)?;

        if let Some(mempool_cache) = mempool_cache.as_mut() {
            for (txid, mempool_entry) in update.mempool_txs.iter() {
                mempool_cache.insert(*txid, mempool_entry.clone());
            }
        }

        Ok(())
    }

    pub fn batch_delete(&self, delete: &BatchDelete) -> DBResult<()> {
        let mut batch = WriteBatch::default();

        // 1. Delete blocks
        {
            let cf_handle = self.cf_handle(OUTPOINTS_CF)?;
            let cf_handle_mempool = self.cf_handle(OUTPOINTS_MEMPOOL_CF)?;

            for txout in delete.tx_outs.iter() {
                batch.delete_cf(&cf_handle, txout);
                batch.delete_cf(&cf_handle_mempool, txout);
            }
        }

        // 2. Delete tx_state_changes
        {
            let cf_handle = self.cf_handle(TRANSACTIONS_STATE_CHANGE_CF)?;
            let cf_handle_mempool = self.cf_handle(TRANSACTIONS_STATE_CHANGE_MEMPOOL_CF)?;

            for txid in delete.tx_state_changes.iter() {
                batch.delete_cf(&cf_handle, txid.as_ref());
                batch.delete_cf(&cf_handle_mempool, txid.as_ref());
            }
        }

        // 3. Delete script_pubkeys_outpoints
        {
            let cf_handle = self.cf_handle(OUTPOINT_TO_SCRIPT_PUBKEY_CF)?;
            let cf_handle_mempool = self.cf_handle(OUTPOINT_TO_SCRIPT_PUBKEY_MEMPOOL_CF)?;

            for outpoint in delete.script_pubkeys_outpoints.iter() {
                batch.delete_cf(&cf_handle, outpoint.previous_outpoint.as_ref());
                batch.delete_cf(&cf_handle_mempool, outpoint.previous_outpoint.as_ref());
            }
        }

        // 4. Delete script_pubkeys_outpoints
        {
            let cf_handle = self.cf_handle(SCRIPT_PUBKEYS_CF)?;
            let cf_handle_mempool = self.cf_handle(SCRIPT_PUBKEYS_MEMPOOL_CF)?;

            for outpoint in delete.script_pubkeys_outpoints.iter() {
                if let Some(script_pubkey) = &outpoint.script_pubkey {
                    batch.delete_cf(
                        &cf_handle,
                        script_pubkey_outpoint_to_bytes(script_pubkey, &outpoint.previous_outpoint),
                    );
                    batch.delete_cf(
                        &cf_handle_mempool,
                        script_pubkey_outpoint_to_bytes(script_pubkey, &outpoint.previous_outpoint),
                    );
                }
            }
        }

        // 5. Delete spent_outpoints_in_mempool
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> =
                self.cf_handle(SPENT_OUTPOINTS_MEMPOOL_CF)?;
            for outpoint in delete.spent_outpoints_in_mempool.iter() {
                batch.delete_cf(&cf_handle, outpoint);
            }
        }

        self.db.write(batch)?;
        Ok(())
    }

    pub fn batch_rollback(&self, rollback: &BatchRollback, mempool: bool) -> DBResult<()> {
        let mut batch = WriteBatch::default();

        // 1. Update runes count (only block reverts change it; a mempool rollback must not
        //    overwrite the count with the value it read when it started)
        if !mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(STATS_CF)?;
            batch.put_cf(
                &cf_handle,
                RUNES_COUNT_KEY,
                rollback.runes_count.to_le_bytes(),
            );
        }

        // 1b. Reverted blocks and the new block count, atomically with the reverted txs
        if !rollback.blocks_to_delete.is_empty() {
            let blocks_cf = self.cf_handle(BLOCKS_CF)?;
            let block_height_to_hash_cf = self.cf_handle(BLOCK_HEIGHT_TO_HASH_CF)?;
            for (height, hash) in rollback.blocks_to_delete.iter() {
                batch.delete_cf(&blocks_cf, hash.as_raw_hash().to_byte_array());
                batch.delete_cf(&block_height_to_hash_cf, height.to_le_bytes());
            }
        }
        if let Some(block_count) = rollback.block_count {
            let cf_handle = self.cf_handle(STATS_CF)?;
            batch.put_cf(&cf_handle, BLOCK_COUNT_KEY, block_count.to_le_bytes());
        }

        // 2. Update rune_entry
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNES_CF)?;
            for (rune_id, rune_entry) in rollback.rune_entry.iter() {
                batch.put_cf(&cf_handle, rune_id.to_bytes(), rune_entry.store_ref());
            }
        }

        // 3. Update txouts
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(OUTPOINTS_MEMPOOL_CF)?
            } else {
                self.cf_handle(OUTPOINTS_CF)?
            };

            for (outpoint, txout) in rollback.txouts.iter() {
                batch.put_cf(&cf_handle, outpoint, txout.store_ref());
            }
        }

        // 4. Update script_pubkey_entry
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(SCRIPT_PUBKEYS_MEMPOOL_CF)?
            } else {
                self.cf_handle(SCRIPT_PUBKEYS_CF)?
            };

            for (script_pubkey, (new_ops, spent_ops)) in rollback.script_pubkey_entry.iter() {
                for outpoint in new_ops.iter() {
                    batch.put_cf(
                        &cf_handle,
                        script_pubkey_outpoint_to_bytes(script_pubkey, outpoint),
                        vec![1],
                    );
                }

                for outpoint in spent_ops.iter() {
                    batch.delete_cf(
                        &cf_handle,
                        script_pubkey_outpoint_to_bytes(script_pubkey, outpoint),
                    );
                }
            }
        }

        // 5. Update outpoints_to_delete
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(OUTPOINTS_MEMPOOL_CF)?
            } else {
                self.cf_handle(OUTPOINTS_CF)?
            };

            for outpoint in rollback.outpoints_to_delete.iter() {
                batch.delete_cf(&cf_handle, outpoint);
            }
        }

        // 6. Script pubkey outpoints
        {
            let cf_handle = if mempool {
                self.cf_handle(OUTPOINT_TO_SCRIPT_PUBKEY_MEMPOOL_CF)?
            } else {
                self.cf_handle(OUTPOINT_TO_SCRIPT_PUBKEY_CF)?
            };

            for outpoint in rollback.outpoints_to_delete.iter() {
                batch.delete_cf(&cf_handle, outpoint);
            }
        }

        // 7. Update prev_outpoints_to_delete
        {
            let cf_handle = self.cf_handle(SPENT_OUTPOINTS_MEMPOOL_CF)?;

            for outpoint in rollback.prev_outpoints_to_delete.iter() {
                batch.delete_cf(&cf_handle, outpoint);
            }
        }

        // 8. Update runes_to_delete
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNES_CF)?;
            for rune_id in rollback.runes_to_delete.iter() {
                batch.delete_cf(&cf_handle, rune_id.to_bytes());
            }
        }

        // 9. Update runes_ids_to_delete
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNE_IDS_CF)?;
            for rune in rollback.runes_ids_to_delete.iter() {
                batch.delete_cf(&cf_handle, rune.0.to_le_bytes());
            }
        }

        // 10. Update rune_numbers_to_delete
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNE_NUMBER_CF)?;
            for number in rollback.rune_numbers_to_delete.iter() {
                batch.delete_cf(&cf_handle, number.to_le_bytes());
            }
        }

        // 11. Update inscriptions_to_delete
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(INSCRIPTIONS_CF)?;
            for inscription_id in rollback.inscriptions_to_delete.iter() {
                batch.delete_cf(&cf_handle, inscription_id.as_bytes());
            }
        }

        // 14. Update txs_to_delete
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(TRANSACTIONS_MEMPOOL_CF)?
            } else {
                self.cf_handle(TRANSACTIONS_CF)?
            };

            for txid in rollback.txs_to_delete.iter() {
                batch.delete_cf(&cf_handle, txid.as_ref());
            }
        }

        // 15. Update tx state changes
        {
            let cf_handle: Arc<BoundColumnFamily<'_>> = if mempool {
                self.cf_handle(TRANSACTIONS_STATE_CHANGE_MEMPOOL_CF)?
            } else {
                self.cf_handle(TRANSACTIONS_STATE_CHANGE_CF)?
            };

            for txid in rollback.txs_to_delete.iter() {
                batch.delete_cf(&cf_handle, txid.as_ref());
            }
        }

        // 16. Update tx confirming block
        if !mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> =
                self.cf_handle(TRANSACTION_CONFIRMING_BLOCK_CF)?;
            for txid in rollback.txs_to_delete.iter() {
                batch.delete_cf(&cf_handle, txid.as_ref());
            }
        }

        // 17. Remove mempool txs (the in-memory cache is updated after the write succeeds)
        if mempool {
            let cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(MEMPOOL_CF)?;
            for txid in rollback.txs_to_delete.iter() {
                batch.delete_cf(&cf_handle, txid.as_ref());
            }
        }

        // 18. Remove the reverted txs from the rune transaction lists
        self.delete_rune_transactions(
            &mut batch,
            &rollback.txs_to_delete,
            mempool,
            &rollback.delete_all_rune_transactions,
        )?;

        // 19. Reverted etchings: drop the whole rune transaction list (and its counter) in both
        // the block and the mempool column families.
        if !rollback.delete_all_rune_transactions.is_empty() {
            let cf_handle = self.cf_handle(RUNE_TRANSACTIONS_CF)?;
            let cf_handle_mempool = self.cf_handle(RUNE_TRANSACTIONS_MEMPOOL_CF)?;
            for rune_id in rollback.delete_all_rune_transactions.iter() {
                Self::clear_rune_transactions(&mut batch, &cf_handle, rune_id);
                Self::clear_rune_transactions(&mut batch, &cf_handle_mempool, rune_id);
            }
        }

        // 20. Renumber the runes etched after the reverted ones
        self.renumber_runes_after_revert(&mut batch, rollback)?;

        let mut mempool_cache = if mempool && !rollback.txs_to_delete.is_empty() {
            Some(
                self.mempool_cache
                    .write()
                    .map_err(|_| RocksDBError::LockPoisoned)?,
            )
        } else {
            None
        };

        self.db.write(batch)?;

        if let Some(mempool_cache) = mempool_cache.as_mut() {
            for txid in rollback.txs_to_delete.iter() {
                mempool_cache.remove(txid);
            }
        }

        Ok(())
    }

    /// Closes the gaps left in the rune numbering by `rollback.rune_numbers_to_delete` (in
    /// `batch`): every later rune moves down, and the now unused highest numbers are removed.
    fn renumber_runes_after_revert(
        &self,
        batch: &mut WriteBatch,
        rollback: &BatchRollback,
    ) -> DBResult<()> {
        if rollback.rune_numbers_to_delete.is_empty() {
            return Ok(());
        }

        let mut rune_numbers_deleted = rollback.rune_numbers_to_delete.clone();
        rune_numbers_deleted.sort_unstable();
        rune_numbers_deleted.dedup();

        let total_runes = rollback.runes_count + rune_numbers_deleted.len() as u64;

        // Now we have to fill the gaps in the rune numbers.
        let start = rune_numbers_deleted[0] + 1;
        let mut to_subtract = 1;
        let mut rune_numbers_to_update = HashMap::default();
        for i in start..total_runes {
            if rune_numbers_deleted.binary_search(&i).is_ok() {
                to_subtract += 1;
                continue;
            }

            rune_numbers_to_update.insert(i, i - to_subtract);
        }

        let rune_ids_to_update = self
            .get_rune_ids_by_numbers(&rune_numbers_to_update.keys().cloned().collect::<Vec<_>>())?;

        // The entries written by this rollback take precedence over the stored ones.
        let missing: Vec<RuneId> = rune_ids_to_update
            .values()
            .filter(|rune_id| !rollback.rune_entry.contains_key(rune_id))
            .cloned()
            .collect();
        let mut rune_entries_to_update = self.get_runes_by_ids(&missing)?;
        for rune_id in rune_ids_to_update.values() {
            if let Some(entry) = rollback.rune_entry.get(rune_id) {
                rune_entries_to_update.insert(*rune_id, entry.clone());
            }
        }

        let runes_cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNES_CF)?;
        let rune_number_cf_handle: Arc<BoundColumnFamily<'_>> = self.cf_handle(RUNE_NUMBER_CF)?;

        for (number, rune_id) in rune_ids_to_update {
            let rune_entry = rune_entries_to_update
                .get_mut(&rune_id)
                .ok_or_else(|| RocksDBError::NotFound(format!("rune not found: {}", rune_id)))?;
            let new_number = rune_numbers_to_update[&number];
            rune_entry.number = new_number;
            batch.put_cf(&runes_cf_handle, rune_id.to_bytes(), rune_entry.store_ref());

            batch.put_cf(
                &rune_number_cf_handle,
                new_number.to_le_bytes(),
                rune_id.store_ref(),
            );
        }

        for number in rollback.runes_count..total_runes {
            batch.delete_cf(&rune_number_cf_handle, number.to_le_bytes());
        }

        Ok(())
    }

    pub fn set_subscription(&self, sub: &Subscription) -> DBResult<()> {
        let cf_handle = self.cf_handle(SUBSCRIPTIONS_CF)?;
        self.db
            .put_cf(&cf_handle, sub.id.as_bytes(), sub.store_ref())?;
        Ok(())
    }

    pub fn get_subscription(&self, id: &Uuid) -> DBResult<Subscription> {
        let cf_handle = self.cf_handle(SUBSCRIPTIONS_CF)?;
        let key = id.as_bytes();
        let data =
            self.get_option_vec_data(&cf_handle, key)
                .mapped()?
                .ok_or(RocksDBError::NotFound(format!(
                    "Subscription not found: {}",
                    id
                )))?;
        Ok(data)
    }

    pub fn get_subscriptions(&self) -> DBResult<Vec<Subscription>> {
        let cf_handle = self.cf_handle(SUBSCRIPTIONS_CF)?;
        let iter = self.db.iterator_cf(&cf_handle, IteratorMode::Start);
        let mut subs = Vec::new();
        for item in iter {
            let (_key, value) = item?;
            subs.push(Subscription::load(&value)?);
        }

        Ok(subs)
    }

    pub fn delete_subscription(&self, id: &Uuid) -> DBResult<()> {
        let cf_handle = self.cf_handle(SUBSCRIPTIONS_CF)?;
        let key = id.as_bytes();
        self.db.delete_cf(&cf_handle, key)?;
        Ok(())
    }

    pub fn update_subscription_last_success(
        &self,
        subscription_id: &Uuid,
        new_time_secs: u64,
    ) -> DBResult<()> {
        let mut sub = self.get_subscription(subscription_id)?;
        sub.last_success_epoch_secs = new_time_secs;
        self.set_subscription(&sub)
    }

    /// Persists everything written so far: syncs the WAL and flushes the memtables of every
    /// column family to SST files.
    pub fn flush(&self) -> DBResult<()> {
        self.db.flush_wal(true)?;

        let handles = COLUMN_FAMILIES
            .iter()
            .map(|name| self.cf_handle(name))
            .collect::<DBResult<Vec<_>>>()?;
        let handle_refs: Vec<&Arc<BoundColumnFamily>> = handles.iter().collect();
        self.db
            .flush_cfs_opt(&handle_refs, &FlushOptions::default())?;
        Ok(())
    }

    pub fn close(self) -> DBResult<()> {
        // 1. Explicitly flush any pending writes
        self.flush()?;

        // 2. Because we're consuming `self`, the DB reference is dropped
        //    at the end of this function.
        //    That will release RocksDB file handles, locks, etc.
        Ok(())
    }
}

fn txid_from_bytes(bytes: &[u8]) -> DBResult<SerializedTxid> {
    <[u8; 32]>::try_from(bytes)
        .map(SerializedTxid::from)
        .map_err(|_| RocksDBError::InvalidTxid)
}

fn rune_id_from_bytes(bytes: &[u8]) -> DBResult<RuneId> {
    if bytes.len() != 12 {
        return Err(RocksDBError::InvalidRuneId);
    }
    let block = u64::from_le_bytes(
        bytes[..8]
            .try_into()
            .map_err(|_| RocksDBError::InvalidRuneId)?,
    );
    let tx = u32::from_le_bytes(
        bytes[8..]
            .try_into()
            .map_err(|_| RocksDBError::InvalidRuneId)?,
    );
    Ok(RuneId::new(block, tx))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::db::testing::{add_rune_txs, remove_txs, txid, TempDb},
    };

    fn page(
        db: &RocksDB,
        rune_id: RuneId,
        skip: u64,
        limit: u64,
        mempool: bool,
    ) -> PaginationResponse<SerializedTxid> {
        db.get_last_rune_transactions(&rune_id, Some(Pagination { skip, limit }), mempool)
            .unwrap()
    }

    /// Follows `offset` from `skip = 0` until an empty page and returns every item.
    fn all_pages(db: &RocksDB, rune_id: RuneId, limit: u64, mempool: bool) -> Vec<SerializedTxid> {
        let mut items = Vec::new();
        let mut skip = 0;
        loop {
            let page = page(db, rune_id, skip, limit, mempool);
            assert!(page.items.len() as u64 <= limit);
            assert_eq!(page.offset, skip + page.items.len() as u64);
            if page.items.is_empty() {
                return items;
            }
            skip = page.offset;
            items.extend(page.items);
        }
    }

    #[test]
    fn rune_transactions_paginate_newest_first_past_256_entries() {
        let db = TempDb::new();
        let rune_id = RuneId::new(840_000, 1);
        let before = RuneId::new(840_000, 0);
        let after = RuneId::new(840_000, 2);

        // Several writes, so the counter carries over between batches; neighbouring runes
        // share most of the key prefix.
        for chunk in (1..=600u64).collect::<Vec<_>>().chunks(200) {
            add_rune_txs(
                &db,
                &[
                    (rune_id, chunk.iter().map(|n| txid(*n)).collect()),
                    (before, vec![txid(10_000 + chunk[0])]),
                    (after, vec![txid(20_000 + chunk[0])]),
                ],
                false,
            );
        }

        let expected: Vec<_> = (1..=600).rev().map(txid).collect();
        assert_eq!(all_pages(&db, rune_id, 50, false), expected);
        assert_eq!(all_pages(&db, rune_id, 7, false), expected);

        let tail = page(&db, rune_id, 590, 50, false);
        assert_eq!(tail.items, expected[590..]);
        assert_eq!(tail.offset, 600);

        let middle = page(&db, rune_id, 255, 3, false);
        assert_eq!(middle.items, vec![txid(345), txid(344), txid(343)]);

        let empty = page(&db, rune_id, 10, 0, false);
        assert!(empty.items.is_empty());
        assert_eq!(empty.offset, 10);

        let past_end = page(&db, rune_id, 1_000, 50, false);
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.offset, 1_000);

        assert_eq!(
            all_pages(&db, after, 50, false),
            vec![txid(20_401), txid(20_201), txid(20_001)]
        );
        assert!(all_pages(&db, rune_id, 50, true).is_empty());
    }

    #[test]
    fn mempool_rune_transactions_skip_holes() {
        let db = TempDb::new();
        let rune_id = RuneId::new(840_000, 1);

        add_rune_txs(&db, &[(rune_id, (1..=300).map(txid).collect())], true);

        let removed: Vec<u64> = (1..=300).filter(|n| n % 3 == 0 || *n == 299).collect();
        remove_txs(
            &db,
            &removed.iter().map(|n| txid(*n)).collect::<Vec<_>>(),
            true,
            vec![],
        );

        let mut expected: Vec<_> = (1..=300)
            .rev()
            .filter(|n| !removed.contains(n))
            .map(txid)
            .collect();
        assert_eq!(all_pages(&db, rune_id, 50, true), expected);
        assert_eq!(page(&db, rune_id, 1, 2, true).items, expected[1..3]);

        // New entries still come first after the newest ones were removed.
        add_rune_txs(&db, &[(rune_id, vec![txid(301)])], true);
        expected.insert(0, txid(301));
        assert_eq!(all_pages(&db, rune_id, 50, true), expected);
    }

    #[test]
    fn rollback_removes_rune_transactions() {
        let db = TempDb::new();
        let rune_id = RuneId::new(840_000, 1);
        let other = RuneId::new(840_000, 2);

        add_rune_txs(&db, &[(rune_id, (1..=5).map(txid).collect())], false);
        add_rune_txs(
            &db,
            &[(rune_id, vec![txid(6)]), (other, vec![txid(6)])],
            false,
        );

        // Reverting the block holding txs 5 and 6 removes them from every list.
        remove_txs(&db, &[txid(5), txid(6)], false, vec![]);
        assert_eq!(
            all_pages(&db, rune_id, 50, false),
            (1..=4).rev().map(txid).collect::<Vec<_>>()
        );
        assert!(all_pages(&db, other, 50, false).is_empty());

        // The list stays dense: new entries continue right after the remaining ones.
        add_rune_txs(&db, &[(rune_id, vec![txid(7)])], false);
        assert_eq!(
            all_pages(&db, rune_id, 50, false),
            vec![txid(7), txid(4), txid(3), txid(2), txid(1)]
        );
        assert_eq!(page(&db, rune_id, 1, 1, false).items, vec![txid(4)]);

        // Reverting the etching clears the rune's lists in both column families.
        add_rune_txs(&db, &[(rune_id, vec![txid(100)])], true);
        remove_txs(&db, &[txid(7)], false, vec![rune_id]);
        assert!(all_pages(&db, rune_id, 50, false).is_empty());
        assert!(all_pages(&db, rune_id, 50, true).is_empty());

        // Counters restart, and the stale mempool reference of tx 100 must not remove the
        // entry that reuses its position.
        add_rune_txs(&db, &[(rune_id, vec![txid(200)])], true);
        remove_txs(&db, &[txid(100)], true, vec![]);
        assert_eq!(all_pages(&db, rune_id, 50, true), vec![txid(200)]);

        add_rune_txs(&db, &[(rune_id, vec![txid(300)])], false);
        assert_eq!(all_pages(&db, rune_id, 50, false), vec![txid(300)]);
        assert!(page(&db, rune_id, 1, 50, false).items.is_empty());
    }

    #[test]
    fn script_pubkey_outpoints_match_the_script_exactly() {
        let db = TempDb::new();
        let script = ScriptBuf::from_bytes(vec![0x51]);
        let extended = ScriptBuf::from_bytes(vec![0x51, b':', 0x52]);
        let outpoint = SerializedOutPoint::new(&[1u8; 32], 0);
        let extended_outpoint = SerializedOutPoint::new(&[2u8; 32], 1);

        let mut update = BatchUpdate::new(0, 0, 0);
        update
            .script_pubkeys
            .insert(script.clone(), (vec![outpoint], vec![]));
        update
            .script_pubkeys
            .insert(extended.clone(), (vec![extended_outpoint], vec![]));
        db.batch_update(&update, false).unwrap();

        assert_eq!(
            db.get_script_pubkey_outpoints(&script, false).unwrap(),
            vec![outpoint]
        );
        assert_eq!(
            db.get_script_pubkey_outpoints(&extended, false).unwrap(),
            vec![extended_outpoint]
        );
    }

    #[test]
    fn writes_survive_reopen_with_and_without_flush() {
        let mut db = TempDb::new();
        let rune_id = RuneId::new(840_000, 1);

        let mut update = BatchUpdate::new(0, 10, 0);
        update.rune_transactions.insert(rune_id, vec![txid(1)]);
        db.batch_update(&update, false).unwrap();
        db.switch_to_online_mode().unwrap();

        // Written after the last flush: only the WAL has it.
        let mut update = BatchUpdate::new(0, 11, 0);
        update.rune_transactions.insert(rune_id, vec![txid(2)]);
        db.batch_update(&update, false).unwrap();

        db.reopen(false);
        assert_eq!(db.get_block_count().unwrap(), 11);
        assert_eq!(all_pages(&db, rune_id, 50, false), vec![txid(2), txid(1)]);

        db.reopen(true);
        assert_eq!(db.get_block_count().unwrap(), 11);
        assert_eq!(all_pages(&db, rune_id, 50, false), vec![txid(2), txid(1)]);
    }

    #[test]
    fn rollback_reverts_block_and_block_count_together() {
        let db = TempDb::new();
        let hash = BlockHash::from_byte_array([5u8; 32]);

        let mut update = BatchUpdate::new(0, 6, 0);
        update.block_hashes.insert(5, hash);
        update
            .rune_transactions
            .insert(RuneId::new(5, 1), vec![txid(1)]);
        db.batch_update(&update, false).unwrap();
        assert_eq!(db.get_block_hash(5).unwrap(), hash);

        let mut rollback = BatchRollback::new(0);
        rollback.txs_to_delete = vec![txid(1)];
        rollback.blocks_to_delete = vec![(5, hash)];
        rollback.block_count = Some(5);
        db.batch_rollback(&rollback, false).unwrap();

        assert_eq!(db.get_block_count().unwrap(), 5);
        assert!(matches!(
            db.get_block_hash(5),
            Err(RocksDBError::NotFound(_))
        ));
        assert!(all_pages(&db, RuneId::new(5, 1), 50, false).is_empty());
    }
}
