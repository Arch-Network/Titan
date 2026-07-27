# Docker & GitHub Container Registry Setup

This project is configured to automatically build and publish Docker images to GitHub Container Registry (ghcr.io) using GitHub Actions.

## Quick Start

### Using Pre-built Images (Recommended)

1. Copy the environment example file:
   ```bash
   cp .env.example .env
   ```

2. Edit `.env` and update the `GITHUB_REPOSITORY` value (defaults to arch-network/titan):
   ```bash
   GITHUB_REPOSITORY=arch-network/titan
   ```

3. Run with Docker Compose:
   ```bash
   docker-compose up -d
   ```

### Local Development

For local development with live rebuilding:

```bash
docker-compose -f docker-compose.dev.yml up -d
```

## GitHub Actions Setup

The repository includes a GitHub Actions workflow (`.github/workflows/docker-publish.yml`) that:

- Builds multi-architecture Docker images (linux/amd64, linux/arm64)
- Publishes to GitHub Container Registry on pushes to main/master
- Tags images with branch names, PR numbers, version tags, and commit SHAs
- Uses GitHub Actions cache for faster builds

### Required Setup Steps

1. **Enable Package Publishing**: Ensure your GitHub repository has package publishing enabled
2. **Repository Secrets**: No additional secrets needed - uses `GITHUB_TOKEN` automatically
3. **Package Visibility**: After first build, you may need to make the package public in your GitHub repository settings

### Image Tags

The workflow creates multiple tags:
- `latest` - Latest main/master branch
- `main` or `master` - Branch-specific tags  
- `v1.2.3` - Version tags (if you use semantic versioning)
- `sha-abcdef` - Commit-specific tags

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_REPOSITORY` | `arch-network/titan` | Your GitHub repository name |
| `DOCKER_TAG` | `latest` | Docker image tag to use |
| `BITCOIN_NETWORK` | `regtest` | Bitcoin network (regtest/testnet/mainnet) |
| `BITCOIN_RPC_PORT` | `18443` | Bitcoin RPC port |
| `BITCOIN_RPC_USERNAME` | `bitcoin` | Bitcoin RPC username |
| `BITCOIN_RPC_PASSWORD` | `bitcoinpass` | Bitcoin RPC password |
| `TITAN_HTTP_PORT` | `3030` | Titan HTTP API port |
| `TITAN_TCP_PORT` | `8080` | Titan TCP subscription port |
| `COMMIT_INTERVAL` | `5` | Titan commit interval |

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

- **Permission denied**: Ensure the package is public or you're authenticated
- **Image not found**: Check the repository name in your `.env` file
- **Build failures**: Check the GitHub Actions logs for detailed error messages 