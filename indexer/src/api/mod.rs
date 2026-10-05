pub use api::*;

#[allow(clippy::module_inception)] // renaming would churn every `api::api` path
mod api;
pub mod content;
pub mod query;
