use {
    super::manager::{now_secs, SubscriptionManager},
    std::{sync::Arc, time::Duration},
    tokio::{select, sync::watch, time::sleep},
    tracing::{error, info},
};

/// Periodically delete subscriptions that haven't succeeded for `expiry_secs`.
/// If a shutdown signal arrives, exit gracefully.
pub async fn cleanup_inactive_subscriptions(
    manager: Arc<SubscriptionManager>,
    interval: Duration,
    expiry_secs: u64,
    mut shutdown_rx: watch::Receiver<()>,
) {
    info!("cleanup_inactive_subscriptions started");

    loop {
        select! {
            _ = sleep(interval) => do_cleanup(&manager, now_secs(), expiry_secs),
            _ = shutdown_rx.changed() => {
                info!("cleanup_inactive_subscriptions received shutdown signal, exiting.");
                break;
            }
        }
    }

    info!("cleanup_inactive_subscriptions ended");
}

/// Remove subscriptions whose last success is more than `expiry_secs` before `now_secs`.
fn do_cleanup(manager: &SubscriptionManager, now_secs: u64, expiry_secs: u64) {
    for id in manager.expired_subscription_ids(now_secs, expiry_secs) {
        if let Err(e) = manager.delete_subscription(&id) {
            error!("cleanup error deleting subscription {}: {:?}", id, e);
        }
    }
}
