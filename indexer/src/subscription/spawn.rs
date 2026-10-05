use {
    super::{tcp_subscription::TcpSubscriptionManager, WebhookSubscriptionManager},
    crate::{
        db::RocksDB,
        subscription::{
            dispatcher::{event_dispatcher, EventLogger},
            tcp_subscription::run_tcp_subscription_server,
            webhook::{cleanup_inactive_subscriptions, SubscriptionError},
        },
        util::ServiceExitSender,
    },
    arch_titan_types_core::Event,
    std::{path::PathBuf, sync::Arc, time::Duration},
    tokio::{
        sync::{mpsc, watch},
        task,
    },
    tracing::{error, info},
};

const DEFAULT_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);
const DEFAULT_CLEANUP_EXPIRY_SECS: u64 = 86400;

pub struct WebhookSubscriptionSpawnResult {
    pub cleanup_handle: task::JoinHandle<()>,
    pub subscription_manager: Arc<WebhookSubscriptionManager>,
}

pub struct TcpSubscriptionSpawnResult {
    pub tcp_server_handle: task::JoinHandle<()>,
    pub tcp_subscription_manager: Arc<TcpSubscriptionManager>,
}

pub struct SubscriptionSpawnResult {
    pub event_sender: mpsc::Sender<Event>,
    pub dispatcher_handle: task::JoinHandle<()>,
    pub webhook_spawn_result: Option<WebhookSubscriptionSpawnResult>,
    pub tcp_spawn_result: Option<TcpSubscriptionSpawnResult>,
    pub shutdown_tx: watch::Sender<()>,
}

pub struct SubscriptionConfig {
    pub enable_webhook_subscriptions: bool,
    pub enable_tcp_subscriptions: bool,
    pub tcp_address: String,
    pub enable_file_logging: bool,
    pub events_log_path: PathBuf,
    pub webhook_allow_private_endpoints: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionSpawnError {
    #[error("failed to bind TCP subscription server to {0}: {1}")]
    TcpBind(String, std::io::Error),
    #[error("failed to start webhook subscriptions: {0}")]
    Webhook(#[from] SubscriptionError),
    #[error("failed to start event log writer: {0}")]
    EventLog(std::io::Error),
}

/// Spawns the subscription-related background tasks (dispatcher + cleanup).
/// The TCP listener is bound before returning so bind failures abort startup.
pub fn spawn_subscription_tasks(
    db: Arc<RocksDB>,
    config: SubscriptionConfig,
    exit_tx: ServiceExitSender,
) -> Result<Option<SubscriptionSpawnResult>, SubscriptionSpawnError> {
    // If both webhook and TCP subscriptions are disabled, return None
    if !config.enable_webhook_subscriptions && !config.enable_tcp_subscriptions {
        return Ok(None);
    }

    // Create a watch channel for shutdown signaling
    let (shutdown_tx, shutdown_rx) = watch::channel(());

    // Create the TCP subscription manager if enabled
    let tcp_spawn_result = if config.enable_tcp_subscriptions {
        let tcp_subscription_manager = Arc::new(TcpSubscriptionManager::new());
        let tcp_subscription_manager_clone = tcp_subscription_manager.clone();
        let shutdown_rx_clone = shutdown_rx.clone();

        let listener = std::net::TcpListener::bind(&config.tcp_address)
            .and_then(|listener| {
                listener.set_nonblocking(true)?;
                tokio::net::TcpListener::from_std(listener)
            })
            .map_err(|e| SubscriptionSpawnError::TcpBind(config.tcp_address.clone(), e))?;
        info!(
            "TCP Subscription Server listening on {}",
            config.tcp_address
        );

        let tcp_server_handle = tokio::spawn(async move {
            run_tcp_subscription_server(
                listener,
                tcp_subscription_manager_clone,
                shutdown_rx_clone,
            )
            .await;
            let _ = exit_tx.send("TCP subscription server");
        });

        Some(TcpSubscriptionSpawnResult {
            tcp_server_handle,
            tcp_subscription_manager,
        })
    } else {
        None
    };

    // Create the webhook subscription manager if enabled
    let webhook_spawn_result = if config.enable_webhook_subscriptions {
        let webhook_subscription_manager = Arc::new(WebhookSubscriptionManager::new(
            db.clone(),
            config.webhook_allow_private_endpoints,
        )?);

        let cleanup_rx = shutdown_rx.clone();
        let cleanup_manager = webhook_subscription_manager.clone();

        let cleanup_handle = tokio::spawn(async move {
            cleanup_inactive_subscriptions(
                cleanup_manager,
                DEFAULT_CLEANUP_INTERVAL,
                DEFAULT_CLEANUP_EXPIRY_SECS,
                cleanup_rx,
            )
            .await;
        });

        Some(WebhookSubscriptionSpawnResult {
            cleanup_handle,
            subscription_manager: webhook_subscription_manager,
        })
    } else {
        None
    };

    // Create the event sender
    let (event_sender, event_receiver) = mpsc::channel::<Event>(10000);

    // Clone the receiver for the dispatcher
    let dispatcher_rx = shutdown_rx.clone();

    // Extract clones of the inner Arcs (if present) so we don’t move the original Options.
    let webhook_manager_for_dispatcher = webhook_spawn_result
        .as_ref()
        .map(|r| r.subscription_manager.clone());
    let tcp_manager_for_dispatcher = tcp_spawn_result
        .as_ref()
        .map(|r| r.tcp_subscription_manager.clone());

    let event_logger = if config.enable_file_logging {
        Some(
            EventLogger::spawn(config.events_log_path.clone())
                .map_err(SubscriptionSpawnError::EventLog)?,
        )
    } else {
        None
    };

    let dispatcher_handle = tokio::spawn(async move {
        event_dispatcher(
            event_receiver,
            webhook_manager_for_dispatcher,
            tcp_manager_for_dispatcher,
            dispatcher_rx,
            event_logger,
        )
        .await;
    });

    info!("Spawned subscription tasks (dispatcher + cleanup).");

    Ok(Some(SubscriptionSpawnResult {
        event_sender,
        dispatcher_handle,
        webhook_spawn_result,
        tcp_spawn_result,
        shutdown_tx,
    }))
}

pub async fn shutdown_and_wait_subscription_tasks(spawn_result: SubscriptionSpawnResult) {
    let SubscriptionSpawnResult {
        dispatcher_handle,
        webhook_spawn_result,
        tcp_spawn_result,
        shutdown_tx,
        ..
    } = spawn_result;

    if let Err(e) = shutdown_tx.send(()) {
        error!("Failed to send shutdown signal: {:?}", e);
    }

    if let Err(e) = dispatcher_handle.await {
        error!("Dispatcher task join error: {:?}", e);
    } else {
        info!("Dispatcher task ended cleanly.");
    }

    if let Some(webhook_spawn_result) = webhook_spawn_result {
        if let Err(e) = webhook_spawn_result.cleanup_handle.await {
            error!("Webhook cleanup task join error: {:?}", e);
        } else {
            info!("Webhook cleanup task ended cleanly.");
        }
    }

    if let Some(tcp_spawn_result) = tcp_spawn_result {
        if let Err(e) = tcp_spawn_result.tcp_server_handle.await {
            error!("TCP server task join error: {:?}", e);
        } else {
            info!("TCP server task ended cleanly.");
        }
    }
}
