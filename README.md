# Titan - High-Performance Bitcoin Indexer

Titan is a next-gen Bitcoin indexer by Saturn, designed for real-time blockchain access, optimized queries, and seamless Runes protocol integration. It delivers superior performance with minimal resource consumption.

## Key Features

- Optimized Performance – Fast, efficient queries tailored for dApps and validator nodes.
- Real-Time Blockchain Access – Instant data retrieval for responsive applications.
- Scalable & Lightweight – High throughput with minimal computational overhead.
- Advanced Runes Integration – Native support for Runes tokens, instant token data, and secure transaction validation.
- Mempool-Level Indexing for Runes – Differentiates pending vs confirmed runes in outputs from transactions in the mempool.

## Requirements

- Bitcoin Core >= 27.0 (the Docker setup uses `bitcoin/bitcoin:29.4`)
- Rust 1.94.0 (the toolchain used by CI and the Docker image; older toolchains are untested)
- A C/C++ toolchain and `libclang` (RocksDB is compiled from source by the `rocksdb` crate)

## How to build it

```bash
cargo build --release -p titan
```

## How to run it

Every option can be given as a command-line flag or as an environment variable named after
the flag in UPPER_SNAKE case (`--bitcoin-rpc-url` ↔ `BITCOIN_RPC_URL`). Flags take
precedence over environment variables. Prefer the environment for secrets so they don't show
up in the process list:

```bash
export BITCOIN_RPC_PASSWORD=<PASSWORD>
cargo run --release -p titan -- \
  --bitcoin-rpc-url http://localhost:<PORT> \
  --bitcoin-rpc-username <USERNAME> \
  --chain regtest \
  --index-addresses \
  --enable-tcp-subscriptions \
  --data-dir ~/titan-indexer
```

To run Titan and Bitcoin Core with Docker Compose, see [DOCKER_SETUP.md](./DOCKER_SETUP.md).

### Options

| Flag | Environment variable | Default | Description |
|------|----------------------|---------|-------------|
| `--bitcoin-rpc-url` | `BITCOIN_RPC_URL` | `http://localhost:8332` | Bitcoin Core RPC URL (include the scheme) |
| `--bitcoin-rpc-username` | `BITCOIN_RPC_USERNAME` | – | RPC username |
| `--bitcoin-rpc-password` | `BITCOIN_RPC_PASSWORD` | – | RPC password |
| `--cookie-file` | `COOKIE_FILE` | – | RPC cookie file (used instead of username/password) |
| `--bitcoin-rpc-limit` | `BITCOIN_RPC_LIMIT` | `12` | Max RPC requests in flight (minimum 4) |
| `--bitcoin-rpc-pool-size` | `BITCOIN_RPC_POOL_SIZE` | `500` | Max idle RPC clients kept in the pool |
| `--chain` | `CHAIN` | `mainnet` | `mainnet` (alias `main`), `testnet` (alias `test`), `testnet4`, `signet`, `regtest` |
| `--data-dir` | `DATA_DIR` | `./data` | Index directory |
| `--index-addresses` | `INDEX_ADDRESSES` | `false` | Index outputs by address |
| `--index-bitcoin-transactions` | `INDEX_BITCOIN_TRANSACTIONS` | `true` | Index bitcoin transactions |
| `--index-spent-outputs` | `INDEX_SPENT_OUTPUTS` | `true` | Keep spent outputs in the index |
| `--no-index-inscriptions` | `NO_INDEX_INSCRIPTIONS` | `false` | Do not index inscriptions (rune icons) |
| `--commit-interval` | `COMMIT_INTERVAL` | `50` | Commit interval in blocks |
| `--main-loop-interval` | `MAIN_LOOP_INTERVAL` | `500` | Main loop interval in milliseconds |
| `--http-listen` | `HTTP_LISTEN` | `0.0.0.0:3030` | HTTP API listen address |
| `--csp-origin` | `CSP_ORIGIN` | – | Origin for the Content-Security-Policy header |
| `--decompress` | `DECOMPRESS` | `false` | Decompress brotli-encoded inscription content (DoS risk) |
| `--enable-tcp-subscriptions` | `ENABLE_TCP_SUBSCRIPTIONS` | `false` | Enable the TCP subscription server |
| `--tcp-address` | `TCP_ADDRESS` | `127.0.0.1:8080` | TCP subscription listen address (max 1024 connections, 16 per IP; clients idle for 300 s without a PING are dropped) |
| `--enable-webhook-subscriptions` | `ENABLE_WEBHOOK_SUBSCRIPTIONS` | `false` | Enable webhook subscriptions |
| `--webhook-admin-token` | `WEBHOOK_ADMIN_TOKEN` | – | Bearer token for the webhook subscription API (required when webhooks are enabled) |
| `--webhook-allow-private-endpoints` | `WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` | `false` | Allow webhook endpoints on loopback/private/link-local addresses |
| `--enable-zmq-listener` | `ENABLE_ZMQ_LISTENER` | `false` | Read mempool transactions from bitcoind's ZMQ feed |
| `--zmq-endpoint` | `ZMQ_ENDPOINT` | `tcp://127.0.0.1:28332` | bitcoind `zmqpubrawtx` endpoint |
| `--enable-file-logging` | `ENABLE_FILE_LOGGING` | `false` | Write subscription events to a log file |
| `--events-log-path` | `EVENTS_LOG_PATH` | `<DATA_DIR>/events.log` | Event log file used by `--enable-file-logging` |

