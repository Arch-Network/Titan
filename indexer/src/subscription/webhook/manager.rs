use {
    super::{
        endpoint::{validate_endpoint, EndpointError, PublicOnlyResolver},
        process_event,
        store::{Store, StoreError},
    },
    arch_titan_types_api::Subscription,
    arch_titan_types_core::{Event, EventType},
    reqwest::{redirect, Client},
    rustc_hash::FxHashMap as HashMap,
    std::{
        sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    tokio::sync::Semaphore,
    uuid::Uuid,
};

/// Maximum number of registered webhook subscriptions.
pub const MAX_SUBSCRIPTIONS: usize = 100;
/// Maximum number of webhook deliveries (including retries) in flight at once.
const MAX_IN_FLIGHT_DELIVERIES: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    InvalidEndpoint(#[from] EndpointError),
    #[error("subscription limit of {0} reached")]
    LimitReached(usize),
    #[error("failed to build webhook http client: {0}")]
    Client(#[from] reqwest::Error),
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Webhook subscriptions, persisted in the store and cached in memory so event
/// dispatch never touches the database.
pub struct SubscriptionManager {
    store: Arc<dyn Store>,
    client: Client,
    cache: RwLock<HashMap<Uuid, Subscription>>,
    deliveries: Arc<Semaphore>,
    allow_private_endpoints: bool,
}

impl SubscriptionManager {
    pub fn new(
        store: Arc<dyn Store>,
        allow_private_endpoints: bool,
    ) -> Result<Self, SubscriptionError> {
        let mut builder = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(redirect::Policy::none())
            .no_proxy();
        if !allow_private_endpoints {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder.build()?;

        let cache = store
            .get_subscriptions()?
            .into_iter()
            .map(|sub| (sub.id, sub))
            .collect();

        Ok(Self {
            store,
            client,
            cache: RwLock::new(cache),
            deliveries: Arc::new(Semaphore::new(MAX_IN_FLIGHT_DELIVERIES)),
            allow_private_endpoints,
        })
    }

    fn read_cache(&self) -> RwLockReadGuard<'_, HashMap<Uuid, Subscription>> {
        self.cache.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_cache(&self) -> RwLockWriteGuard<'_, HashMap<Uuid, Subscription>> {
        self.cache.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Register a new subscription. The id and last-success timestamp are
    /// assigned by the server; client-supplied values are ignored.
    pub async fn add_subscription(
        &self,
        endpoint: &str,
        event_types: Vec<EventType>,
    ) -> Result<Subscription, SubscriptionError> {
        let url = validate_endpoint(endpoint, self.allow_private_endpoints).await?;

        let subscription = Subscription {
            id: Uuid::new_v4(),
            endpoint: url.to_string(),
            event_types,
            last_success_epoch_secs: now_secs(),
        };

        // Hold the write lock across the count check and insert so concurrent
        // requests cannot exceed the cap.
        let mut cache = self.write_cache();
        if cache.len() >= MAX_SUBSCRIPTIONS {
            return Err(SubscriptionError::LimitReached(MAX_SUBSCRIPTIONS));
        }
        self.store.set_subscription(&subscription)?;
        cache.insert(subscription.id, subscription.clone());

        Ok(subscription)
    }

    pub fn delete_subscription(&self, id: &Uuid) -> Result<(), StoreError> {
        let mut cache = self.write_cache();
        self.store.delete_subscription(id)?;
        cache.remove(id);
        Ok(())
    }

    pub fn get_subscriptions(&self) -> Vec<Subscription> {
        self.read_cache().values().cloned().collect()
    }

    pub fn get_subscription(&self, id: &Uuid) -> Result<Subscription, StoreError> {
        self.read_cache()
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(format!("subscription {id}")))
    }

    pub(super) fn mark_success(&self, id: &Uuid, epoch_secs: u64) -> Result<(), StoreError> {
        let mut cache = self.write_cache();
        let Some(sub) = cache.get_mut(id) else {
            // Deleted while the delivery was in flight.
            return Ok(());
        };
        self.store
            .update_subscription_last_success(id, epoch_secs)?;
        sub.last_success_epoch_secs = epoch_secs;
        Ok(())
    }

    /// Ids of subscriptions whose last success is older than `expiry_secs`.
    pub(super) fn expired_subscription_ids(&self, now_secs: u64, expiry_secs: u64) -> Vec<Uuid> {
        self.read_cache()
            .values()
            .filter(|sub| now_secs.saturating_sub(sub.last_success_epoch_secs) > expiry_secs)
            .map(|sub| sub.id)
            .collect()
    }

    /// (id, endpoint) of every subscription interested in `event_type`.
    pub(super) fn interested(&self, event_type: &EventType) -> Vec<(Uuid, String)> {
        self.read_cache()
            .values()
            .filter(|sub| sub.event_types.contains(event_type))
            .map(|sub| (sub.id, sub.endpoint.clone()))
            .collect()
    }

    pub(super) fn client(&self) -> &Client {
        &self.client
    }

    pub(super) fn deliveries(&self) -> &Arc<Semaphore> {
        &self.deliveries
    }

    /// Queue `event` for delivery to all interested subscriptions. Never blocks:
    /// deliveries beyond the in-flight cap are dropped.
    pub fn broadcast(self: &Arc<Self>, event: &Event) {
        process_event(self, event);
    }
}
