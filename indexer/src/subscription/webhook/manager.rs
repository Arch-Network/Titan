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
    tokio::sync::Semaphore,
    uuid::Uuid,
};

const MAX_CONCURRENT_WEBHOOK_DELIVERIES: usize = 32;
const MAX_WEBHOOK_SUBSCRIPTIONS: usize = 100;

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
}

pub struct SubscriptionManager {
    store: Arc<dyn Store>,
    mutation_lock: Mutex<()>,
    delivery_semaphore: Arc<Semaphore>,
}

impl SubscriptionManager {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            mutation_lock: Mutex::new(()),
            delivery_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_WEBHOOK_DELIVERIES)),
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

    pub async fn broadcast(&self, event: &Event) -> Result<(), Box<dyn std::error::Error>> {
        process_event(&self.store, &self.delivery_semaphore, event).await
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

    #[test]
    fn enforces_subscription_limit_without_blocking_updates() {
        let manager = SubscriptionManager::new(Arc::new(MemoryStore::default()));

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
        let manager = SubscriptionManager::new(Arc::new(MemoryStore::default()));
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
}
