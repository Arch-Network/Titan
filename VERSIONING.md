# Versioning Guide for Titan Docker Images

This guide explains how to create, manage, and use versioned Docker images for the Titan Bitcoin indexer.

## 🏷️ Semantic Versioning

We follow [Semantic Versioning](https://semver.org/) (SemVer):
- **MAJOR.MINOR.PATCH** (e.g., `v1.2.3`)
- **MAJOR**: Breaking changes or incompatible API changes
- **MINOR**: New features, backward compatible
- **PATCH**: Bug fixes, backward compatible

## 🚀 Creating Releases

Releases are stable `vMAJOR.MINOR.PATCH` tags. The tag must match the workspace version in
`Cargo.toml` (`[workspace.package] version` and the `arch-titan-*` entries in
`[workspace.dependencies]`): `publish-crates.yml` rejects any other tag format and any
version mismatch. Pre-release tags (`v1.0.0-rc.1`, ...) are not supported: they build no
Docker image and fail crate publishing.

### Using the release script (recommended)

```bash
./scripts/version-bump.sh
```

From a clean working tree, the script:

1. asks for the new version (`vX.Y.Z`) and refuses existing tags,
2. writes it into `Cargo.toml`, refreshes `Cargo.lock` (`cargo update --workspace`) and
   commits both as `Release vX.Y.Z`,
3. creates the annotated tag and pushes the branch and the tag to `origin`.

### Manually

```bash
# 1. Set version = "1.0.0" in [workspace.package] and on the arch-titan-* workspace dependencies
cargo update --workspace
git commit -am "Release v1.0.0"

# 2. Tag and push
git tag -a v1.0.0 -m "Release v1.0.0: Initial production release"
git push origin HEAD v1.0.0
```

## 📦 Available Image Tags

When you push the tag `v1.2.3`, the Docker workflow publishes (image tags have no `v` prefix):

| Tag | Description | Example |
|-----|-------------|---------|
| `1.2.3` | Exact version | `ghcr.io/arch-network/titan:1.2.3` |
| `1.2` | Major.Minor | `ghcr.io/arch-network/titan:1.2` |
| `1` | Major only | `ghcr.io/arch-network/titan:1` |
| `latest` | Latest main/master build | `ghcr.io/arch-network/titan:latest` |
| `main` | Latest main branch | `ghcr.io/arch-network/titan:main` |
| `sha-abc123` | Specific commit | `ghcr.io/arch-network/titan:sha-abc123` |

## 🔧 Using Versions in Docker Compose

### Production (Pinned Version)
```yaml
# .env file
DOCKER_TAG=1.0.0
```

### Staging (Latest Features)
```yaml
# .env file
DOCKER_TAG=main
```

### Development (Specific Branch)
```yaml
# .env file
DOCKER_TAG=feature-branch-name
```

## 📋 Version Management Commands

### List Available Tags
```bash
# List local tags
git tag -l

# List remote tags
git ls-remote --tags origin

# List Docker image tags (requires GitHub CLI)
gh api repos/Arch-Network/Titan/packages/container/titan/versions
```

### Delete Tags
```bash
# Delete local tag
git tag -d v1.0.0

# Delete remote tag
git push origin --delete v1.0.0
```

### Check Current Version
```bash
# Show current commit info
git describe --tags --always

# Show latest tag
git describe --tags --abbrev=0
```

## 🔄 Rollback Strategy

### Quick Rollback
```bash
# Deploy the previous version (updates DOCKER_TAG in .env, pulls, recreates titan)
./scripts/deploy.sh v1.0.0
```

Rolling back across an index schema change (e.g. from schema v2 to a release that uses
schema v1) requires re-indexing: remove the `titan-data` volume first.

### Emergency Rollback
```bash
# Pull a specific version and recreate titan with it
DOCKER_TAG=0.9.0 docker compose pull titan
DOCKER_TAG=0.9.0 docker compose up -d titan
```

## 🎯 Best Practices

### 1. Version Naming
- Use descriptive tag messages: `git tag -a v1.0.0 -m "Production release with Runes support"`
- Include changelog in tag description
- Use consistent git tag naming: `v1.0.0` not `1.0.0` or `version-1.0.0` (image tags drop the `v`)

### 2. Release Process
1. **Test thoroughly** on development branch
2. **Merge to main** via pull request
3. **Create tag** from main branch
4. **Monitor CI/CD** build process
5. **Update production** environment variables
6. **Verify deployment** functionality

### 3. Environment Strategy
- **Production**: Always use specific versions (`1.0.0`)
- **Staging**: Use `main` branch for latest features
- **Development**: Use `docker-compose.dev.yml` for local builds

### 4. Monitoring
- Monitor GitHub Actions for build status
- Check image availability before deploying
- Keep rollback versions readily available

## 🛠️ Automation Scripts

### `scripts/version-bump.sh`
Bumps the workspace version, commits it, then creates and pushes the release tag (see
[Creating Releases](#-creating-releases)).

### `scripts/deploy.sh`
```bash
./scripts/deploy.sh v1.0.0            # deploy image 1.0.0 with docker-compose.yml
./scripts/deploy.sh latest --dev      # build and run docker-compose.dev.yml
./scripts/deploy.sh v1.0.0 --dry-run  # show what would happen
./scripts/deploy.sh v1.0.0 -e prod.env -f docker-compose.yml
```

The script maps a `vX.Y.Z` release tag to the `X.Y.Z` image tag, writes `DOCKER_TAG` to the
env file (created from `.env.example` if missing), refuses to continue while
`BITCOIN_RPC_PASSWORD` is unset, pulls the image and recreates the services with
`docker compose up -d`.

## 📊 Version History

Keep track of your releases:

| Version | Date | Description |
|---------|------|-------------|
| v1.0.0 | 2024-01-15 | Initial production release |
| v1.0.1 | 2024-01-16 | Bug fixes for RPC connection |
| v1.1.0 | 2024-01-20 | Added mempool-level indexing 