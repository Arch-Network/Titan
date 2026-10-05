use {
    crate::{
        index::{Chain, Settings},
        server::ServerConfig,
        subscription::SubscriptionConfig,
    },
    bitcoincore_rpc::Auth,
    clap::{
        builder::{
            styling::{AnsiColor, Effects},
            Styles,
        },
        ArgAction, Parser,
    },
    std::path::PathBuf,
    tracing::warn,
};

#[derive(Clone, Default, Debug, Parser)]
#[command(
    name = "rune-indexer",
    about = "A minimal Rune indexer",
    version,
    styles = Styles::styled()
      .error(AnsiColor::Red.on_default() | Effects::BOLD)
      .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
      .invalid(AnsiColor::Red.on_default())
      .literal(AnsiColor::Blue.on_default())
      .placeholder(AnsiColor::Cyan.on_default())
      .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
      .valid(AnsiColor::Green.on_default()),
  )]
pub struct Options {
    /// Authenticate to Bitcoin Core RPC with <BITCOIN_RPC_PASSWORD>.
    #[arg(long, env, hide_env_values = true)]
    pub(super) bitcoin_rpc_password: Option<String>,

    /// Connect to Bitcoin Core RPC at <BITCOIN_RPC_URL>.
    #[arg(long, env, default_value = "http://localhost:8332")]
    pub(super) bitcoin_rpc_url: String,

    /// Authenticate to Bitcoin Core RPC as <BITCOIN_RPC_USERNAME>.
    #[arg(long, env)]
    pub(super) bitcoin_rpc_username: Option<String>,

    /// Max <N> Bitcoin Core RPC requests in flight (minimum 4).
    #[arg(long, env, default_value = "12")]
    pub(super) bitcoin_rpc_limit: u32,

    /// Max number of idle RPC clients kept in the pool.
    #[arg(long, env, default_value = "500")]
    pub(super) bitcoin_rpc_pool_size: u32,

    /// Use <CHAIN>.
    #[arg(long = "chain", env = "CHAIN", value_enum, default_value = "mainnet")]
    pub(super) chain: Chain,

    /// Load Bitcoin Core RPC cookie file from <COOKIE_FILE>.
    #[arg(long, env)]
    pub(super) cookie_file: Option<PathBuf>,

    /// Store index in <DATA_DIR>.
    #[arg(long, env, alias = "datadir", default_value = "./data")]
    pub(super) data_dir: PathBuf,

    /// Do not index inscriptions (rune icons).
    #[arg(long, env, short, alias = "noindex_inscriptions", action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) no_index_inscriptions: bool,

    /// Index bitcoin transactions.
    #[arg(long, env, short, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "true")]
    pub(super) index_bitcoin_transactions: bool,

    /// Index spent outputs.
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "true")]
    pub(super) index_spent_outputs: bool,

    /// Index addresses.
    #[arg(long, env, short = 'a', action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) index_addresses: bool,

    /// Commit interval in blocks.
    #[arg(long, env, default_value = "50")]
    pub(super) commit_interval: u64,

    /// Enable zmq listener. This optimizes the mempool indexing process because
    /// we don't need to fetch transactions from the RPC.
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) enable_zmq_listener: bool,

    /// ZeroMQ endpoint for raw transactions from bitcoind.
    #[arg(long, env, default_value = "tcp://127.0.0.1:28332")]
    pub(super) zmq_endpoint: String,

    /// Listen address for the REST API server.
    #[arg(long, env, default_value = "0.0.0.0:3030")]
    pub(super) http_listen: String,

    /// Use <CSP_ORIGIN> in Content-Security-Policy header. Set this to the public-facing URL of your rune-indexer instance.
    #[arg(long, env)]
    pub(super) csp_origin: Option<String>,

    /// Decompress encoded content. Currently only supports brotli. Be careful using this on production instances. A decompressed inscription may be arbitrarily large, making decompression a DoS vector.
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) decompress: bool,

    /// Main loop interval in milliseconds.
    #[arg(long, env, default_value = "500")]
    pub(super) main_loop_interval: u64,

    /// Enable webhook subscription service (requires --webhook-admin-token).
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) enable_webhook_subscriptions: bool,

    /// Bearer token required (`Authorization: Bearer <TOKEN>`) on all webhook subscription routes.
    #[arg(long, env, hide_env_values = true)]
    pub(super) webhook_admin_token: Option<String>,

    /// Allow webhook endpoints on loopback/private/link-local addresses. Only for
    /// trusted deployments where webhook receivers live on an internal network.
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) webhook_allow_private_endpoints: bool,

    /// Enable TCP subscription service.
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) enable_tcp_subscriptions: bool,

    /// TCP address to listen to.
    #[arg(long, env, default_value = "127.0.0.1:8080")]
    pub(super) tcp_address: String,

    /// Enable event file logging.
    #[arg(long, env, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub(super) enable_file_logging: bool,

    /// Event log file used by --enable-file-logging. [default: <DATA_DIR>/events.log]
    #[arg(long, env)]
    pub(super) events_log_path: Option<PathBuf>,
}

