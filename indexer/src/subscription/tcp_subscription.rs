use arch_titan_types_api::TcpSubscriptionRequest;
use arch_titan_types_core::{Event, EventType};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::error::TrySendError;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, RwLock, Semaphore},
};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const MAX_LINE: usize = 8 * 1024;
const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// A subscription coming from a TCP client.
#[derive(Debug)]
pub struct TcpSubscription {
    pub id: Uuid,
    /// The set of event types (as strings) the client wants.
    pub event_types: HashSet<EventType>,
    /// Channel sender to deliver events to this client.
    pub sender: mpsc::Sender<Event>,
}

/// Manages all active TCP subscriptions.
#[derive(Default, Debug)]
pub struct TcpSubscriptionManager {
    subscriptions: RwLock<HashMap<Uuid, TcpSubscription>>,
}

impl TcpSubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(HashMap::default()),
        }
    }

    /// Register a new TCP subscription.
    pub async fn register(&self, sub: TcpSubscription) {
        self.subscriptions.write().await.insert(sub.id, sub);
    }

    /// Unregister a subscription by its id.
    pub async fn unregister(&self, id: Uuid) {
        self.subscriptions.write().await.remove(&id);
    }

    /// Broadcast an event to all subscriptions that have registered interest.
    pub async fn broadcast(&self, event: &Event) {
        // Assume you can derive a string event type from your event.
        // For example, if you have a function or trait implementation:
        let event_type: EventType = EventType::from(event.clone()); // adjust as needed

        let subs = self.subscriptions.read().await;
        let mut failed_ids = Vec::new();

        for (id, sub) in subs.iter() {
            if sub.event_types.contains(&event_type) {
                // Non-blocking send to avoid stalling the dispatcher on slow clients.
                match sub.sender.try_send(event.clone()) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        info!(
                            "Dropping event and evicting slow TCP subscriber {} due to full channel",
                            id
                        );
                        failed_ids.push(*id);
                    }
                    Err(TrySendError::Closed(_)) => {
                        info!(
                            "Evicting TCP subscriber {} because its channel is closed",
                            id
                        );
                        failed_ids.push(*id);
                    }
                }
            }
        }

        // Drop the read lock before removing subscriptions
        drop(subs);

        // Remove any subscriptions that failed to receive events
        for id in failed_ids {
            self.unregister(id).await;
            info!("Unregistered failed subscription with id {}", id);
        }
    }
}

/// Maximum concurrent TCP subscription connections.
const MAX_CONNECTIONS: usize = 1024;
/// Maximum concurrent TCP subscription connections from one IP address.
const MAX_CONNECTIONS_PER_IP: usize = 16;
/// Close a connection if the client sends nothing (e.g. `PING`) for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Close a connection if a single write to it takes longer than this.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Backoff after a failed `accept` (e.g. EMFILE) before trying again.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// Per-IP connection counts; a slot is released when its guard drops.
#[derive(Default)]
struct IpConnections {
    counts: std::sync::Mutex<HashMap<IpAddr, usize>>,
}

struct IpSlot {
    ip: IpAddr,
    connections: Arc<IpConnections>,
}

impl IpConnections {
    fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Option<IpSlot> {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let count = counts.entry(ip).or_insert(0);
        if *count >= MAX_CONNECTIONS_PER_IP {
            return None;
        }
        *count += 1;
        Some(IpSlot {
            ip,
            connections: self.clone(),
        })
    }
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut counts = self
            .connections
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = counts.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.ip);
            }
        }
    }
}

