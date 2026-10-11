use {
    crate::bitcoin_rpc::{BitcoinCoreRpcResultExt, RpcClientPool},
    bitcoin::Block,
    bitcoincore_rpc::{Client, RpcApi},
    std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc, Arc, Condvar, Mutex,
        },
        thread,
        time::Duration,
    },
    threadpool::ThreadPool,
    tracing::{error, trace, warn},
};

/// Simple counting semaphore for synchronizing buffer capacity between producer and consumer threads.
///
/// The semaphore can be closed, which wakes every waiter and makes all current and future
/// `acquire` calls fail. This is used to release producers once the consumer side of a fetch is
/// gone, so no worker stays blocked after the fetch ends.
#[derive(Clone, Debug)]
struct Semaphore {
    inner: Arc<(Mutex<SemaphoreState>, Condvar)>,
}

#[derive(Debug)]
struct SemaphoreState {
    permits: usize,
    closed: bool,
}

impl Semaphore {
    /// Create a new semaphore that allows `permits` concurrent acquisitions.
    fn new(permits: usize) -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(SemaphoreState {
                    permits,
                    closed: false,
                }),
                Condvar::new(),
            )),
        }
    }

    /// Block the current thread until a permit can be acquired.
    ///
    /// Returns `false` without acquiring a permit if the semaphore is (or gets) closed.
    fn acquire(&self) -> bool {
        let (mutex, cvar) = &*self.inner;
        // Enter standard Condvar pattern: guard + loop on predicate.
        let mut state = mutex.lock().expect("Semaphore mutex poisoned");
        loop {
            if state.closed {
                return false;
            }
            if state.permits > 0 {
                state.permits -= 1;
                return true;
            }
            state = match cvar.wait(state) {
                Ok(guard) => guard, // spurious wake-up or notify()
                Err(poison) => panic!("Semaphore condvar poisoned: {poison}"),
            };
        }
    }

    /// Release a previously-acquired permit, unblocking one waiting thread if any.
    fn release(&self) {
        let (mutex, cvar) = &*self.inner;
        let mut state = mutex.lock().expect("Semaphore mutex poisoned");
        state.permits += 1;
        cvar.notify_one();
    }

    /// Close the semaphore, waking every waiter. All subsequent `acquire` calls fail.
    fn close(&self) {
        let (mutex, cvar) = &*self.inner;
        let mut state = mutex.lock().expect("Semaphore mutex poisoned");
        state.closed = true;
        cvar.notify_all();
    }

    fn is_closed(&self) -> bool {
        let (mutex, _) = &*self.inner;
        mutex.lock().expect("Semaphore mutex poisoned").closed
    }
}

/// Owned by the reorder thread; dropping it marks the fetch as finished. It closes the buffer
/// semaphore (unblocking and cancelling pending fetch jobs) and disconnects the stats logging
/// thread, so no helper thread outlives the fetch.
struct FetchDoneGuard {
    buffer_semaphore: Semaphore,
    _stop_logging: mpsc::Sender<()>,
}

impl Drop for FetchDoneGuard {
    fn drop(&mut self) {
        self.buffer_semaphore.close();
    }
}

#[derive(Debug, Default, Clone)]
pub struct BlockFetcherStats {
    /// Number of blocks currently being fetched (requested but not yet completed)
    pub blocks_being_fetched: Arc<AtomicU64>,
    /// Total number of blocks sent to the intermediate channel
    pub blocks_sent_to_intermediate: Arc<AtomicU64>,
    /// Total number of blocks sent to the final channel
    pub blocks_sent_to_final: Arc<AtomicU64>,
    /// Current number of blocks in the reordering buffer
    pub blocks_in_buffer: Arc<AtomicU64>,
}

