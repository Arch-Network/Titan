# Titan - High-Performance Bitcoin Indexer

Titan is a next-gen Bitcoin indexer by Saturn, designed for real-time blockchain access, optimized queries, and seamless Runes protocol integration. It delivers superior performance with minimal resource consumption.

## Key Features

- Optimized Performance – Fast, efficient queries tailored for dApps and validator nodes.
- Real-Time Blockchain Access – Instant data retrieval for responsive applications.
- Scalable & Lightweight – High throughput with minimal computational overhead.
- Advanced Runes Integration – Native support for Runes tokens, instant token data, and secure transaction validation.
- Mempool-Level Indexing for Runes – Differentiates pending vs confirmed runes in outputs from transactions in the mempool.

## Requirements

- Bitcoin Node >= 27.0
- Rust v1.56+ (using the 2021 edition)
- Cargo v1.85.1

## How to run it

```bash
cargo run -p titan -- --bitcoin-rpc-url http://localhost:<PORT> --bitcoin-rpc-username <USERNAME> --bitcoin-rpc-password <PASSWORD> --chain regtest --index-addresses --index-bitcoin-transactions --enable-tcp-subscriptions --data-dir ~/titan-indexer
```

## How to build it

```bash
cargo build --release
```

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
- **.env Configuration:** Copy `.env.example` to `.env`, then set a unique `BITCOIN_RPC_USERNAME` and a long, random `BITCOIN_RPC_PASSWORD`. Docker Compose rejects empty credentials.

## How to run

The Docker Compose file orchestrates two services on a private `titan-network`:

- bitcoind:
  Runs the official Bitcoin Core container in regtest mode with `-txindex` enabled. Its RPC interface requires the credentials from `.env` and is not published on the host.

- titan:
  Runs the configured Titan image, waits for Bitcoind to become healthy, and receives the same RPC credentials through environment variables.

Copy and configure the environment file before starting the services:

```bash
cp .env.example .env
# Set BITCOIN_RPC_USERNAME and BITCOIN_RPC_PASSWORD in .env.
docker compose up
```