Boolean options accept an optional value, so a default of `true` can be turned off:
`--index-spent-outputs` and `--index-spent-outputs=true` enable it, while
`--index-spent-outputs=false`, `--index-spent-outputs false` or `INDEX_SPENT_OUTPUTS=false`
disable it.

### Webhook subscriptions

When `--enable-webhook-subscriptions` is set, Titan refuses to start without
`--webhook-admin-token` (or `WEBHOOK_ADMIN_TOKEN`). Every webhook subscription route
(`GET /subscriptions`, `POST /subscription`, `GET`/`DELETE /subscription/{id}`) then requires
`Authorization: Bearer <token>` and returns `401` otherwise. Webhook URLs that resolve to
loopback, private or link-local addresses are rejected unless
`--webhook-allow-private-endpoints` is set (needed when receivers run in the same compose
network):

```bash
curl -H "Authorization: Bearer $WEBHOOK_ADMIN_TOKEN" http://localhost:3030/subscriptions
```

### Upgrading: database schema v2

This release uses index schema version 2 (rune transaction keys changed and the genesis rune
is now indexed). Titan refuses to open an index created by an earlier release and reports a
schema mismatch. There is no in-place migration: stop Titan, delete the data directory
(`--data-dir`, or the `titan-data` volume with Docker) and let it re-index from scratch.

### Upgrading: paginated address and mempool endpoints (breaking)

Endpoints that returned unbounded lists are now paginated with the same `skip`/`limit`
query parameters and `{ items, offset }` response as `/runes` (pass `offset` as the next
`skip`; an empty page means the end):

| Before | Now |
| --- | --- |
| `GET /address/{address}` → `{ value, runes, outputs }` | `GET /address/{address}` → `{ value, runes, output_count }` |
| (outputs inlined above) | `GET /address/{address}/outputs?skip&limit` → page of outputs (max 50) |
| `GET /mempool/txids` → `[txid]` | `GET /mempool/txids?skip&limit` → page of txids (default and max 1000) |
| `GET /mempool/entries/all` → `{ txid: entry }` | `GET /mempool/entries?skip&limit` → page of `{ txid, entry }` (max 50) |

The Rust client (`get_address_outputs`, `get_mempool_txids(pagination)`,
`list_mempool_entries`) and the TypeScript SDK (`getAddressOutputs`,
`getMempoolTxids(pagination)`, `listMempoolEntries`) are updated to match, so this
release is a new major version of the published crates.

## Client SDKs

Titan provides official client SDKs for easy integration:

- Rust SDK – [Titan Client for Rust](./client/README.md)

- TypeScript SDK – [Titan Client for TypeScript](./ts-sdk/README.md)

Each SDK has its own README with detailed installation and usage instructions.

# Running Titan with Bitcoind Using Docker

This guide explains how to build and run the Titan indexer together with a Bitcoin Core node (Bitcoind) using Docker and docker-compose. Both the Titan Dockerfile and the docker-compose file are configured for production-style deployment and can be easily adapted.

## Prerequisites

- **Docker & Docker Compose:** Make sure you have Docker and Docker Compose installed.
- **Git:** Required to build the Titan image from source.
- **.env Configuration:** Copy `.env.example` to `.env` and set `BITCOIN_RPC_PASSWORD` (required; e.g. `openssl rand -hex 32`). Set `BITCOIN_NETWORK` (`main`, `test`, `testnet4`, `signet` or `regtest`) and, to enable webhooks, `ENABLE_WEBHOOK_SUBSCRIPTIONS=true` plus `WEBHOOK_ADMIN_TOKEN`.

## How to run

The compose files orchestrate two services on a custom network (titan-network):

- bitcoind:
  Runs the pinned Bitcoin Core image (`bitcoin/bitcoin:29.4`) on `BITCOIN_NETWORK` with `-txindex`. Its RPC port accepts connections only from the compose network and is not published on the host (the dev compose file publishes it on 127.0.0.1).

- titan:
  Uses the published image (`docker-compose.yml`) or builds it from the local Dockerfile (`docker-compose.dev.yml`). It waits for bitcoind's health check and is configured through environment variables only.

Run the services with the published image:

```bash
docker compose up -d
```

or build from source:

```bash
docker compose -f docker-compose.dev.yml up -d --build
```

See [DOCKER_SETUP.md](./DOCKER_SETUP.md) for all variables, image tags and the schema v2 upgrade steps.
