mod connection_status;
mod reconnection;
#[cfg(feature = "tcp_client")]
mod tcp_client;
#[cfg(feature = "tcp_client_blocking")]
mod tcp_client_blocking;

pub use connection_status::{ConnectionStatus, ConnectionStatusTracker};
pub use reconnection::{ReconnectionConfig, ReconnectionManager};
#[cfg(feature = "tcp_client")]
pub use tcp_client::{
    AsyncTcpClient as TitanTcpClient, Config as TitanTcpClientConfig,
    TcpClientError as TitanTcpClientError,
};
#[cfg(feature = "tcp_client_blocking")]
pub use tcp_client_blocking::{
    TcpClient as TitanTcpClientBlocking, TcpClientConfig as TitanTcpClientBlockingConfig,
    TcpClientError as TitanTcpClientBlockingError,
};
