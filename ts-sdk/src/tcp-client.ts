import * as net from 'net';
import { EventEmitter } from 'events';
import * as readline from 'readline';
import { TcpSubscriptionRequest } from './types';

/**
 * Options for the TCP client.
 */
export interface TcpClientOptions {
  autoReconnect?: boolean; // Should the client attempt to reconnect on disconnection?
  heartbeatIntervalMs?: number; // Interval between client PINGs (ms)
  heartbeatTimeoutMs?: number; // Time to wait for PONG before considering dead (ms)
  // Exponential backoff options (if provided, used instead of fixed reconnectDelayMs)
  maxRetries?: number; // Maximum reconnect attempts (undefined => infinite)
  baseDelayMs?: number; // Base delay for backoff (defaults to 1000ms)
  maxDelayMs?: number; // Max delay cap for backoff (defaults to 60000ms)
  jitterRatio?: number; // 0..1 jitter fraction applied to delay (defaults to 0.3)
  maxLineLengthBytes?: number; // Max allowed incoming line length before reconnect (defaults to 10 MiB)
}

/**
 * A TCP client class for subscribing to events from the Titan service.
 * This version automatically attempts to reconnect if the connection is lost.
 *
 * It emits:
 *   - "event": when a new event is received.
 *   - "error": when an error occurs.
 *   - "close": when the connection is closed.
 *   - "reconnect": when a reconnection attempt is made.
 *
 * Usage example:
 *
 *   const tcpClient = new TitanTcpClient('localhost', 4000, { autoReconnect: true, reconnectDelayMs: 5000 });
 *   tcpClient.on('event', (event) => console.log('Received event:', event));
 *   tcpClient.subscribe({ subscribe: ['RuneEtched', 'RuneMinted'] });
 *
 *   // To shut down:
 *   tcpClient.shutdown();
 */
export class TitanTcpClient extends EventEmitter {
  private socket: net.Socket | null = null;
  private rl: readline.Interface | null = null;
  private subscriptionRequest: TcpSubscriptionRequest | null = null;
  private shuttingDown = false;
  private connecting = false;

  // Options for reconnection behavior.
  private autoReconnect: boolean;
  private heartbeatIntervalMs: number;
  private heartbeatTimeoutMs: number;
  private maxRetries?: number;
  private baseDelayMs: number;
  private maxDelayMs: number;
  private jitterRatio: number;
  private maxLineLengthBytes: number;

  // Heartbeat state
  private heartbeatInterval: NodeJS.Timeout | null = null;
  private heartbeatTimeout: NodeJS.Timeout | null = null;
  private awaitingPong = false;

  // Reconnect/backoff state
  private reconnectAttempt = 0;
  private reconnectTimer: NodeJS.Timeout | null = null;

  // Status lifecycle
  private status: ConnectionStatus = 'Disconnected';

  /**
   * @param addr The hostname or IP address of the TCP subscription server.
   * @param port The port number of the TCP subscription server.
   * @param options Optional configuration for reconnection behavior.
   */
  constructor(
    private addr: string,
    private port: number,
    options?: TcpClientOptions,
  ) {
    super();
    this.autoReconnect = options?.autoReconnect ?? false;
    this.heartbeatIntervalMs = options?.heartbeatIntervalMs ?? 30000;
    this.heartbeatTimeoutMs = options?.heartbeatTimeoutMs ?? 10000;
    this.maxRetries = options?.maxRetries;
    this.baseDelayMs = options?.baseDelayMs ?? 1000;
    this.maxDelayMs = options?.maxDelayMs ?? 60000;
    this.jitterRatio = options?.jitterRatio ?? 0.3;
    this.maxLineLengthBytes = options?.maxLineLengthBytes ?? 10 * 1024 * 1024;
  }

  /**
   * Initiates the subscription process. The provided subscription request will be
   * stored and used for all (re)connections.
   *
   * @param subscriptionRequest The subscription request (e.g. { subscribe: ['RuneEtched', 'RuneMinted'] }).
   */
  subscribe(subscriptionRequest: TcpSubscriptionRequest): void {
    this.subscriptionRequest = subscriptionRequest;
    this.shuttingDown = false;
    // Tear down any existing connection synchronously (its listeners are
    // detached, so its deferred 'close' cannot affect the new connection),
    // then connect with the new request regardless of autoReconnect.
    this.cleanup();
    this.reconnectAttempt = 0;
    this.connect();
  }

