use std::{
    collections::VecDeque,
    ops::{Deref, DerefMut},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use bitcoincore_rpc::Client;
use tracing::warn;

use super::{RpcClientError, RpcClientProvider};

/// Smallest in-flight limit we accept. The indexer holds up to two clients while
/// fetcher threads acquire more, so a lower limit could deadlock it.
const MIN_IN_FLIGHT: usize = 4;

// Wrapper that automatically returns the client to the pool when dropped
pub struct PooledClient {
    client: Option<Client>,
    pool: RpcClientPool,
}

impl Deref for PooledClient {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        self.client.as_ref().expect("Client was taken")
    }
}

impl DerefMut for PooledClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.client.as_mut().expect("Client was taken")
    }
}

impl Drop for PooledClient {
    fn drop(&mut self) {
        let client = self.client.take();
        self.pool.release(client);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcClientPoolError {
    #[error("lock poisoned")]
    LockPoisoned,

    #[error("timed out waiting for a free RPC client")]
    Exhausted,

    #[error("provider error: {0}")]
    Provider(#[from] RpcClientError),
}

struct PoolState {
    idle: VecDeque<Client>,
    in_flight: usize,
}

struct PoolInner {
    state: Mutex<PoolState>,
    available: Condvar,
    max_idle: usize,
    max_in_flight: usize,
    provider: Arc<dyn RpcClientProvider>,
}

/// Pool of bitcoind RPC clients. At most `max_in_flight` clients are checked
/// out at once; `get` blocks until one is released.
#[derive(Clone)]
pub struct RpcClientPool {
    inner: Arc<PoolInner>,
}

impl RpcClientPool {
    pub fn new(
        provider: Arc<dyn RpcClientProvider>,
        max_idle: usize,
        max_in_flight: usize,
    ) -> Self {
        if max_in_flight < MIN_IN_FLIGHT {
            warn!(
                "RPC in-flight limit {} is below the minimum of {}, using {}",
                max_in_flight, MIN_IN_FLIGHT, MIN_IN_FLIGHT
            );
        }

        Self {
            inner: Arc::new(PoolInner {
                state: Mutex::new(PoolState {
                    idle: VecDeque::new(),
                    in_flight: 0,
                }),
                available: Condvar::new(),
                max_idle,
                max_in_flight: max_in_flight.max(MIN_IN_FLIGHT),
                provider,
            }),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, PoolState>, RpcClientPoolError> {
        self.inner
            .state
            .lock()
            .map_err(|_| RpcClientPoolError::LockPoisoned)
    }

    /// Get a client, blocking until the in-flight count is below the limit.
    pub fn get(&self) -> Result<PooledClient, RpcClientPoolError> {
        self.acquire(None)
    }

    /// Like `get`, but gives up with `RpcClientPoolError::Exhausted` after `timeout`.
    pub fn get_timeout(&self, timeout: Duration) -> Result<PooledClient, RpcClientPoolError> {
        self.acquire(Some(timeout))
    }

    fn acquire(&self, timeout: Option<Duration>) -> Result<PooledClient, RpcClientPoolError> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut state = self.lock()?;

        while state.in_flight >= self.inner.max_in_flight {
            state = match deadline {
                None => self
                    .inner
                    .available
                    .wait(state)
                    .map_err(|_| RpcClientPoolError::LockPoisoned)?,
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(RpcClientPoolError::Exhausted);
                    }
                    self.inner
                        .available
                        .wait_timeout(state, remaining)
                        .map_err(|_| RpcClientPoolError::LockPoisoned)?
                        .0
                }
            };
        }

        state.in_flight += 1;
        let idle = state.idle.pop_front();
        drop(state);

        let client = match idle {
            Some(client) => client,
            None => match self.inner.provider.get_new_rpc_client() {
                Ok(client) => client,
                Err(e) => {
                    self.release(None);
                    return Err(e.into());
                }
            },
        };

        Ok(PooledClient {
            client: Some(client),
            pool: self.clone(),
        })
    }

    /// Release an in-flight permit, returning the client to the idle queue if there is room.
    fn release(&self, client: Option<Client>) {
        // Never panic in Drop: recover the guard even if the mutex is poisoned.
        let mut state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };

        state.in_flight = state.in_flight.saturating_sub(1);
        if let Some(client) = client {
            if state.idle.len() < self.inner.max_idle {
                state.idle.push_back(client);
            }
        }
        drop(state);

        self.inner.available.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProvider;

    impl RpcClientProvider for TestProvider {
        fn get_new_rpc_client(&self) -> Result<Client, RpcClientError> {
            Client::new("http://127.0.0.1:1", bitcoincore_rpc::Auth::None)
                .map_err(|e| RpcClientError::FailedToConnect(e.to_string()))
        }
    }

    #[test]
    fn get_is_bounded_by_in_flight_limit_and_released_on_drop() {
        let pool = RpcClientPool::new(Arc::new(TestProvider), 10, MIN_IN_FLIGHT);

        let held: Vec<_> = (0..MIN_IN_FLIGHT).map(|_| pool.get().unwrap()).collect();
        assert!(matches!(
            pool.get_timeout(Duration::from_millis(20)),
            Err(RpcClientPoolError::Exhausted)
        ));

        drop(held);
        assert!(pool.get_timeout(Duration::from_millis(20)).is_ok());
    }
}
