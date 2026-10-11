use {
    crate::{
        bitcoin_rpc::{RpcClientError, RpcClientProvider},
        index::Chain,
    },
    bitcoincore_rpc::{Auth, Client},
};

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub(crate) chain: Chain,
    pub(crate) csp_origin: Option<String>,
    pub(crate) decompress: bool,

    pub(crate) http_listen: String,

    pub(crate) bitcoin_rpc_url: String,
    pub(crate) bitcoin_rpc_auth: Auth,

    pub(crate) index_addresses: bool,
    pub(crate) enable_webhook_subscriptions: bool,
    pub(crate) webhook_admin_token: Option<String>,
}

impl RpcClientProvider for ServerConfig {
    fn get_new_rpc_client(&self) -> Result<Client, RpcClientError> {
        Client::new(&self.bitcoin_rpc_url, self.bitcoin_rpc_auth.clone())
            .map_err(|e| RpcClientError::FailedToConnect(e.to_string()))
    }
}
