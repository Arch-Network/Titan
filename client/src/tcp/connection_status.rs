use std::sync::{Arc, RwLock};
use tracing::error;

/// Represents the current state of the TCP connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Initial state or deliberately disconnected
    Disconnected,
    /// Currently attempting to connect
    Connecting,
    /// Successfully connected to the server
    Connected,
    /// Connection was lost, attempting to reconnect
    Reconnecting,
}

/// Trait for any type that can receive connection status updates
pub trait StatusListener: Send + Sync {
    fn notify(&self, status: ConnectionStatus) -> bool;
}

// Implement for std mpsc::Sender
impl StatusListener for std::sync::mpsc::Sender<ConnectionStatus> {
    fn notify(&self, status: ConnectionStatus) -> bool {
        self.send(status).is_ok()
    }
}

// Implement for tokio mpsc::Sender
impl StatusListener for tokio::sync::mpsc::Sender<ConnectionStatus> {
    fn notify(&self, status: ConnectionStatus) -> bool {
        // A full channel only means the consumer is lagging; keep the listener
        // registered and drop this update. Unregister only once it is closed.
        !matches!(
            self.try_send(status),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
        )
    }
}

/// Thread-safe wrapper for tracking and updating connection status
#[derive(Clone)]
pub struct ConnectionStatusTracker {
    status: Arc<RwLock<ConnectionStatus>>,
    listeners: Arc<RwLock<Vec<Box<dyn StatusListener>>>>,
}

impl ConnectionStatusTracker {
    /// Creates a new ConnectionStatusTracker with the initial status set to Disconnected
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(ConnectionStatus::Disconnected)),
            listeners: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Creates a new ConnectionStatusTracker with the specified initial status
    pub fn with_status(initial_status: ConnectionStatus) -> Self {
        Self {
            status: Arc::new(RwLock::new(initial_status)),
            listeners: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get the current connection status
    pub fn get_status(&self) -> ConnectionStatus {
        match self.status.read() {
            Ok(status) => *status,
            Err(_) => {
                // If the lock is poisoned, default to Disconnected
                error!("Failed to read connection status due to poisoned lock");
                ConnectionStatus::Disconnected
            }
        }
    }

    /// Register a listener to receive status updates
    pub fn register_listener<L: StatusListener + 'static>(&self, listener: L) {
        if let Ok(mut listeners) = self.listeners.write() {
            listeners.push(Box::new(listener));
        } else {
            error!("Failed to register status listener due to poisoned lock");
        }
    }

    /// Update the connection status and notify all listeners
    pub fn update_status(&self, new_status: ConnectionStatus) {
        if let Ok(mut status_guard) = self.status.write() {
            *status_guard = new_status;

            // Notify all listeners
            if let Ok(mut listeners) = self.listeners.write() {
                // Keep only listeners that successfully received the notification
                listeners.retain(|listener| listener.notify(new_status));
            }
        } else {
            error!("Failed to update connection status due to poisoned lock");
        }
    }

    /// Create a helper closure that can be used to update the status
    pub fn create_updater<'a>(&'a self) -> impl Fn(ConnectionStatus) + 'a {
        let status = Arc::clone(&self.status);
        let listeners = Arc::<RwLock<Vec<Box<dyn StatusListener>>>>::clone(&self.listeners);

        move |new_status| {
            if let Ok(mut status_guard) = status.write() {
                *status_guard = new_status;

                // Notify all listeners
                if let Ok(mut listeners_guard) = listeners.write() {
                    listeners_guard.retain(|listener| listener.notify(new_status));
                }
            } else {
                error!("Failed to update connection status due to poisoned lock");
            }
        }
    }

    /// Get a clone of the inner Arc<RwLock<ConnectionStatus>>
    /// This is useful when you need to share the status across threads
    pub fn get_inner(&self) -> Arc<RwLock<ConnectionStatus>> {
        Arc::clone(&self.status)
    }
}

impl Default for ConnectionStatusTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokio_listener_survives_full_channel() {
        let tracker = ConnectionStatusTracker::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tracker.register_listener(tx);

        tracker.update_status(ConnectionStatus::Connecting);
        // Channel is full: this update is dropped but the listener must stay.
        tracker.update_status(ConnectionStatus::Reconnecting);
        assert_eq!(rx.try_recv().unwrap(), ConnectionStatus::Connecting);

        tracker.update_status(ConnectionStatus::Connected);
        assert_eq!(rx.try_recv().unwrap(), ConnectionStatus::Connected);

        drop(rx);
        tracker.update_status(ConnectionStatus::Disconnected);
        assert!(tracker.listeners.read().unwrap().is_empty());
    }
}
