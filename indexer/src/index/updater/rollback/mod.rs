#[allow(clippy::module_inception)] // renaming would churn every `rollback::rollback` path
mod rollback;
mod rollback_cache;

pub use rollback::{Rollback, RollbackError};
