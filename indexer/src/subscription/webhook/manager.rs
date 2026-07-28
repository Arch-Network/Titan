use {
    super::{
        process_event,
        store::{Store, StoreError},
        validate_webhook_url, EndpointError,
    },
    arch_titan_types_api::Subscription,
    arch_titan_types_core::Event,
    std::sync::{Arc, Mutex},
    thiserror::Error,
    tokio::sync::{
        mpsc::{self, error::TrySendError},
        Semaphore,
    },
    tracing::error,
    uuid::Uuid,
};

const MAX_CONCURRENT_WEBHOOK_DELIVERIES: usize = 32;
const MAX_WEBHOOK_SUBSCRIPTIONS: usize = 100;
const MAX_QUEUED_WEBHOOK_EVENTS: usize = 64;

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    InvalidEndpoint(#[from] EndpointError),
    #[error("webhook subscription limit reached ({limit})")]
    LimitExceeded { limit: usize },
    #[error("webhook subscription mutation lock is poisoned")]
    MutationLockPoisoned,
    #[error("webhook delivery queue is full ({capacity} events)")]
    DeliveryQueueFull { capacity: usize },
    #[error("webhook delivery queue is closed")]
    DeliveryQueueClosed,
}

pub struct SubscriptionManager {
    store: Arc<dyn Store>,
    mutation_lock: Mutex<()>,
    delivery_sender: mpsc::Sender<Event>,
}