/// Run the TCP subscription server on an already-bound listener.
/// This server accepts incoming TCP connections and spawns a task
/// to handle each connection, until a shutdown signal arrives.
pub async fn run_tcp_subscription_server(
    listener: TcpListener,
    manager: Arc<TcpSubscriptionManager>,
    mut shutdown_rx: watch::Receiver<()>,
) {
    let connection_permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let ip_connections = Arc::new(IpConnections::default());

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (socket, remote_addr) = match accept_result {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        // Accept errors (EMFILE, ECONNABORTED, ...) are transient.
                        warn!("TCP subscription accept error: {:?}", e);
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                        continue;
                    }
                };

                let Ok(permit) = connection_permits.clone().try_acquire_owned() else {
                    warn!("Rejecting TCP connection from {}: connection limit reached", remote_addr);
                    continue;
                };
                let Some(ip_slot) = ip_connections.try_acquire(remote_addr.ip()) else {
                    warn!("Rejecting TCP connection from {}: per-IP connection limit reached", remote_addr);
                    continue;
                };

                info!("New TCP connection from {}", remote_addr);
                let manager_clone = manager.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ip_slot = ip_slot;
                    if let Err(e) = handle_tcp_connection(socket, manager_clone).await {
                        error!("Error handling TCP connection from {}: {:?}", remote_addr, e);
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                info!("TCP Subscription Server shutting down");
                break;
            }
        }
    }
}

async fn write_line(writer: &mut OwnedWriteHalf, bytes: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_all(bytes).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "write timed out"))?
}

/// Handle a single TCP connection:
/// 1. Read a line (JSON) from the client specifying the event types to subscribe to.
/// 2. Create an mpsc channel and register a subscription.
/// 3. Forward events from the channel to the client.
/// 4. Also monitor the connection (for PINGs or disconnection); the connection is closed
///    when the client disconnects, stays silent for `IDLE_TIMEOUT`, or a write stalls.
async fn handle_tcp_connection(
    socket: TcpStream,
    manager: Arc<TcpSubscriptionManager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Split the socket into reader and writer.
    let (reader, mut writer) = socket.into_split();
    let mut lines = FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_LINE));

    // Read subscription request with timeout.
    let request: TcpSubscriptionRequest = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        read_handshake_request(&mut lines, &mut writer),
    )
    .await?
    .map_err(|e| e.to_string())?;

    info!("Received TCP subscription request: {:?}", request);

    let event_types: HashSet<EventType> = request.subscribe.into_iter().collect();

    // Create an mpsc channel for delivering events to this connection.
    let (tx, mut rx) = mpsc::channel::<Event>(100);
    let sub = TcpSubscription {
        id: Uuid::new_v4(),
        event_types,
        sender: tx,
    };
    let sub_id = sub.id;
    manager.register(sub).await;
    info!("Registered TCP subscription with id {}", sub_id);

    let result = forward_events(&mut lines, &mut writer, &mut rx, sub_id).await;

    manager.unregister(sub_id).await;
    info!("Unregistered TCP subscription with id {}", sub_id);
    result
}

async fn forward_events(
    lines: &mut FramedRead<OwnedReadHalf, LinesCodec>,
    writer: &mut OwnedWriteHalf,
    rx: &mut mpsc::Receiver<Event>,
    sub_id: Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            // Send events received from the channel to the client.
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        let json_bytes = serde_json::to_vec(&event)?;
                        write_line(writer, &json_bytes).await?;
                    },
                    None => {
                        info!("Event channel closed for subscription {}", sub_id);
                        return Ok(());
                    }
                }
            }
            // Also monitor the connection for any client input (to detect disconnect).
            result = lines.next() => {
                match result {
                    Some(Ok(line)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                        let trimmed = line.trim();
                        if trimmed == "PING" {
                            write_line(writer, b"PONG").await?;
                        } else {
                            debug!("Received message from client: {}", trimmed);
                        }
                    }
                    Some(Err(e)) => {
                        error!("Error reading from TCP connection: {:?}", e);
                        return Ok(());
                    }
                    None => {
                        info!("TCP client disconnected for subscription {}", sub_id);
                        return Ok(());
                    }
                }
            }
            _ = &mut idle => {
                info!("Closing idle TCP subscription {}", sub_id);
                return Ok(());
            }
        }
    }
}