impl BlockFetcherStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the counter for blocks being fetched
    pub fn increment_fetching(&self) {
        self.blocks_being_fetched.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the counter for blocks being fetched (when fetch completes)
    pub fn decrement_fetching(&self) {
        self.blocks_being_fetched.fetch_sub(1, Ordering::Relaxed);
    }

    /// Increment the counter for blocks sent to intermediate channel
    pub fn increment_intermediate(&self) {
        self.blocks_sent_to_intermediate
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_intermediate(&self) {
        self.blocks_sent_to_intermediate
            .fetch_sub(1, Ordering::Relaxed);
    }

    /// Increment the counter for blocks sent to final channel
    pub fn increment_final(&self) {
        self.blocks_sent_to_final.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_final(&self) {
        self.blocks_sent_to_final.fetch_sub(1, Ordering::Relaxed);
    }

    /// Increment the counter for blocks in buffer
    pub fn increment_buffer(&self) {
        self.blocks_in_buffer.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the counter for blocks in buffer
    pub fn decrement_buffer(&self) {
        self.blocks_in_buffer.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get the current number of blocks being fetched
    pub fn get_blocks_being_fetched(&self) -> u64 {
        self.blocks_being_fetched.load(Ordering::Relaxed)
    }

    /// Get the total number of blocks sent to intermediate channel
    pub fn get_blocks_sent_to_intermediate(&self) -> u64 {
        self.blocks_sent_to_intermediate.load(Ordering::Relaxed)
    }

    /// Get the total number of blocks sent to final channel
    pub fn get_blocks_sent_to_final(&self) -> u64 {
        self.blocks_sent_to_final.load(Ordering::Relaxed)
    }

    /// Get the current number of blocks in buffer
    pub fn get_blocks_in_buffer(&self) -> u64 {
        self.blocks_in_buffer.load(Ordering::Relaxed)
    }

    /// Get a snapshot of all stats
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.get_blocks_being_fetched(),
            self.get_blocks_sent_to_intermediate(),
            self.get_blocks_sent_to_final(),
            self.get_blocks_in_buffer(),
        )
    }
}

pub enum BlockLocator {
    Height(u64),
}

pub fn fetch_blocks_from(
    bitcoin_rpc_pool: RpcClientPool,
    start_height: u64,
    limit: u64,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<(mpsc::Receiver<Block>, BlockFetcherStats), bitcoincore_rpc::Error> {
    fetch_blocks_from_with_buffer_limit(bitcoin_rpc_pool, start_height, limit, 500, shutdown_flag)
}

pub fn fetch_blocks_from_with_buffer_limit(
    bitcoin_rpc_pool: RpcClientPool,
    start_height: u64,
    limit: u64,
    max_buffer_size: u64,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<(mpsc::Receiver<Block>, BlockFetcherStats), bitcoincore_rpc::Error> {
    let stats = BlockFetcherStats::new();
    let buffer_semaphore = Semaphore::new(max_buffer_size as usize); // Limit buffer size

    // Final channel for ordered blocks.
    let (final_sender, final_rx) = mpsc::sync_channel(32);

    // Intermediate channel for unordered (height, block) tuples.
    let (intermediate_sender, intermediate_receiver) = mpsc::sync_channel(1000);

    // Disconnected when the reorder thread (and thus this fetch) finishes.
    let (stop_logging_tx, stop_logging_rx) = mpsc::channel::<()>();

    // Spawn a logging thread to report stats every second. Stops when this fetch finishes or
    // `shutdown_flag` is set.
    thread::spawn({
        let stats = stats.clone();
        let shutdown_flag = shutdown_flag.clone();
        move || {
            while !shutdown_flag.load(Ordering::SeqCst) {
                match stop_logging_rx.recv_timeout(Duration::from_secs(1)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let (fetching, intermediate, final_count, buffer) = stats.snapshot();
                tracing::debug!(
                    "Block fetcher stats - Fetching: {}, Intermediate: {}, Final: {}, Buffer: {}",
                    fetching,
                    intermediate,
                    final_count,
                    buffer
                );
            }
            trace!("Block fetcher logging thread terminating");
        }
    });

    // Spawn a thread to reorder blocks. Terminates when `shutdown_flag` is set or channels close.
    // Its exit marks the end of the fetch (see `FetchDoneGuard`).
    thread::spawn({
        let final_sender = final_sender.clone();
        let stats = stats.clone();
        let buffer_semaphore = buffer_semaphore.clone();
        let shutdown_flag = shutdown_flag.clone();
        let done_guard = FetchDoneGuard {
            buffer_semaphore: buffer_semaphore.clone(),
            _stop_logging: stop_logging_tx,
        };
        move || {
            let _done_guard = done_guard;
            let mut next_expected = start_height;
            let mut buffer = BTreeMap::new();
            loop {
                if shutdown_flag.load(Ordering::SeqCst) {
                    trace!("Reorder thread terminating due to shutdown signal");
                    return;
                }

                match intermediate_receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok((height, block)) => {
                        stats.decrement_intermediate();

                        if height == next_expected {
                            // Release the buffer permit that was acquired by the producer for this block.
                            buffer_semaphore.release();

                            if final_sender.send(block).is_err() {
                                trace!("Final receiver disconnected");
                                return;
                            }
                            stats.increment_final();
                            next_expected += 1;
                            while let Some(block) = buffer.remove(&next_expected) {
                                stats.decrement_buffer();
                                buffer_semaphore.release(); // Release buffer space held for this buffered block
                                if final_sender.send(block).is_err() {
                                    warn!("Final receiver disconnected");
                                    return;
                                }
                                stats.increment_final();
                                next_expected += 1;
                            }
                        } else {
                            // Insert out-of-order block into the buffer. Capacity for this block was already
                            // reserved by the producer before the network fetch, so we can safely insert
                            // without acquiring here.
                            buffer.insert(height, block);
                            stats.increment_buffer();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Periodically check shutdown flag even if no data is coming through.
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        trace!("Intermediate sender disconnected");
                        return;
                    }
                }
            }
        }
    });

    // Create a thread pool sized for I/O-bound tasks.
    let num_threads = 50;
    let pool = ThreadPool::new(num_threads);
    for height in start_height..limit {
        let bitcoin_rpc_pool = bitcoin_rpc_pool.clone();
        let intermediate_sender = intermediate_sender.clone();
        let stats = stats.clone();
        let buffer_semaphore = buffer_semaphore.clone();
        let shutdown_flag = shutdown_flag.clone();

        stats.increment_fetching();
        pool.execute(move || {
            if shutdown_flag.load(Ordering::SeqCst) {
                // If shutdown has already been requested, skip fetching.
                stats.decrement_fetching();
                return;
            }

            // Reserve buffer capacity BEFORE starting the expensive RPC call. This guarantees that
            // once the block is fetched there will always be space for it in the in-memory buffer
            // or the downstream channels, preventing deadlocks when an earlier block is delayed.
            // Fails once the fetch has ended (consumer gone or shutdown), in which case we skip.
            if !buffer_semaphore.acquire() {
                stats.decrement_fetching();
                return;
            }

            let client = match bitcoin_rpc_pool.get() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to create RPC client for height {}: {}", height, e);
                    stats.decrement_fetching();
                    buffer_semaphore.release(); // Release reserved buffer slot on error
                    return;
                }
            };

            let cancelled = || shutdown_flag.load(Ordering::SeqCst) || buffer_semaphore.is_closed();
            let Some(result) =
                get_block_with_retries_until(&client, BlockLocator::Height(height), cancelled)
            else {
                trace!("Fetch of block {} cancelled", height);
                buffer_semaphore.release();
                stats.decrement_fetching();
                return;
            };

            match result {
                Ok(Some(block)) => {
                    if intermediate_sender.send((height, block)).is_err() {
                        trace!("Intermediate receiver disconnected");
                        // Intermediate receiver hung up – free the reserved buffer slot.
                        buffer_semaphore.release();
                    } else {
                        stats.increment_intermediate();
                    }
                }
                Ok(None) => {
                    error!("Block not found for height {}", height);
                    buffer_semaphore.release(); // Release reserved buffer slot on error
                }
                Err(err) => {
                    error!("Failed to fetch block {}: {}", height, err);
                    buffer_semaphore.release(); // Release reserved buffer slot on error
                }
            }

            stats.decrement_fetching();
        });
    }

    Ok((final_rx, stats))
}

/// Fetches a block, retrying failed attempts with exponential backoff, but gives up (returning
/// `None`) as soon as `cancelled` returns true, so a worker never keeps retrying after its fetch
/// has ended.
fn get_block_with_retries_until(
    client: &Client,
    locator: BlockLocator,
    cancelled: impl Fn() -> bool,
) -> Option<Result<Option<Block>, bitcoincore_rpc::Error>> {
    let mut errors = 0;
    loop {
        if cancelled() {
            return None;
        }
        match get_block_attempt(client, &locator) {
            Err(err) => {
                errors += 1;
                let seconds = (1u64 << errors.min(7)).min(120);
                let loc_str = match locator {
                    BlockLocator::Height(h) => h.to_string(),
                };
                warn!(
                    "Failed to fetch block {}: retrying in {}s: {}",
                    loc_str, seconds, err
                );
                // Sleep in short steps so cancellation is noticed promptly.
                for _ in 0..seconds {
                    if cancelled() {
                        return None;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
            Ok(result) => return Some(Ok(result)),
        }
    }
}

fn get_block_attempt(
    client: &Client,
    locator: &BlockLocator,
) -> Result<Option<Block>, bitcoincore_rpc::Error> {
    match locator {
        BlockLocator::Height(height) => {
            let hash = client.get_block_hash(*height).into_option()?;
            hash.map(|hash| client.get_block(&hash)).transpose()
        }
    }
}
