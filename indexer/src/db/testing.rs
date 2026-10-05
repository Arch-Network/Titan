use {
    super::RocksDB,
    crate::models::{BatchRollback, BatchUpdate},
    arch_titan_types_core::{RuneId, SerializedTxid},
    std::{ops::Deref, path::PathBuf},
    uuid::Uuid,
};

/// A distinct txid per `n`.
pub(crate) fn txid(n: u64) -> SerializedTxid {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    bytes[31] = 0xab;
    SerializedTxid::from(bytes)
}

/// Appends the txids to the rune transaction lists, like an indexed block or mempool batch.
pub(crate) fn add_rune_txs(
    db: &RocksDB,
    rune_txs: &[(RuneId, Vec<SerializedTxid>)],
    mempool: bool,
) {
    let mut update = BatchUpdate::new(0, 0, 0);
    update.rune_transactions = rune_txs.iter().cloned().collect();
    db.batch_update(&update, mempool).unwrap();
}

/// Rolls back `txids`, like a reverted block (or txs leaving the mempool).
pub(crate) fn remove_txs(
    db: &RocksDB,
    txids: &[SerializedTxid],
    mempool: bool,
    reverted_etchings: Vec<RuneId>,
) {
    let mut rollback = BatchRollback::new(0);
    rollback.txs_to_delete = txids.to_vec();
    rollback.delete_all_rune_transactions = reverted_etchings;
    db.batch_rollback(&rollback, mempool).unwrap();
}

/// A `RocksDB` in a fresh temporary directory, removed on drop.
pub(crate) struct TempDb {
    db: Option<RocksDB>,
    path: PathBuf,
}

impl TempDb {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("titan-rocksdb-test-{}", Uuid::new_v4()));
        let db = Self::open(&path);
        Self { db: Some(db), path }
    }

    /// Opens the database again from disk. With `flush == false` the handle is just dropped,
    /// so whatever is not flushed must be recovered from the WAL (like after a crash).
    pub(crate) fn reopen(&mut self, flush: bool) {
        let db = self.db.take().expect("db is open");
        if flush {
            db.close().unwrap();
        } else {
            drop(db);
        }
        self.db = Some(Self::open(&self.path));
    }

    fn open(path: &std::path::Path) -> RocksDB {
        RocksDB::open(path.to_str().expect("temp dir path is UTF-8"))
            .expect("open temporary RocksDB")
    }
}

impl Deref for TempDb {
    type Target = RocksDB;

    fn deref(&self) -> &RocksDB {
        self.db.as_ref().expect("db is open until drop")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        drop(self.db.take());
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
