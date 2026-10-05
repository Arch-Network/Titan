use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use arch_titan_types_core::SerializedTxid;

#[derive(Debug, Default, Clone)]
pub struct TransactionUpdate {
    /// The set of transactions that entered (were added to) the mempool in this update
    mempool_added: HashSet<SerializedTxid>,

    /// The set of transactions that left (were removed from) the mempool in this update
    mempool_removed: HashSet<SerializedTxid>,

    /// The set of transactions that were added to (mined in) the current best chain
    block_added: HashSet<SerializedTxid>,

    /// The set of transactions that were removed from the best chain (reorged out)
    block_removed: HashSet<SerializedTxid>,

    /// Count of how many times a transaction was removed from blocks in this update.
    /// Needed to detect the edge case where a tx was added to a block, then reorged out,
    /// then re-mined within the same update cycle.
    block_removed_counts: HashMap<SerializedTxid, u32>,

    /// Count of how many times a transaction was added to blocks in this update.
    /// Needed to detect the edge case where a tx was mined, reorged out, then re-mined
    /// within the same update cycle (appearing twice in `block_added`).
    block_added_counts: HashMap<SerializedTxid, u32>,
}

pub struct TransactionChangeSet {
    pub removed: HashSet<SerializedTxid>,
    pub added: HashSet<SerializedTxid>,
}

impl TransactionUpdate {
    pub fn add_block_tx(&mut self, txid: SerializedTxid) {
        self.block_added.insert(txid);
        let entry = self.block_added_counts.entry(txid).or_insert(0);
        *entry += 1;
    }

    pub fn remove_block_tx(&mut self, txid: SerializedTxid) {
        self.block_removed.insert(txid);
        let entry = self.block_removed_counts.entry(txid).or_insert(0);
        *entry += 1;
    }

    pub fn update_mempool(&mut self, mempool: TransactionChangeSet) {
        self.mempool_added.extend(mempool.added);
        self.mempool_removed.extend(mempool.removed);
    }

    pub fn reset(&mut self) {
        self.mempool_added = HashSet::default();
        self.mempool_removed = HashSet::default();
        self.block_added = HashSet::default();
        self.block_removed = HashSet::default();
        self.block_added_counts = HashMap::default();
        self.block_removed_counts = HashMap::default();
    }

    /// Categorize into exactly two “buckets”:
    ///
    /// 1. **Newly added**:
    ///    TXs that were **not** previously in mempool or chain,
    ///    but are **now** in mempool or chain.
    ///
    /// 2. **Fully removed**:
    ///    TXs that **were** in mempool or chain,
    ///    but are now in **neither**.
    ///
    /// Everything else (e.g. reorg from chain to mempool, or mempool to chain)
    /// does *not* appear in these two sets.
    ///
    /// Returns `(newly_added, fully_removed)`.
    pub fn categorize_to_change_set(&self) -> TransactionChangeSet {
        // We consider the union of `mempool_added` and `block_added` to be
        // “transactions that ended up in the system (mempool or chain) *now*”.
        //
        // We consider the union of `mempool_removed` and `block_removed` to be
        // “transactions that left the system *now*”.
        //
        // However, we only want:
        //   - newly_added: those that are *only* in the added sets and *not*
        //                  in the removed sets (so they truly came from outside).
        //
        //   - fully_removed: those that are *only* in the removed sets
        //                    and *not* in the added sets (so they truly left entirely).
        //
        // If a transaction is both in `mempool_added` and `mempool_removed`,
        // that implies it was in the mempool before, left, and re-entered in the same update,
        // so it is *not* “brand-new from outside.” Likewise for `block_added` + `block_removed`.

        let in_added = &self.mempool_added | &self.block_added; // union
        let in_removed = &self.mempool_removed | &self.block_removed; // union

        // 1. Newly added: present in `in_added` but NOT present in `in_removed`.
        //    This means they were not in our system *before* this update,
        //    but have arrived in mempool or chain now.
        let newly_added = &in_added - &in_removed;

        // 2. Fully removed: present in `in_removed` but NOT present in `in_added`.
        //    This means they were in our system *before* (mempool or chain),
        //    but they are absent now.
        let fully_removed = &in_removed - &in_added;

        TransactionChangeSet {
            removed: fully_removed,
            added: newly_added,
        }
    }