async fn read_handshake_request(
    lines: &mut FramedRead<OwnedReadHalf, LinesCodec>,
    writer: &mut OwnedWriteHalf,
) -> Result<TcpSubscriptionRequest, Box<dyn std::error::Error>> {
    // Be tolerant to empty lines and early PINGs from clients or probes.
    // Put a small cap on how many preamble lines we accept to avoid abuse.
    const MAX_PREAMBLE_LINES: usize = 10;
    let mut preamble_lines = 0usize;
    loop {
        match lines.next().await {
            Some(Ok(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    preamble_lines += 1;
                    if preamble_lines >= MAX_PREAMBLE_LINES {
                        return Err("Too many empty lines before subscription request".into());
                    }
                    continue;
                }
                if trimmed == "PING" {
                    if let Err(e) = writer.write_all(b"PONG\n").await {
                        error!("Failed to send PONG: {:?}", e);
                        return Err("Failed to send PONG".into());
                    }
                    if let Err(e) = writer.flush().await {
                        error!("Failed to flush after PONG: {:?}", e);
                        return Err("Failed to flush after PONG".into());
                    }
                    preamble_lines += 1;
                    if preamble_lines >= MAX_PREAMBLE_LINES {
                        return Err("Too many preamble lines before subscription request".into());
                    }
                    continue;
                }
                if trimmed.starts_with('{') {
                    match serde_json::from_str::<TcpSubscriptionRequest>(trimmed) {
                        Ok(req) => return Ok(req),
                        Err(e) => {
                            error!(
                                "Failed to parse TCP subscription request JSON: {} (data starts: {:?})",
                                e,
                                &trimmed.chars().take(80).collect::<String>()
                            );
                            return Err("Invalid subscription request JSON".into());
                        }
                    }
                } else {
                    debug!(
                        "Ignoring non-JSON preface before subscription: {:?}",
                        &trimmed.chars().take(80).collect::<String>()
                    );
                    preamble_lines += 1;
                    if preamble_lines >= MAX_PREAMBLE_LINES {
                        return Err(
                            "Too many non-JSON preface lines before subscription request".into(),
                        );
                    }
                    continue;
                }
            }
            Some(Err(e)) => {
                error!("Error reading handshake line: {:?}", e);
                return Err(e.into());
            }
            None => {
                return Err("Connection closed before sending subscription request".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::codec::{FramedRead, LinesCodec};

    async fn setup_pair(
    ) -> Result<(TcpStream, OwnedReadHalf, OwnedWriteHalf), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let client = TcpStream::connect(addr).await?;
        let (server, _) = listener.accept().await?;
        let (server_read, server_write) = server.into_split();
        Ok((client, server_read, server_write))
    }

    #[tokio::test]
    async fn handshake_success_with_ping_and_json() -> Result<(), Box<dyn std::error::Error>> {
        let (client, server_read, mut server_write) = setup_pair().await?;
        let mut client_reader = BufReader::new(client);

        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));

        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        // Send an empty line, a PING (expect PONG), then a valid JSON subscription
        let client_stream = client_reader.get_mut();
        client_stream.write_all(b"\n").await?;
        client_stream.write_all(b"PING\n").await?;
        client_stream
            .write_all(b"{\"subscribe\":[\"RuneEtched\",\"NewBlock\"]}\n")
            .await?;
        client_stream.flush().await?;

        // Read PONG from server
        let mut pong_line = String::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            client_reader.read_line(&mut pong_line),
        )
        .await??;
        assert_eq!(pong_line, "PONG\n");

        // Wait for server to parse the JSON
        let req = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        let req = req?;
        assert_eq!(req.subscribe.len(), 2);
        assert_eq!(req.subscribe[0], EventType::RuneEtched);
        assert_eq!(req.subscribe[1], EventType::NewBlock);
        Ok(())
    }

    #[tokio::test]
    async fn handshake_errors_on_too_many_empty_lines() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, server_read, mut server_write) = setup_pair().await?;
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));

        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        // Send many empty lines to exceed preamble limit inside the function
        for _ in 0..20 {
            client.write_all(b"\n").await?;
        }
        client.flush().await?;

        let res = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn handshake_errors_on_invalid_json() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, server_read, mut server_write) = setup_pair().await?;
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));

        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        client.write_all(b"{invalid}\n").await?;
        client.flush().await?;

        let res = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn handshake_errors_on_oversized_line() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, server_read, mut server_write) = setup_pair().await?;
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));

        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        // Create a line longer than MAX_LINE
        let long = "a".repeat(MAX_LINE + 1);
        client.write_all(long.as_bytes()).await?;
        client.write_all(b"\n").await?;
        client.flush().await?;

        let res = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn handshake_ignores_noise_then_accepts_json() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, server_read, mut server_write) = setup_pair().await?;
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));
        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        client.write_all(b"GET / HTTP/1.1\n").await?;
        client.write_all(b"ping\n").await?; // lowercase, should be ignored (no PONG)
        client.write_all(b"   \n").await?; // whitespace line
        client
            .write_all(b"{\"subscribe\":[\"NewBlock\"]}\n")
            .await?;
        client.flush().await?;

        let req = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        let req = req?;
        assert_eq!(req.subscribe, vec![EventType::NewBlock]);
        Ok(())
    }

    #[tokio::test]
    async fn handshake_many_pings_then_json_under_cap() -> Result<(), Box<dyn std::error::Error>> {
        let (client, server_read, mut server_write) = setup_pair().await?;
        let mut client_reader = BufReader::new(client);
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));
        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        // 9 PINGs is under the 10-line cap
        for _ in 0..9 {
            client_reader.get_mut().write_all(b"PING\n").await?;
        }
        client_reader
            .get_mut()
            .write_all(b"{\"subscribe\":[\"Reorg\"]}\n")
            .await?;
        client_reader.get_mut().flush().await?;

        for _ in 0..9 {
            let mut pong = String::new();
            tokio::time::timeout(Duration::from_secs(1), client_reader.read_line(&mut pong))
                .await??;
            assert_eq!(pong, "PONG\n");
        }

        let req = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        let req = req?;
        assert_eq!(req.subscribe, vec![EventType::Reorg]);
        Ok(())
    }

    #[tokio::test]
    async fn handshake_too_many_pings_errors_and_sends_all_pongs(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (client, server_read, mut server_write) = setup_pair().await?;
        let mut client_reader = BufReader::new(client);
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));
        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        // 10 PINGs hits the cap (>= 10) and should error after responding to the 10th
        for _ in 0..10 {
            client_reader.get_mut().write_all(b"PING\n").await?;
        }
        client_reader.get_mut().flush().await?;

        for _ in 0..10 {
            let mut pong = String::new();
            tokio::time::timeout(Duration::from_secs(1), client_reader.read_line(&mut pong))
                .await??;
            assert_eq!(pong, "PONG\n");
        }

        let res = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn handshake_trims_whitespace_and_crlf() -> Result<(), Box<dyn std::error::Error>> {
        let (client, server_read, mut server_write) = setup_pair().await?;
        let mut client_reader = BufReader::new(client);
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));
        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        client_reader.get_mut().write_all(b"  PING  \r\n").await?;
        client_reader
            .get_mut()
            .write_all(b"  {\"subscribe\":[\"RuneMinted\"]}  \r\n")
            .await?;
        client_reader.get_mut().flush().await?;

        let mut pong = String::new();
        tokio::time::timeout(Duration::from_secs(1), client_reader.read_line(&mut pong)).await??;
        assert_eq!(pong, "PONG\n");

        let req = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        let req = req?;
        assert_eq!(req.subscribe, vec![EventType::RuneMinted]);
        Ok(())
    }

    #[tokio::test]
    async fn handshake_invalid_event_type_errors() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, server_read, mut server_write) = setup_pair().await?;
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));
        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        client
            .write_all(b"{\"subscribe\":[\"NotAnEvent\"]}\n")
            .await?;
        client.flush().await?;

        let res = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn handshake_connection_closed_before_request() -> Result<(), Box<dyn std::error::Error>>
    {
        let (client, server_read, mut server_write) = setup_pair().await?;
        let mut lines = FramedRead::new(server_read, LinesCodec::new_with_max_length(MAX_LINE));
        let server_task = tokio::spawn(async move {
            read_handshake_request(&mut lines, &mut server_write)
                .await
                .map_err(|e| e.to_string())
        });

        drop(client); // close without sending anything

        let res = tokio::time::timeout(Duration::from_secs(1), server_task).await??;
        assert!(res.is_err());
        Ok(())
    }
}