impl Options {
    pub fn get_bitcoin_rpc_auth(&self) -> Auth {
        let bitcoin_rpc_auth = if let Some(cookie_file) = self.cookie_file.as_ref() {
            Auth::CookieFile(cookie_file.clone())
        } else if let Some(bitcoin_rpc_username) = self.bitcoin_rpc_username.as_ref() {
            Auth::UserPass(
                bitcoin_rpc_username.clone(),
                self.bitcoin_rpc_password.clone().unwrap_or_default(),
            )
        } else {
            warn!("No authentication provided for Bitcoin Core RPC");
            Auth::None
        };

        bitcoin_rpc_auth
    }
}

impl From<Options> for Settings {
    fn from(options: Options) -> Self {
        let bitcoin_rpc_auth = options.get_bitcoin_rpc_auth();

        Self {
            data_dir: options.data_dir,
            zmq_endpoint: options.zmq_endpoint,
            bitcoin_rpc_limit: options.bitcoin_rpc_limit,
            bitcoin_rpc_url: options.bitcoin_rpc_url,
            bitcoin_rpc_auth,
            chain: options.chain,
            no_index_inscriptions: options.no_index_inscriptions,
            index_bitcoin_transactions: options.index_bitcoin_transactions,
            index_spent_outputs: options.index_spent_outputs,
            index_addresses: options.index_addresses,
            commit_interval: options.commit_interval,
            main_loop_interval: options.main_loop_interval,
        }
    }
}

impl From<Options> for ServerConfig {
    fn from(options: Options) -> Self {
        let bitcoin_rpc_auth = options.get_bitcoin_rpc_auth();
        Self {
            chain: options.chain,
            csp_origin: options.csp_origin,
            decompress: options.decompress,

            http_listen: options.http_listen,
            bitcoin_rpc_url: options.bitcoin_rpc_url,
            bitcoin_rpc_auth,

            index_addresses: options.index_addresses,
            enable_webhook_subscriptions: options.enable_webhook_subscriptions,
            webhook_admin_token: options.webhook_admin_token,
        }
    }
}

impl From<Options> for SubscriptionConfig {
    fn from(options: Options) -> Self {
        Self {
            enable_webhook_subscriptions: options.enable_webhook_subscriptions,
            enable_tcp_subscriptions: options.enable_tcp_subscriptions,
            tcp_address: options.tcp_address,
            enable_file_logging: options.enable_file_logging,
            events_log_path: options
                .events_log_path
                .unwrap_or_else(|| options.data_dir.join("events.log")),
            webhook_allow_private_endpoints: options.webhook_allow_private_endpoints,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_flags_accept_explicit_values() {
        let options = Options::try_parse_from([
            "titan",
            "--index-bitcoin-transactions=false",
            "--index-spent-outputs",
            "false",
            "--index-addresses",
        ])
        .unwrap();
        assert!(!options.index_bitcoin_transactions);
        assert!(!options.index_spent_outputs);
        assert!(options.index_addresses);

        let defaults = Options::try_parse_from(["titan"]).unwrap();
        assert!(defaults.index_bitcoin_transactions);
        assert!(defaults.index_spent_outputs);
        assert!(!defaults.index_addresses);
    }
}
