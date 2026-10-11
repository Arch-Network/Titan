use {
    crate::{
        db::RocksDB,
        index::{store::Store, updater::store_lock::StoreWithLock},
        models::{BatchUpdate, RuneEntry},
    },
    arch_titan_types_core::{Rune, RuneId, SerializedTxid, SpacedRune},
    std::{path::PathBuf, sync::Arc},
    uuid::Uuid,
};

/// A `RocksDB` in a fresh temporary directory, removed on drop.
pub(crate) struct TempStore {
    db: Arc<RocksDB>,
    path: PathBuf,
}

impl TempStore {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("titan-updater-test-{}", Uuid::new_v4()));
        let db = RocksDB::open(path.to_str().expect("temp dir path is UTF-8"))
            .expect("open temporary RocksDB");
        Self {
            db: Arc::new(db),
            path,
        }
    }

    pub(crate) fn db(&self) -> &RocksDB {
        &self.db
    }

    pub(crate) fn shared(&self) -> Arc<dyn Store + Send + Sync> {
        self.db.clone()
    }

    pub(crate) fn locked(&self) -> Arc<StoreWithLock> {
        Arc::new(StoreWithLock::new(self.shared()))
    }

    /// Writes `entry` the way an indexed block does.
    pub(crate) fn put_rune(&self, id: RuneId, entry: &RuneEntry) {
        let mut update = BatchUpdate::new(0, 0, 0);
        update.runes.insert(id, entry.clone());
        self.db.batch_update(&update, false).unwrap();
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn rune_entry(mints: u128, pending_mints: u128) -> RuneEntry {
    RuneEntry {
        block: 840_000,
        burned: 0,
        divisibility: 0,
        etching: SerializedTxid::all_zeros(),
        terms: None,
        mints,
        number: 1,
        premine: 0,
        spaced_rune: SpacedRune {
            rune: Rune(99_246_114_928_149_462),
            spacers: 0,
        },
        symbol: None,
        timestamp: 0,
        turbo: false,
        pending_burns: 0,
        pending_mints,
        inscription_id: None,
    }
}