  /**
   * Creates a TCP connection and sets up event listeners.
   */
  private connect(): void {
    if (!this.subscriptionRequest) {
      this.emit('error', new Error('No subscription request provided.'));
      return;
    }

    if (this.connecting || this.socket) {
      return; // Already connecting/connected
    }

    this.updateStatus(
      this.reconnectAttempt > 0 ? 'Reconnecting' : 'Connecting',
    );
    this.connecting = true;

    // Create a new TCP connection. Every handler checks that it still belongs
    // to the current socket so events from replaced sockets are ignored.
    const socket = net.createConnection(this.port, this.addr, () => {
      if (this.socket !== socket) return;
      // On connection, send the subscription request as JSON (terminated by newline).
      const reqJson = JSON.stringify(this.subscriptionRequest);
      socket.write(reqJson + '\n');
      // Enable TCP keepalive
      socket.setKeepAlive(true, 30000);
      this.emit('reconnect'); // notify that a (re)connection has occurred.
      this.startHeartbeat();
      // Backoff is reset only once the session proves healthy (first event
      // or PONG), so a server that accepts and immediately closes still
      // backs off.
      this.updateStatus('Connected');
      this.connecting = false;
    });
    this.socket = socket;

    socket.on('error', (err) => {
      if (this.socket !== socket) return;
      this.emit('error', err);
    });

    // Set up a readline interface to handle incoming lines.
    const rl = readline.createInterface({ input: socket });
    this.rl = rl;
    rl.on('line', (line: string) => {
      if (this.socket !== socket) return;
      try {
        const trimmed = line.trim();
        // Max line length protection (approximate by UTF-8 length)
        const lineBytes = Buffer.byteLength(line, 'utf8');
        if (lineBytes > this.maxLineLengthBytes) {
          this.emit(
            'error',
            new Error(
              `Incoming line exceeds max length (${lineBytes} > ${this.maxLineLengthBytes})`,
            ),
          );
          // Force reconnect
          socket.destroy(new Error('Max line length exceeded'));
          return;
        }
        if (trimmed === 'PONG') {
          // Heartbeat response
          this.onPong();
          return;
        }
        const event = JSON.parse(line);
        // Receiving a valid event also proves liveness
        this.onPong();
        this.emit('event', event);
      } catch (err) {
        this.emit('error', new Error(`Failed to parse event: ${line}`));
      }
    });

    // Handle connection closure.
    socket.on('close', () => {
      if (this.socket !== socket) return;
      this.emit('close');
      this.cleanup();
      // If explicitly shut down, mark disconnected and exit
      if (this.shuttingDown) {
        this.updateStatus('Disconnected');
        return;
      }
      // If the client has not been explicitly shut down and autoReconnect is enabled,
      // try to reconnect after a delay.
      if (this.autoReconnect) {
        this.scheduleReconnect();
      } else {
        this.updateStatus('Disconnected');
      }
    });
  }

  /**
   * Cleans up the socket and readline interface. Listeners are detached so the
   * old socket's deferred events cannot affect a subsequent connection.
   */
  private cleanup(): void {
    this.stopHeartbeat();
    this.clearReconnectTimer();
    this.connecting = false;
    if (this.rl) {
      this.rl.removeAllListeners();
      this.rl.close();
      this.rl = null;
    }
    if (this.socket) {
      const socket = this.socket;
      this.socket = null;
      socket.removeAllListeners();
      // Swallow late errors from the discarded socket.
      socket.on('error', () => {});
      socket.destroy();
    }
  }

  /**
   * Shuts down the TCP client, canceling any pending reconnection attempts.
   */
  shutdown(): void {
    this.shuttingDown = true;
    const hadSocket = this.socket !== null;
    this.cleanup();
    if (hadSocket) this.emit('close');
    this.updateStatus('Disconnected');
  }

  /**
   * Gracefully shuts down and resolves after the socket is closed and timers cleared.
   */
  async shutdownAsync(): Promise<void> {
    if (this.shuttingDown) return;
    const socket = this.socket;
    this.shutdown();
    if (socket && !socket.closed) {
      await new Promise<void>((resolve) => socket.once('close', () => resolve()));
    }
  }

  private startHeartbeat(): void {
    this.stopHeartbeat();
    if (!this.socket) return;
    this.awaitingPong = false;
    this.heartbeatInterval = setInterval(() => {
      if (!this.socket) return;
      if (this.awaitingPong) {
        // Still waiting for previous PONG; skip sending another PING
        return;
      }
      this.sendPing();
    }, this.heartbeatIntervalMs);
  }

  private stopHeartbeat(): void {
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval);
      this.heartbeatInterval = null;
    }
    if (this.heartbeatTimeout) {
      clearTimeout(this.heartbeatTimeout);
      this.heartbeatTimeout = null;
    }
    this.awaitingPong = false;
  }

  private sendPing(): void {
    if (!this.socket) return;
    try {
      this.socket.write('PING\n');
      this.awaitingPong = true;
      if (this.heartbeatTimeout) {
        clearTimeout(this.heartbeatTimeout);
      }
      this.heartbeatTimeout = setTimeout(() => {
        if (!this.awaitingPong) return;
        this.emit('error', new Error('Heartbeat timeout (no PONG received)'));
        // Force reconnect by destroying the socket; 'close' handler will schedule reconnect
        if (this.socket) {
          this.socket.destroy(new Error('Heartbeat timeout'));
        }
      }, this.heartbeatTimeoutMs);
    } catch (e) {
      this.emit('error', e as Error);
    }
  }

  private onPong(): void {
    this.awaitingPong = false;
    // A PONG or event proves the session is healthy: reset backoff.
    this.reconnectAttempt = 0;
    if (this.heartbeatTimeout) {
      clearTimeout(this.heartbeatTimeout);
      this.heartbeatTimeout = null;
    }
  }

  private scheduleReconnect(): void {
    this.updateStatus('Reconnecting');
    if (
      this.maxRetries !== undefined &&
      this.reconnectAttempt >= this.maxRetries
    ) {
      this.emit('error', new Error('Maximum reconnect attempts reached'));
      this.updateStatus('Disconnected');
      return;
    }
    const exp = Math.min(30, this.reconnectAttempt); // prevent overflow
    const raw = Math.min(
      this.maxDelayMs,
      this.baseDelayMs * Math.pow(2, exp),
    );
    const jitter = raw * this.jitterRatio * (Math.random() - 0.5) * 2; // +/- jitterRatio
    const delay = Math.max(0, Math.floor(raw + jitter));
    this.reconnectAttempt += 1;
    this.clearReconnectTimer();
    this.reconnectTimer = setTimeout(() => {
      this.connect();
    }, delay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private updateStatus(next: ConnectionStatus): void {
    if (this.status === next) return;
    this.status = next;
    this.emit('status', next);
  }

  getStatus(): ConnectionStatus {
    return this.status;
  }
}

export type ConnectionStatus =
  | 'Connecting'
  | 'Connected'
  | 'Reconnecting'
  | 'Disconnected';
