pub use into_usize::IntoUsize;

mod into_usize;

/// Notifies the main task that a long-running service (HTTP server, TCP
/// subscription server) has exited, so the process can shut down instead of
/// running without it. The payload names the service.
pub type ServiceExitSender = tokio::sync::mpsc::UnboundedSender<&'static str>;
