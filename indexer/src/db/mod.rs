pub use {error::RocksDBError, rocks::RocksDB};

mod entry;
mod error;
mod mapper;
mod rocks;
#[cfg(test)]
pub(crate) mod testing;
pub mod util;
