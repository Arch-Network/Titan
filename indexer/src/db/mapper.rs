use bitcoin::{consensus, hashes::Hash, BlockHash, Transaction};
use rocks::DBResult;

use super::{entry::Entry, *};

pub trait DBResultMapper<T> {
    fn mapped(self) -> DBResult<Option<T>>;
}

fn map_db_type<T>(
    val: DBResult<Option<Vec<u8>>>,
    map_fn: &dyn Fn(&[u8]) -> DBResult<T>,
) -> DBResult<Option<T>> {
    match val {
        Ok(Some(val)) => {
            let mapped = map_fn(&val)?;
            Ok(Some(mapped))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    }
}

impl<T: Entry> DBResultMapper<T> for DBResult<Option<Vec<u8>>> {
    fn mapped(self) -> DBResult<Option<T>> {
        map_db_type(self, &|vec: &[u8]| T::load(vec))
    }
}

impl DBResultMapper<u64> for DBResult<Option<Vec<u8>>> {
    fn mapped(self) -> DBResult<Option<u64>> {
        let func = |vec: &[u8]| -> DBResult<u64> {
            match vec.to_owned().try_into() {
                Ok(val) => Ok(u64::from_le_bytes(val)),
                Err(_) => Err(RocksDBError::InvalidU64),
            }
        };

        map_db_type(self, &func)
    }
}

impl DBResultMapper<String> for DBResult<Option<Vec<u8>>> {
    fn mapped(self) -> DBResult<Option<String>> {
        let func = |vec: &[u8]| -> DBResult<String> {
            Ok(std::str::from_utf8(vec)
                .map_err(|_| RocksDBError::InvalidString)?
                .to_string())
        };

        map_db_type(self, &func)
    }
}

impl DBResultMapper<BlockHash> for DBResult<Option<Vec<u8>>> {
    fn mapped(self) -> DBResult<Option<BlockHash>> {
        let func = |vec: &[u8]| -> DBResult<BlockHash> {
            Ok(BlockHash::from_raw_hash(
                Hash::from_slice(vec).map_err(|_| RocksDBError::InvalidBlockHash)?,
            ))
        };

        map_db_type(self, &func)
    }
}

impl DBResultMapper<Transaction> for DBResult<Option<Vec<u8>>> {
    fn mapped(self) -> DBResult<Option<Transaction>> {
        let func = |vec: &[u8]| -> DBResult<Transaction> {
            consensus::deserialize(vec)
                .map_err(|e| RocksDBError::Deserialize(format!("transaction: {e}")))
        };

        map_db_type(self, &func)
    }
}
