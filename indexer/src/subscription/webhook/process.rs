use {
    super::manager::{now_secs, SubscriptionManager},
    arch_titan_types_core::{Event, EventType},
    reqwest::{Client, StatusCode},
    std::{sync::Arc, time::Duration},
    thiserror::Error,
    tokio::time::sleep,
    tracing::{error, warn},
};

const MAX_RETRIES: usize = 5;

#[derive(Debug, Error)]
pub enum SendEventError {
    #[error("endpoint returned non-retryable status {0}")]
    Rejected(StatusCode),
    #[error("timeout after {attempts} retries")]
    Timeout { attempts: usize },
}

/// Statuses worth retrying: server errors and the client errors that signal a
/// temporary condition. Other 3xx/4xx responses fail the delivery immediately.
fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
        )
}

// Helper to send event to endpoint with retry logic
async fn send_event_with_retry(
    client: &Client,
    endpoint: &str,
    event: &Event,
    max_retries: usize,
) -> Result<(), SendEventError> {
    let mut attempt = 0;
    loop {
        let res = client.post(endpoint).json(event).send().await;

        match res {
            Ok(response) if response.status().is_success() => {
                // SUCCESS - stop retrying
                return Ok(());
            }
            Ok(response) if !is_retryable_status(response.status()) => {
                error!(
                    "Non-retryable HTTP status {} from endpoint: {}",
                    response.status(),
                    endpoint
                );
                return Err(SendEventError::Rejected(response.status()));
            }
            Ok(response) => {
                error!("Non-success HTTP status: {}", response.status());
            }
            Err(e) => {
                error!("HTTP request error: {}", e);
            }
        }

        attempt += 1;
        if attempt >= max_retries {
            error!("Max retries reached for endpoint: {}", endpoint);
            return Err(SendEventError::Timeout {
                attempts: max_retries,
            });
        }

        // Exponential backoff
        let delay = Duration::from_secs(2u64.pow(attempt as u32));
        sleep(delay).await;
    }
}

/// Spawn one delivery task per interested subscription. Each task holds a
/// permit from the manager's delivery semaphore; when none are free the
/// delivery is dropped so the dispatcher (and the indexer feeding it) never waits.
pub fn process_event(manager: &Arc<SubscriptionManager>, event: &Event) {
    let event_type = EventType::from(event.clone());
    let interested = manager.interested(&event_type);
    if interested.is_empty() {
        return;
    }

    let event = Arc::new(event.clone());
    for (id, endpoint) in interested {
        let Ok(permit) = manager.deliveries().clone().try_acquire_owned() else {
            warn!(
                "Webhook delivery capacity exhausted, dropping {:?} event for subscription {}",
                event_type, id
            );
            continue;
        };

        let manager = manager.clone();
        let event = event.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result =
                send_event_with_retry(manager.client(), &endpoint, &event, MAX_RETRIES).await;

            if result.is_ok() {
                if let Err(e) = manager.mark_success(&id, now_secs()) {
                    error!("Failed to record webhook success for {}: {:?}", id, e);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_only_transient_statuses() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_EARLY,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(is_retryable_status(status), "{status}");
        }
        for status in [
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
        ] {
            assert!(!is_retryable_status(status), "{status}");
        }
    }
}
