use {
    crate::index::store::Store,
    std::{
        ops::Deref,
        sync::{Arc, Mutex, MutexGuard, PoisonError},
    },
};

type SharedStore = Arc<dyn Store + Send + Sync>;

/// Serializes the updater's writers to the store: the background block writer, the
/// indexer thread (mempool flush and rollback, reorg reverts, tip flags) and the
/// broadcast path. A writer keeps the guard from `write()` for its whole
/// read-modify-write.
///
/// Reads take no lock: every store write is a single atomic RocksDB batch and
/// every read-modify-write runs under `write()`, so readers never need to exclude
/// writers. In particular the indexer keeps reading while the background writer
/// persists a large batch.
///
/// Lock ordering: `Updater::broadcast_lock` -> `write()` -> `BatchDB::update`. The
/// updater's in-memory `RwLock`s (`transaction_update`, `pre_index_submitted_txs`,
/// `zmq_received_txs`) are leaves: take no other lock while holding one. `write()` is
/// not re-entrant: never call it while this thread holds its guard, and never wait
/// for the background writer (`flush_sync`, `BlockCache::close`, dropping a
/// `BlockCache`) while holding it, because the writer needs `write()`.
///
/// `RuneEntry` fields have one owner each: block indexing owns `mints`/`burned`,
/// mempool indexing owns `pending_mints`/`pending_burns`. Every writer re-reads the
/// stored entry under `write()` and keeps the other owner's fields, so neither side
/// overwrites the other's counters with a stale copy.
pub struct StoreWithLock {
    store: SharedStore,
    writer: Mutex<()>,
}

pub struct StoreWriteGuard<'a> {
    store: &'a SharedStore,
    _writer: MutexGuard<'a, ()>,
}

impl Deref for StoreWriteGuard<'_> {
    type Target = SharedStore;

    fn deref(&self) -> &SharedStore {
        self.store
    }
}

impl StoreWithLock {
    pub fn new(store: SharedStore) -> Self {
        Self {
            store,
            writer: Mutex::new(()),
        }
    }

    /// Read access. Takes no lock; see the type docs.
    pub fn read(&self) -> &SharedStore {
        &self.store
    }

    /// Exclusive write access until the guard is dropped.
    pub fn write(&self) -> StoreWriteGuard<'_> {
        // A writer that panicked left nothing half-written: each store write is one
        // atomic batch.
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        StoreWriteGuard {
            store: &self.store,
            _writer: writer,
        }
    }
}
