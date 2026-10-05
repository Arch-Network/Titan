use std::{
    sync::{Arc, OnceLock, PoisonError, RwLock, RwLockReadGuard},
    thread,
    time::{Duration, Instant},
};

use arch_titan_types_core::RuneId;
use crossbeam_channel::{bounded, Sender};
use thiserror::Error;
use tracing::{debug, error, info};

use crate::{
    index::{store::Store, updater::store_lock::StoreWithLock, StoreError},
    models::{BatchDelete, BatchUpdate},
};

/// A background write failed. The error is sticky: the writer drops every later
/// batch unwritten, and every later `save`, `wait_until_empty` or `close` returns it.
#[derive(Debug, Clone, Error)]
#[error("background RocksDB write failed: {0}")]
pub struct BgWriteError(String);

pub struct BatchDB {
    /// Readers share it; the writer thread takes `write()` only for the short merge
    /// in `merge_stored_pending_rune_fields`.
    update: RwLock<BatchUpdate>,
    delete: BatchDelete,
}

impl BatchDB {
    pub fn new(update: BatchUpdate, delete: BatchDelete) -> Self {
        Self {
            update: RwLock::new(update),
            delete,
        }
    }

    fn update(&self) -> RwLockReadGuard<'_, BatchUpdate> {
        self.update.read().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct BgWriterSettings {
    pub max_async_batches: usize,
}

pub struct BgWriter {
    handle: Option<std::thread::JoinHandle<()>>,
    sender: Option<Sender<BgMessage>>,
    pending_batches: Vec<Arc<BatchDB>>,
    error: Arc<OnceLock<BgWriteError>>,
}

struct BgMessage {
    batch: Arc<BatchDB>,
    /// Fires only after the batch was persisted. On failure it is dropped, so the
    /// receiver sees a disconnect instead.
    notify: Option<crossbeam_channel::Sender<()>>,
}

impl BgWriter {
    pub fn start(db: Arc<StoreWithLock>, settings: BgWriterSettings) -> Self {
        let (tx, rx) = bounded::<BgMessage>(settings.max_async_batches);
        let error = Arc::new(OnceLock::new());
        let thread_error = error.clone();

        let handle = thread::Builder::new()
            .name("rocksdb-writer".into())
            .spawn(move || {
                while let Ok(BgMessage { batch, notify }) = rx.recv() {
                    // Never persist anything after a failed batch: later batches carry
                    // block counts that would claim the missing data was written.
                    if thread_error.get().is_some() {
                        continue;
                    }

                    let result = {
                        let store = db.write();
                        Self::write_batch(&**store, &batch)
                    };

                    match result {
                        Ok(()) => {
                            if let Some(done_tx) = notify {
                                info!("BgWriter: sending notify");
                                if let Err(e) = done_tx.send(()) {
                                    error!("BgWriter: error sending notify: {:?}", e);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Background RocksDB write failed: {:?}", e);
                            let _ = thread_error.set(BgWriteError(e.to_string()));
                        }
                    }
                }
            })
            .expect("failed to spawn rocksdb-writer thread");

        Self {
            handle: Some(handle),
            sender: Some(tx),
            pending_batches: Vec::new(),
            error,
        }
    }

    fn write_batch(store: &dyn Store, batch: &BatchDB) -> Result<(), StoreError> {
        Self::merge_stored_pending_rune_fields(store, batch)?;

        let update = batch.update();
        let start = Instant::now();
        store.batch_update(&update, false)?;
        debug!("Flushed update: {} in {:?}", *update, start.elapsed());

        let start = Instant::now();
        store.batch_delete(&batch.delete)?;
        debug!("Flushed delete: {} in {:?}", batch.delete, start.elapsed());

        Ok(())
    }

    /// The block cache holds each rune entry for a whole commit window, so its
    /// `pending_mints`/`pending_burns` can be older than what mempool indexing has
    /// stored since. Those fields belong to mempool indexing: take them from the
    /// store. The caller holds the store write lock, so nothing changes them before
    /// this batch is written.
    fn merge_stored_pending_rune_fields(
        store: &dyn Store,
        batch: &BatchDB,
    ) -> Result<(), StoreError> {
        let rune_ids: Vec<RuneId> = batch.update().runes.keys().copied().collect();
        if rune_ids.is_empty() {
            return Ok(());
        }

        let stored = store.get_runes_by_ids(&rune_ids)?;
        let mut update = batch.update.write().unwrap_or_else(PoisonError::into_inner);

        for (rune_id, entry) in update.runes.iter_mut() {
            if let Some(stored) = stored.get(rune_id) {
                entry.pending_mints = stored.pending_mints;
                entry.pending_burns = stored.pending_burns;
            }
        }

        Ok(())
    }

    /// Search a value inside the currently pending (still in–flight) batch updates.
    /// The provided closure will be executed on each `BatchUpdate` starting from the
    /// most-recent one (the last pushed into `pending_batches`). The first `Some` value
    /// returned will be forwarded. This is a generic utility that avoids repeating the
    /// same lookup logic for every data type we need to read while a background write
    /// is ongoing.
    pub fn find_in_pending<T, F>(&self, finder: F) -> Option<T>
    where
        F: Fn(&BatchUpdate) -> Option<T>,
    {
        // Iterate newest → oldest to make sure we prefer the most recently flushed batch.
        for batch in self.pending_batches.iter().rev() {
            if let Some(val) = finder(&batch.update()) {
                return Some(val);
            }
        }
        None
    }

    /// Fails if an earlier background write failed or the writer thread died.
    pub fn check(&self) -> Result<(), BgWriteError> {
        if let Some(e) = self.error.get() {
            return Err(e.clone());
        }

        // The writer only exits once the channel is closed: earlier, it panicked.
        if self.sender.is_some() && self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            return Err(BgWriteError("background writer thread stopped".into()));
        }

        Ok(())
    }

    pub fn save(&mut self, batch: Arc<BatchDB>) -> Result<(), BgWriteError> {
        self.enqueue(batch, None)
    }

    /// Enqueue a batch and return a receiver that fires once the background write
    /// completes. The receiver disconnects without a message if the write fails.
    pub fn save_with_notify(
        &mut self,
        batch: Arc<BatchDB>,
    ) -> Result<crossbeam_channel::Receiver<()>, BgWriteError> {
        let (tx, rx) = bounded::<()>(1);
        self.enqueue(batch, Some(tx))?;
        Ok(rx)
    }

    fn enqueue(
        &mut self,
        batch: Arc<BatchDB>,
        notify: Option<crossbeam_channel::Sender<()>>,
    ) -> Result<(), BgWriteError> {
        self.check()?;
        self.cleanup_completed_batches();
        self.pending_batches.push(batch.clone());

        let Some(sender) = &self.sender else {
            return Err(BgWriteError("background writer is closed".into()));
        };

        sender
            .send(BgMessage { batch, notify })
            .map_err(|_| BgWriteError("background writer thread stopped".into()))
    }

    fn cleanup_completed_batches(&mut self) {
        self.pending_batches.retain(|b| Arc::strong_count(b) > 1);
    }

    /// Block the caller until all queued batches have been handled by the writer,
    /// then report whether any of them failed.
    ///
    /// This is useful in scenarios (e.g. reorg handling) where we must guarantee
    /// that every update previously pushed via `save` is fully persisted before
    /// we proceed with further read-or-write operations on the underlying store.
    /// The caller must not hold the `StoreWithLock` write guard.
    pub fn wait_until_empty(&mut self) -> Result<(), BgWriteError> {
        // Actively poll the list of in-flight batches until it becomes empty.
        // The background writer will drop its Arc once the write completes.
        while !self.pending_batches.is_empty() {
            // Remove any batches that the writer has already finished.
            self.cleanup_completed_batches();

            // If there are still pending batches, wait a short amount of time
            // before polling again to avoid busy-spinning.
            if !self.pending_batches.is_empty() {
                thread::sleep(Duration::from_millis(50));
            }
        }

        self.check()
    }

    /// Stop the writer after it has handled every queued batch and report whether
    /// any write failed. The caller must not hold the `StoreWithLock` write guard.
    pub fn close(&mut self) -> Result<(), BgWriteError> {
        self.join();
        self.check()
    }

    fn join(&mut self) {
        // Close the sender side of the channel first so the background
        // thread can finish processing and exit its loop.
        drop(self.sender.take());

        if let Some(handle) = self.handle.take() {
            if let Err(err) = handle.join() {
                tracing::error!("Failed to join rocksdb-writer thread: {:?}", err);
                let _ = self
                    .error
                    .set(BgWriteError("background writer thread panicked".into()));
            }
        }

        self.pending_batches.clear();
    }
}

impl Drop for BgWriter {
    fn drop(&mut self) {
        // Wait for the background writer thread to finish to guarantee that
        // all pending updates have been flushed before shutting down.
        self.join();
    }
}
