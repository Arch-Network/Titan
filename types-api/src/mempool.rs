use {
    arch_titan_types_core::MempoolEntry,
    bitcoin::Txid,
    serde::{Deserialize, Serialize},
};

/// One item of `GET /mempool/entries`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MempoolTxEntry {
    pub txid: Txid,
    pub entry: MempoolEntry,
}
