use {
    crate::models::TransactionStateChangeInput,
    arch_titan_types_core::{SerializedOutPoint, SerializedTxid},
    std::{collections::HashSet, fmt::Display},
};

#[derive(Debug, Default, Clone)]
pub struct BatchDelete {
    pub tx_outs: HashSet<SerializedOutPoint>,
    pub script_pubkeys_outpoints: HashSet<TransactionStateChangeInput>,
    pub spent_outpoints_in_mempool: HashSet<SerializedOutPoint>,
    pub tx_state_changes: HashSet<SerializedTxid>,
}

impl BatchDelete {
    pub fn new() -> Self {
        Self {
            tx_outs: HashSet::new(),
            script_pubkeys_outpoints: HashSet::new(),
            spent_outpoints_in_mempool: HashSet::new(),
            tx_state_changes: HashSet::new(),
        }
    }
}

impl Display for BatchDelete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BatchDelete: {} tx_outs, {} script_pubkeys_outpoints, {} spent_outpoints_in_mempool, {} tx_state_changes",
            self.tx_outs.len(),
            self.script_pubkeys_outpoints.len(),
            self.spent_outpoints_in_mempool.len(),
            self.tx_state_changes.len()
        )
    }
}
