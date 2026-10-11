# Docker & GitHub Container Registry Setup

This project is configured to automatically build and publish Docker images to GitHub Container Registry (ghcr.io) using GitHub Actions. The compose files run Titan together with a Bitcoin Core node.

## Quick Start

### Using Pre-built Images (Recommended)

1. Copy the environment example file:
   ```bash
   cp .env.example .env
   ```

2. Set an RPC password (required; compose refuses to start without one) and review the other values:
   ```bash
   BITCOIN_RPC_PASSWORD=$(openssl rand -hex 32)
   GITHUB_REPOSITORY=arch-network/titan
   ```

3. Run with Docker Compose:
   ```bash
   docker compose up -d
   ```

### Local Development

For local development, build the image from the working tree:

```bash
docker compose -f docker-compose.dev.yml up -d --build
```

## Services

- **bitcoind** – `bitcoin/bitcoin:29.4`, started with `-chain=$BITCOIN_NETWORK`, `-rpcport=$BITCOIN_RPC_PORT` and `-txindex`. Data lives in the `btcd-data` volume. RPC accepts connections only from the compose network (`172.28.0.0/24`). The production compose file does not publish the RPC port; the dev compose file publishes it on `127.0.0.1` only.
- **titan** – the indexer. It waits for bitcoind to become healthy and is configured entirely through environment variables. The HTTP API (`TITAN_HTTP_PORT`) and TCP subscription server (`TITAN_TCP_PORT`) are published on all interfaces. Index data lives in the `titan-data` volume.

## GitHub Actions Setup

The repository includes these workflows:

- `.github/workflows/ci.yml` – on pull requests and pushes to main/master: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` (including doctests) and the TypeScript SDK/test-script lint and build.
- `.github/workflows/docker-publish.yml` – builds linux/amd64 and linux/arm64 images, pushes them to GitHub Container Registry on pushes to main/master and on `v*` tags, and builds without pushing for pull requests.
- `.github/workflows/publish-crates.yml` – publishes the Rust crates on `vX.Y.Z` tags (see [VERSIONING.md](./VERSIONING.md)).

### Required Setup Steps

1. **Enable Package Publishing**: Ensure your GitHub repository has package publishing enabled
2. **Repository Secrets**: The image workflow uses `GITHUB_TOKEN`; crate publishing needs the `CRATES_IO_TOKEN` secret
3. **Package Visibility**: After first build, you may need to make the package public in your GitHub repository settings

### Image Tags

The workflow creates multiple tags:
- `latest` - Latest main/master branch
- `main` or `master` - Branch-specific tags
- `1.2.3`, `1.2`, `1` - From a `v1.2.3` git tag (the image tags have no `v` prefix)
- `sha-abcdef` - Commit-specific tags

## Environment Variables

Variables read by docker compose (from `.env`):

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_REPOSITORY` | `arch-network/titan` | Your GitHub repository name |
| `DOCKER_TAG` | `latest` | Docker image tag to use |
| `BITCOIN_NETWORK` | `regtest` | Network in bitcoind's naming: `main`, `test`, `testnet4`, `signet` or `regtest`. Passed to bitcoind as `-chain` and to Titan as `CHAIN` (Titan accepts these names). |
| `BITCOIN_RPC_PORT` | `18443` | bitcoind RPC port. bitcoind is started with `-rpcport` set to this value, so it applies to every network. |
| `BITCOIN_RPC_USERNAME` | `bitcoin` | Bitcoin RPC username |
| `BITCOIN_RPC_PASSWORD` | – (required) | Bitcoin RPC password |
| `TITAN_HTTP_PORT` | `3030` | Titan HTTP API port |
| `TITAN_TCP_PORT` | `8080` | Titan TCP subscription port |
| `COMMIT_INTERVAL` | `5` | Titan commit interval (blocks) |
| `INDEX_ADDRESSES` | `true` | Index outputs by address |
| `INDEX_BITCOIN_TRANSACTIONS` | `true` | Index bitcoin transactions |
| `INDEX_SPENT_OUTPUTS` | `true` | Keep spent outputs in the index |
| `ENABLE_TCP_SUBSCRIPTIONS` | `true` | Enable the TCP subscription server |
| `ENABLE_WEBHOOK_SUBSCRIPTIONS` | `false` | Enable webhook subscriptions |
| `WEBHOOK_ADMIN_TOKEN` | – | Bearer token for the webhook subscription API. Required when webhook subscriptions are enabled; passed to Titan only when set. |
| `WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` | `false` | Allow webhook receivers on private/loopback addresses (e.g. other compose services) |

### Running the image directly

The image's entrypoint passes no configuration on the command line: Titan reads every option from the environment variable of the same name (see the options table in [README.md](./README.md)). The image sets these defaults:

```
DATA_DIR=/home/titan/data          CHAIN=regtest
BITCOIN_RPC_URL=http://bitcoind:18443
BITCOIN_RPC_USERNAME=bitcoin       COMMIT_INTERVAL=5
HTTP_LISTEN=0.0.0.0:3030           TCP_ADDRESS=0.0.0.0:8080
MAIN_LOOP_INTERVAL=500             INDEX_ADDRESSES=true
INDEX_BITCOIN_TRANSACTIONS=true    INDEX_SPENT_OUTPUTS=true
ENABLE_TCP_SUBSCRIPTIONS=true
```

`BITCOIN_RPC_PASSWORD` has no default. Empty variables are ignored, so Titan falls back to its built-in default. Extra flags can still be passed as the command, e.g. `docker run ... ghcr.io/arch-network/titan:latest --index-addresses=false`.

The container starts as root only to fix ownership of the data directory and then drops to the unprivileged `titan` user. `PUID`/`PGID` remap that user (e.g. for bind mounts); `SKIP_CHOWN=1` skips the ownership fix.

### Upgrading to database schema v2

Titan refuses to open an index created before schema version 2 and reports a schema mismatch. Re-index from scratch:

```bash
docker compose down
docker volume rm <project>_titan-data   # e.g. titan_titan-data; see `docker volume ls`
docker compose up -d
```

The bitcoind volume (`btcd-data`) does not need to be removed.

## Workflow Triggers

The Docker build workflow runs on:
- Push to `main` or `master` branches
- Push of version tags (e.g., `v1.0.0`)
- Pull requests (builds only, doesn't push)

## Manual Image Building

To build and push manually:

```bash
# Build locally
docker build -t ghcr.io/arch-network/titan:latest .

# Push to registry (requires authentication)
docker push ghcr.io/arch-network/titan:latest
```

## Troubleshooting

- **`required variable BITCOIN_RPC_PASSWORD is missing a value`**: set `BITCOIN_RPC_PASSWORD` in `.env`
- **Titan exits with a schema mismatch**: see "Upgrading to database schema v2" above
- **Permission denied**: Ensure the package is public or you're authenticated
- **Image not found**: Check the repository name and `DOCKER_TAG` in your `.env` file (release images are tagged `1.2.3`, not `v1.2.3`)
- **Build failures**: Check the GitHub Actions logs for detailed error messages