impl SubscriptionManager {
    pub fn new(store: Arc<dyn Store>) -> Self {
        let (delivery_sender, mut delivery_receiver) = mpsc::channel(MAX_QUEUED_WEBHOOK_EVENTS);
        let delivery_store = store.clone();
        let delivery_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_WEBHOOK_DELIVERIES));

        tokio::spawn(async move {
            while let Some(event) = delivery_receiver.recv().await {
                if let Err(error) =
                    process_event(&delivery_store, &delivery_semaphore, &event).await
                {
                    error!("Error processing queued webhook event: {error}");
                }
            }
        });

        Self {
            store,
            mutation_lock: Mutex::new(()),
            delivery_sender,
        }
    }

    pub fn add_subscription(&self, subscription: &Subscription) -> Result<(), ManagerError> {
        validate_webhook_url(&subscription.endpoint)?;
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| ManagerError::MutationLockPoisoned)?;
        let subscriptions = self.store.get_subscriptions()?;
        let already_exists = subscriptions
            .iter()
            .any(|existing| existing.id == subscription.id);

        if !already_exists && subscriptions.len() >= MAX_WEBHOOK_SUBSCRIPTIONS {
            return Err(ManagerError::LimitExceeded {
                limit: MAX_WEBHOOK_SUBSCRIPTIONS,
            });
        }

        self.store.set_subscription(subscription)?;
        Ok(())
    }

    pub fn delete_subscription(&self, id: &Uuid) -> Result<(), ManagerError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| ManagerError::MutationLockPoisoned)?;
        self.store.delete_subscription(id)?;
        Ok(())
    }

    pub fn get_subscriptions(&self) -> Result<Vec<Subscription>, ManagerError> {
        Ok(self.store.get_subscriptions()?)
    }

    pub fn get_subscription(&self, id: &Uuid) -> Result<Subscription, ManagerError> {
        Ok(self.store.get_subscription(id)?)
    }

    pub fn broadcast(&self, event: &Event) -> Result<(), ManagerError> {
        self.delivery_sender
            .try_send(event.clone())
            .map_err(|error| match error {
                TrySendError::Full(_) => ManagerError::DeliveryQueueFull {
                    capacity: self.delivery_sender.max_capacity(),
                },
                TrySendError::Closed(_) => ManagerError::DeliveryQueueClosed,
            })
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        arch_titan_types_core::EventType,
        std::{
            collections::HashMap,
            sync::{Mutex as StdMutex, MutexGuard},
        },
    };

    #[derive(Default)]
    struct MemoryStore {
        subscriptions: StdMutex<HashMap<Uuid, Subscription>>,
    }

    impl MemoryStore {
        fn subscriptions(&self) -> MutexGuard<'_, HashMap<Uuid, Subscription>> {
            self.subscriptions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    impl Store for MemoryStore {
        fn set_subscription(&self, subscription: &Subscription) -> Result<(), StoreError> {
            self.subscriptions()
                .insert(subscription.id, subscription.clone());
            Ok(())
        }

        fn update_subscription_last_success(
            &self,
            id: &Uuid,
            last_success: u64,
        ) -> Result<(), StoreError> {
            let mut subscriptions = self.subscriptions();
            let subscription = subscriptions
                .get_mut(id)
                .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            subscription.last_success_epoch_secs = last_success;
            Ok(())
        }

        fn get_subscription(&self, id: &Uuid) -> Result<Subscription, StoreError> {
            self.subscriptions()
                .get(id)
                .cloned()
                .ok_or_else(|| StoreError::NotFound(id.to_string()))
        }

        fn get_subscriptions(&self) -> Result<Vec<Subscription>, StoreError> {
            Ok(self.subscriptions().values().cloned().collect())
        }

        fn delete_subscription(&self, id: &Uuid) -> Result<(), StoreError> {
            self.subscriptions()
                .remove(id)
                .map(|_| ())
                .ok_or_else(|| StoreError::NotFound(id.to_string()))
        }
    }

    fn subscription(id: u128) -> Subscription {
        Subscription {
            id: Uuid::from_u128(id),
            endpoint: "https://example.com/webhook".to_string(),
            event_types: vec![EventType::NewBlock],
            last_success_epoch_secs: 0,
        }
    }

    fn manager(store: Arc<dyn Store>) -> SubscriptionManager {
        let (delivery_sender, _delivery_receiver) = mpsc::channel(1);
        SubscriptionManager {
            store,
            mutation_lock: Mutex::new(()),
            delivery_sender,
        }
    }

    #[test]
    fn enforces_subscription_limit_without_blocking_updates() {
        let manager = manager(Arc::new(MemoryStore::default()));

        for id in 1..=MAX_WEBHOOK_SUBSCRIPTIONS as u128 {
            manager.add_subscription(&subscription(id)).unwrap();
        }

        assert!(matches!(
            manager.add_subscription(&subscription(MAX_WEBHOOK_SUBSCRIPTIONS as u128 + 1)),
            Err(ManagerError::LimitExceeded {
                limit: MAX_WEBHOOK_SUBSCRIPTIONS
            })
        ));

        let mut updated = subscription(1);
        updated.last_success_epoch_secs = 42;
        manager.add_subscription(&updated).unwrap();
        assert_eq!(
            manager
                .get_subscription(&updated.id)
                .unwrap()
                .last_success_epoch_secs,
            42
        );
    }

    #[test]
    fn rejects_private_endpoint_before_storage() {
        let manager = manager(Arc::new(MemoryStore::default()));
        let mut subscription = subscription(1);
        subscription.endpoint = "http://169.254.169.254/latest/meta-data".to_string();

        assert!(matches!(
            manager.add_subscription(&subscription),
            Err(ManagerError::InvalidEndpoint(EndpointError::HostNotPublic(
                _
            )))
        ));
        assert!(manager.get_subscriptions().unwrap().is_empty());
    }

    #[test]
    fn webhook_queue_is_bounded_and_non_blocking() {
        let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
        let (delivery_sender, mut delivery_receiver) = mpsc::channel(1);
        let manager = SubscriptionManager {
            store,
            mutation_lock: Mutex::new(()),
            delivery_sender,
        };
        let event = Event::TransactionsAdded { txids: Vec::new() };

        manager.broadcast(&event).unwrap();
        assert!(matches!(
            manager.broadcast(&event),
            Err(ManagerError::DeliveryQueueFull { capacity: 1 })
        ));
        assert_eq!(delivery_receiver.try_recv().unwrap(), event);

        drop(delivery_receiver);
        assert!(matches!(
            manager.broadcast(&event),
            Err(ManagerError::DeliveryQueueClosed)
        ));
    }
}