    /// Detect txids that were reorged out and then re-mined within the same update,
    /// i.e., present in `block_removed` and added to `block_added` at least twice.
    pub fn block_added_after_mined_and_reorg(&self) -> HashSet<SerializedTxid> {
        self.block_added_counts
            .iter()
            .filter_map(
                |(txid, added_count)| match self.block_removed_counts.get(txid) {
                    Some(removed_count) if *removed_count > 0 && *added_count > *removed_count => {
                        Some(*txid)
                    }
                    _ => None,
                },
            )
            .collect()
    }
}

#[cfg(test)]
impl TransactionUpdate {
    pub fn new(mempool: TransactionChangeSet, block: TransactionChangeSet) -> Self {
        let mut block_added_counts = HashMap::default();
        for txid in &block.added {
            block_added_counts.insert(*txid, 1);
        }

        let mut block_removed_counts = HashMap::default();
        for txid in &block.removed {
            block_removed_counts.insert(*txid, 1);
        }

        Self {
            mempool_added: mempool.added,
            mempool_removed: mempool.removed,
            block_added: block.added,
            block_removed: block.removed,
            block_added_counts,
            block_removed_counts,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mempool_added.is_empty()
            && self.mempool_removed.is_empty()
            && self.block_added.is_empty()
            && self.block_removed.is_empty()
    }

    pub fn enough_events_to_send(&self) -> bool {
        self.block_added.len() > 10_000
    }

    pub fn categorize_to_mempool(&self) -> TransactionChangeSet {
        TransactionChangeSet {
            added: self.mempool_added.clone(),
            removed: self.mempool_removed.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet as HashSet;
    use std::iter::FromIterator;

    fn tx(i: u8) -> SerializedTxid {
        SerializedTxid::from([i; 32])
    }

    fn set<const N: usize>(items: [SerializedTxid; N]) -> HashSet<SerializedTxid> {
        HashSet::from_iter(items)
    }

    fn cs<A: IntoIterator<Item = SerializedTxid>, R: IntoIterator<Item = SerializedTxid>>(
        added: A,
        removed: R,
    ) -> TransactionChangeSet {
        TransactionChangeSet {
            added: added.into_iter().collect(),
            removed: removed.into_iter().collect(),
        }
    }

    #[test]
    fn categorize_to_change_set_newly_added_in_mempool_only() {
        let a = tx(1);
        let update = TransactionUpdate::new(cs([a], []), cs([], []));

        let out = update.categorize_to_change_set();
        assert_eq!(out.added, set([a]));
        assert!(out.removed.is_empty());
    }

    #[test]
    fn categorize_to_change_set_newly_added_in_block_only() {
        let b = tx(2);
        let update = TransactionUpdate::new(cs([], []), cs([b], []));

        let out = update.categorize_to_change_set();
        assert_eq!(out.added, set([b]));
        assert!(out.removed.is_empty());
    }

    #[test]
    fn categorize_to_change_set_neither_if_mempool_removed_and_block_added() {
        let x = tx(3);
        let update = TransactionUpdate::new(
            cs([], [x]), // mempool_removed
            cs([x], []), // block_added
        );

        let out = update.categorize_to_change_set();
        assert!(out.added.is_empty());
        assert!(out.removed.is_empty());
    }

    #[test]
    fn categorize_to_change_set_neither_if_block_removed_and_mempool_added() {
        let y = tx(4);
        let update = TransactionUpdate::new(
            cs([y], []), // mempool_added
            cs([], [y]), // block_removed
        );

        let out = update.categorize_to_change_set();
        assert!(out.added.is_empty());
        assert!(out.removed.is_empty());
    }

    #[test]
    fn categorize_to_change_set_removed_if_block_removed_only() {
        let r = tx(5);
        let update = TransactionUpdate::new(
            cs([], []),
            cs([], [r]), // block_removed
        );

        let out = update.categorize_to_change_set();
        assert!(out.added.is_empty());
        assert_eq!(out.removed, set([r]));
    }

    #[test]
    fn categorize_to_change_set_removed_if_mempool_removed_only() {
        let r = tx(6);
        let update = TransactionUpdate::new(
            cs([], [r]), // mempool_removed
            cs([], []),
        );

        let out = update.categorize_to_change_set();
        assert!(out.added.is_empty());
        assert_eq!(out.removed, set([r]));
    }

    // Additional edge cases

    #[test]
    fn categorize_to_change_set_neither_if_block_removed_and_block_added_same_tx() {
        let t = tx(7);
        let update = TransactionUpdate::new(cs([], []), cs([t], [t]));
        let out = update.categorize_to_change_set();
        assert!(out.added.is_empty());
        assert!(out.removed.is_empty());
    }

    #[test]
    fn categorize_to_change_set_neither_if_mempool_removed_and_added_same_tx() {
        let t = tx(8);
        let update = TransactionUpdate::new(cs([t], [t]), cs([], []));
        let out = update.categorize_to_change_set();
        assert!(out.added.is_empty());
        assert!(out.removed.is_empty());
    }

    #[test]
    fn categorize_to_change_set_mixed_multiple_transactions() {
        let a = tx(10); // mempool-only new -> added
        let b = tx(11); // block-only new -> added
        let c = tx(12); // mempool_removed + block_added -> neither
        let d = tx(13); // block_removed + mempool_added -> neither
        let e = tx(14); // block_removed only -> removed
        let f = tx(15); // mempool_removed only -> removed

        let update = TransactionUpdate::new(cs([a, d], [c, f]), cs([b, c], [d, e]));
        let out = update.categorize_to_change_set();

        assert_eq!(out.added, set([a, b]));
        assert_eq!(out.removed, set([e, f]));
    }

    #[test]
    fn helper_is_empty_and_reset() {
        let a = tx(21);
        let b = tx(22);
        let mut update = TransactionUpdate::new(cs([a], []), cs([b], []));
        assert!(!update.is_empty());
        update.reset();
        assert!(update.is_empty());
    }

    #[test]
    fn helper_update_mempool_extends_sets() {
        let a1 = tx(31);
        let a2 = tx(32);
        let r1 = tx(33);
        let r2 = tx(34);

        let mut update = TransactionUpdate::new(cs([a1], [r1]), cs([], []));
        update.update_mempool(cs([a2], [r2]));

        let mem = update.categorize_to_mempool();
        assert_eq!(mem.added, set([a1, a2]));
        assert_eq!(mem.removed, set([r1, r2]));
    }

    #[test]
    fn helper_categorize_to_mempool_reflects_mempool_sets_only() {
        let a = tx(41);
        let r = tx(42);
        let ba = tx(43);
        let br = tx(44);

        let update = TransactionUpdate::new(cs([a], [r]), cs([ba], [br]));
        let mem = update.categorize_to_mempool();
        assert_eq!(mem.added, set([a]));
        assert_eq!(mem.removed, set([r]));
    }

    #[test]
    fn helper_enough_events_to_send_threshold() {
        let mut update = TransactionUpdate::default();
        // Exactly 10_000 distinct txids -> false
        for i in 0..10_000u32 {
            let bytes4 = i.to_be_bytes();
            let mut bytes32 = [0u8; 32];
            // Repeat the 4 bytes 8 times to make a 32-byte unique pattern per i
            for k in 0..8 {
                bytes32[k * 4..(k + 1) * 4].copy_from_slice(&bytes4);
            }
            update.add_block_tx(SerializedTxid::from(bytes32));
        }
        assert!(!update.enough_events_to_send());

        // Add one more -> true
        update.add_block_tx(tx(200));
        assert!(update.enough_events_to_send());
    }

    #[test]
    fn no_event_if_reorged_twice_and_remined_twice() {
        // Start with tx in a block, then reorged out twice and re-mined twice within the same update
        let t = tx(50);

        let mut update = TransactionUpdate::new(cs([t], []), cs([], []));

        // Simulate two reorg outs
        update.remove_block_tx(t);
        update.remove_block_tx(t);

        // Simulate two re-mines
        update.add_block_tx(t);
        update.add_block_tx(t);

        // Our helper should not flag this as an event-worthy re-add
        let flagged = update.block_added_after_mined_and_reorg();
        assert!(flagged.is_empty());
    }

    #[test]
    fn event_if_direct_block_add_then_reorg_once_and_remined_once() {
        let t = tx(51);

        // New tx added directly to a block
        let mut update = TransactionUpdate::new(cs([], []), cs([t], []));

        // One reorg out
        update.remove_block_tx(t);

        // One re-mine
        update.add_block_tx(t);

        // Should be flagged as event-worthy (added > removed)
        let flagged = update.block_added_after_mined_and_reorg();
        assert!(flagged.contains(&t));
    }

    #[test]
    fn no_event_if_reorged_once_and_not_remined() {
        let t = tx(52);
        let mut update = TransactionUpdate::new(cs([t], []), cs([], []));

        // Reorg out once, not re-mined
        update.remove_block_tx(t);

        let flagged = update.block_added_after_mined_and_reorg();
        assert!(flagged.is_empty());
    }
}
