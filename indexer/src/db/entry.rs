use {
    super::RocksDBError,
    crate::models::{BlockId, Inscription, RuneEntry, TransactionStateChange, TxRuneIndexRef},
    arch_titan_types_api::Subscription,
    arch_titan_types_core::{Block, MempoolEntry, RuneId, SpenderReference, TxOut},
    borsh::{BorshDeserialize, BorshSerialize},
};

pub trait Entry: Sized + BorshDeserialize + BorshSerialize {
    /// Deserializes a stored value. Corrupt or truncated values yield an error instead of
    /// panicking.
    fn load(value: &[u8]) -> Result<Self, RocksDBError> {
        Self::deserialize(&mut &value[..]).map_err(|e| {
            RocksDBError::Deserialize(format!("{}: {e}", std::any::type_name::<Self>()))
        })
    }

    fn store(self) -> Vec<u8> {
        self.store_ref()
    }

    /// Serialize the entry by reference without consuming `self`.
    fn store_ref(&self) -> Vec<u8> {
        // Writing into a `Vec` cannot fail.
        borsh::to_vec(self).expect("borsh serialization into a Vec is infallible")
    }
}

impl Entry for Block {}
impl Entry for BlockId {}
impl Entry for Inscription {}
impl Entry for RuneEntry {}
impl Entry for TxRuneIndexRef {}
impl Entry for Vec<TxRuneIndexRef> {}
impl Entry for TransactionStateChange {}
impl Entry for TxOut {}
impl Entry for Subscription {}
impl Entry for SpenderReference {}
impl Entry for MempoolEntry {}
impl Entry for RuneId {}
