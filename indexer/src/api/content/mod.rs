pub use {
    accept_encoding::AcceptEncoding,
    content::{content_response, ContentError},
};

mod accept_encoding;
#[allow(clippy::module_inception)] // renaming would churn every `content::content` path
mod content;
