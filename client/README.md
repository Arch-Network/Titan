# Titan Client for Rust

**Titan Client** is a Rust library that provides both HTTP and TCP clients for interacting with the [Titan Indexer](https://github.com/SaturnBTC/titan) for Bitcoin and runes. The indexer itself is written in Rust and indexes Bitcoin blockchain data along with runic metadata. This client package allows you to:

- **Query data** such as node status, block tips, transactions, addresses, inscriptions, and runes using HTTP.
- **Broadcast transactions** to the Bitcoin network.
- **Subscribe to real-time events** (e.g. new blocks, transaction updates, rune events) over a TCP socket.

> **Note:** The TCP subscription client is available in two variants:
>
> - **Asynchronous TCP Client:** Uses Tokio and works in async contexts.
> - **Blocking TCP Client:** Works in synchronous environments.
>
> Both require enabling the appropriate feature flags (`tcp_client` or `tcp_client_blocking`).

---

## Features

- **Asynchronous HTTP Client:**
  - Built on top of `reqwest` and uses async/await.
  - Provides methods to fetch status, blocks, transactions, addresses, inscriptions, runes, and subscriptions.
- **Synchronous (Blocking) HTTP Client:**
  - Uses `reqwest::blocking` for environments that do not use async.
- **TCP Subscription Clients:**
  - Asynchronous and blocking versions to subscribe to real-time events.
  - Automatic reconnection is supported in the asynchronous client.
- **Comprehensive API:**
  - Both clients support a wide array of endpoints, including broadcast and query endpoints for transactions, outputs, inscriptions, runes, and more.

---

## Requirements

- **Rust 1.56+** (using the 2021 edition).
- Bitcoin Node 27.0 (https://bitcoincore.org/bin/bitcoin-core-27.0/)
- A running instance of the Titan Indexer (HTTP on a specified port and TCP for event subscriptions).
- For the TCP clients, note that the async version uses Tokio while the blocking version uses standard Rust threads and non-blocking I/O.

Detailed Setup: Follow the [Setup Instructions](../SetupInstructions.md) for step-by-step guidance.

---

## Installation

Run the following Cargo command in your project directory:
```bash
cargo add arch-titan-client
```

Or add the following line to your Cargo.toml:
```toml
[dependencies]
arch-titan-client = "0.1.31"
```

## Usage

### Asynchronous HTTP Client

The asynchronous client uses reqwest with async/await. Create an instance of TitanClient (which re-exports the async client) and call its methods.

#### Example

```rust
use arch_titan_client::TitanApi; // Re-export of TitanApiAsync
use arch_titan_client::TitanClient;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create an async client with the Titan Indexer base URL.
    let client = TitanClient::new("http://localhost:3030");

    // Retrieve the indexer status.
    let status = client.get_status().await?;
    println!("Status: {:?}", status);

    // Retrieve the current block tip.
    let tip = client.get_tip().await?;
    println!("Block Tip: {:?}", tip);

    // Retrieve address data.
    let address_data = client.get_address("your-bitcoin-address").await?;
    println!("Address Data: {:?}", address_data);

    Ok(())
}
```

### Synchronous (Blocking) HTTP Client

For environments that do not support async/await, use the blocking client (re-exported as TitanBlockingClient).

#### Example

```rust
use arch_titan_client::TitanApiBlocking; // Re-export of TitanApiSync
use arch_titan_client::TitanBlockingClient;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a blocking client.
    let client = TitanBlockingClient::new("http://localhost:3030");

    // Retrieve the indexer status.
    let status = client.get_status()?;
    println!("Status: {:?}", status);

    // Retrieve the current block tip.
    let tip = client.get_tip()?;
    println!("Block Tip: {:?}", tip);

    // Retrieve address data.
    let address_data = client.get_address("your-bitcoin-address")?;
    println!("Address Data: {:?}", address_data);

    Ok(())
}
```

### TCP Subscription Client (Asynchronous)

This client subscribes to real-time events (e.g. new blocks, transaction updates, runic events) over a TCP socket. It uses Tokio and supports auto-reconnection.

**Important**: This client works only in async environments (i.e. in Node.js–like setups or within Tokio applications).

#### Example

```rust
use arch_titan_client::{subscribe_to_titan, TcpClientError};
use titan_types::{Event, TcpSubscriptionRequest, EventType};
use tokio::{sync::watch, time::{sleep, Duration}};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // TCP server address (e.g., "127.0.0.1:8080").
    let addr = "127.0.0.1:8080";

    // Prepare a subscription request.
    let subscription_request = TcpSubscriptionRequest {
        subscribe: vec![
            EventType::TransactionsAdded,
            EventType::NewBlock,
        ],
    };

    // Create a shutdown channel to gracefully signal shutdown.
    let (shutdown_tx, shutdown_rx) = watch::channel(());

    // Subscribe to events.
    let mut event_receiver = subscribe_to_titan(addr, subscription_request, shutdown_rx).await?;

    // Spawn a task to process incoming events.
    tokio::spawn(async move {
        while let Some(event) = event_receiver.recv().await {
            println!("Received event: {:?}", event);
        }
    });

    // Run for 10 seconds then signal shutdown.
    sleep(Duration::from_secs(10)).await;
    let _ = shutdown_tx.send(());

    Ok(())
}
```

### TCP Subscription Client (Blocking)

If you need a synchronous TCP subscription client, enable the `tcp_client_blocking` feature and use the blocking API.

#### Example

```rust
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use arch_titan_client::{subscribe_to_titan_blocking, TcpClientError};
use titan_types::{Event, TcpSubscriptionRequest, EventType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:9000";
    let subscription_request = TcpSubscriptionRequest {
        subscribe: vec![
            EventType::TransactionsAdded,
            EventType::NewBlock,
        ],
    };

    // Use an atomic flag to signal shutdown.
    let shutdown_flag = Arc::new(AtomicBool::new(false));

    // Subscribe to events.
    let event_receiver = subscribe_to_titan_blocking(addr, subscription_request, shutdown_flag.clone())?;

    // Spawn a thread to process events.
    thread::spawn(move || {
        for event in event_receiver {
            println!("Received event: {:?}", event);
        }
    });

    // Let the subscription run for 10 seconds.
    thread::sleep(Duration::from_secs(10));
    shutdown_flag.store(true, Ordering::SeqCst);
    println!("Shutting down TCP subscription.");

    Ok(())
}
```

## API Reference

### Asynchronous HTTP Client (TitanClient / TitanApiAsync)

- **get_status()**: `Result<Status, Error>`  
  Retrieves the indexer's status, including network information and block height.

- **get_tip()**: `Result<BlockTip, Error>`  
  Retrieves the current best block tip (height and hash).

- **get_block(query: &query::Block)**: `Result<Block, Error>`  
  Fetches details for a block using either the block height or hash.

- **get_block_hash_by_height(height: u64)**: `Result<String, Error>`  
  Returns the block hash for the specified height.

- **get_block_txids(query: &query::Block)**: `Result<Vec<String>, Error>`  
  Retrieves a list of transaction IDs in a block.

- **get_address(address: &str)**: `Result<AddressData, Error>`  
  Retrieves information for a Bitcoin address (balance, outputs, etc.).

- **get_transaction(txid: &str)**: `Result<Transaction, Error>`  
  Retrieves a detailed transaction object, including runic information.

- **get_transaction_raw(txid: &str)**: `Result<Vec<u8>, Error>`  
  Retrieves the raw transaction bytes.

- **get_transaction_hex(txid: &str)**: `Result<String, Error>`  
  Retrieves the transaction in hexadecimal format.

- **send_transaction(tx_hex: String)**: `Result<Txid, Error>`  
  Broadcasts a transaction to the network.

- **get_output(outpoint: &str)**: `Result<TxOutEntry, Error>`  
  Retrieves a specific transaction output by its outpoint.

- **get_inscription(inscription_id: &str)**: `Result<(HeaderMap, Vec<u8>), Error>`  
  Retrieves an inscription's headers and data.

- **get_runes(pagination: Option<Pagination>)**: `Result<PaginationResponse<RuneResponse>, Error>`  
  Retrieves a paginated list of runes.

- **get_rune(rune: &str)**: `Result<RuneResponse, Error>`  
  Retrieves information for a specific rune.

- **get_rune_transactions(rune: &str, pagination: Option<Pagination>)**: `Result<PaginationResponse<Txid>, Error>`  
  Retrieves transactions involving a given rune.

- **get_mempool_txids()**: `Result<Vec<Txid>, Error>`  
  Retrieves the current mempool transaction IDs.

- **get_mempool_entry(txid: &str)**: `Result<MempoolEntry, Error>`  
  Retrieves a specific mempool entry by its txid.

- **get_mempool_entries(txids: &[Txid])**: `Result<HashMap<Txid, Option<MempoolEntry>>, Error>`  
  Retrieves multiple mempool entries by their txids.

- **get_all_mempool_entries()**: `Result<HashMap<Txid, MempoolEntry>, Error>`  
  Retrieves all mempool entries.

- **get_subscription(id: &str)**: `Result<Subscription, Error>`  
  Retrieves a subscription by its ID.

- **list_subscriptions()**: `Result<Vec<Subscription>, Error>`  
  Lists all subscriptions.

- **add_subscription(subscription: &Subscription)**: `Result<Subscription, Error>`  
  Adds a new subscription.

- **delete_subscription(id: &str)**: `Result<(), Error>`  
  Deletes a subscription by its ID.

### Synchronous HTTP Client (TitanBlockingClient / TitanApiSync)

Provides the same set of methods as the async client, but in a blocking (synchronous) manner.

### TCP Subscription Clients

#### Asynchronous TCP Client:

- Use `subscribe_to_titan(addr, subscription_request, shutdown_rx)` to subscribe.
- Returns a Tokio mpsc receiver that streams incoming events.

#### Synchronous (Blocking) TCP Client:

- Use `subscribe_to_titan_blocking(addr, subscription_request, shutdown_flag)` to subscribe.
- Returns a standard mpsc receiver for events.
