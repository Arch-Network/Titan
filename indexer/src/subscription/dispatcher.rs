use {
    super::tcp_subscription::TcpSubscriptionManager,
    crate::subscription::WebhookSubscriptionManager,
    arch_titan_types_core::Event,
    chrono::{DateTime, Utc},
    std::{
        fs::OpenOptions,
        io::{BufWriter, Write},
        path::PathBuf,
        sync::{
            mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
            Arc,
        },
        time::SystemTime,
    },
    tokio::{
        select,
        sync::{mpsc, watch},
    },
    tracing::{error, info, warn},
};

const EVENT_LOG_QUEUE: usize = 10_000;

/// Appends events to a file on a dedicated thread so the async runtime never
/// performs blocking file IO. Lines are dropped if the writer falls behind.
pub struct EventLogger {
    sender: SyncSender<String>,
}

impl EventLogger {
    pub fn spawn(path: PathBuf) -> std::io::Result<Self> {
        let (sender, receiver) = sync_channel(EVENT_LOG_QUEUE);
        std::thread::Builder::new()
            .name("titan-event-log".into())
            .spawn(move || write_event_log(path, receiver))?;
        Ok(Self { sender })
    }

    fn log(&self, event: &Event) {
        match self.sender.try_send(format!("{:?}", event)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => warn!("Event log queue full, dropping event"),
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

fn write_event_log(path: PathBuf, receiver: Receiver<String>) {
    let file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to open event log {}: {}", path.display(), e);
            return;
        }
    };
    let mut writer = BufWriter::new(file);

    // Block for the next line, write it and everything else already queued, then flush.
    while let Ok(first) = receiver.recv() {
        let timestamp = DateTime::<Utc>::from(SystemTime::now())
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let result = std::iter::once(first)
            .chain(receiver.try_iter())
            .try_for_each(|content| writeln!(writer, "[{:?}] {:?}", timestamp, content))
            .and_then(|_| writer.flush());

        if let Err(e) = result {
            error!("Failed to write event log {}: {}", path.display(), e);
        }
    }
}

/// Asynchronously receive events from `receiver` and process them.
/// If a shutdown signal arrives, exit gracefully.
pub async fn event_dispatcher(
    mut receiver: mpsc::Receiver<Event>,
    subscription_manager: Option<Arc<WebhookSubscriptionManager>>,
    tcp_subscription_manager: Option<Arc<TcpSubscriptionManager>>,
    mut shutdown_rx: watch::Receiver<()>,
    event_logger: Option<EventLogger>,
) {
    info!("event_dispatcher started");
    loop {
        select! {
            maybe_event = receiver.recv() => {
                match maybe_event {
                    Some(event) => {
                        // Queue webhook deliveries (never blocks).
                        if let Some(manager) = &subscription_manager {
                            manager.broadcast(&event);
                        }

                        if let Some(logger) = &event_logger {
                            if matches!(
                                event,
                                Event::TransactionsAdded { .. }
                                | Event::TransactionsReplaced { .. }
                                | Event::NewBlock { .. }
                                | Event::Reorg { .. }
                            ) {
                                logger.log(&event);
                            }
                        }

                        // Broadcast to TCP subscribers
                        if let Some(manager) = &tcp_subscription_manager {
                            manager.broadcast(&event).await;
                        }
                    },
                    None => {
                        // The sender side was dropped, so no more events
                        info!("event_dispatcher: channel closed, exiting");
                        break;
                    }
                }
            }

            // If we ever get a shutdown signal, exit
            _ = shutdown_rx.changed() => {
                info!("event_dispatcher received shutdown signal, exiting.");
                break;
            }
        }
    }

    info!("event_dispatcher ended");
}
