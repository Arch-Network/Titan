# Versioning Guide for Titan Docker Images

This guide explains how to create, manage, and use versioned Docker images for the Titan Bitcoin indexer.

## 🏷️ Semantic Versioning

We follow [Semantic Versioning](https://semver.org/) (SemVer):
- **MAJOR.MINOR.PATCH** (e.g., `v1.2.3`)
- **MAJOR**: Breaking changes or incompatible API changes
- **MINOR**: New features, backward compatible
- **PATCH**: Bug fixes, backward compatible

## 🚀 Creating Releases

### 1. Create a Version Tag

```bash
# Create an annotated tag
git tag -a v1.0.0 -m "Release v1.0.0: Initial production release"

# Push the tag to trigger CI/CD
git push origin v1.0.0
```

### 2. Pre-release Versions

For beta/alpha versions:

```bash
# Alpha release
git tag -a v1.0.0-alpha.1 -m "Alpha release for testing"
git push origin v1.0.0-alpha.1

# Beta release
git tag -a v1.0.0-beta.1 -m "Beta release for testing"
git push origin v1.0.0-beta.1

# Release candidate
git tag -a v1.0.0-rc.1 -m "Release candidate"
git push origin v1.0.0-rc.1
```

## 📦 Available Image Tags

When you create a version tag `v1.2.3`, the CI/CD creates:

| Tag | Description | Example |
|-----|-------------|---------|
| `v1.2.3` | Exact version | `ghcr.io/arch-network/titan:v1.2.3` |
| `1.2` | Major.Minor | `ghcr.io/arch-network/titan:1.2` |
| `1` | Major only | `ghcr.io/arch-network/titan:1` |
| `latest` | Latest stable | `ghcr.io/arch-network/titan:latest` |
| `main` | Latest main branch | `ghcr.io/arch-network/titan:main` |
| `sha-abc123` | Specific commit | `ghcr.io/arch-network/titan:sha-abc123` |

## 🔧 Using Versions in Docker Compose

### Production (Pinned Version)
```yaml
# .env file
DOCKER_TAG=v1.0.0
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
# Update .env to previous version
DOCKER_TAG=v1.0.0

# Restart services
docker-compose down
docker-compose up -d
```

### Emergency Rollback
```bash
# Pull specific version immediately
docker pull ghcr.io/arch-network/titan:v0.9.0
docker tag ghcr.io/arch-network/titan:v0.9.0 ghcr.io/arch-network/titan:latest

# Restart with local tag
docker-compose restart titan
```

## 🎯 Best Practices

### 1. Version Naming
- Use descriptive tag messages: `git tag -a v1.0.0 -m "Production release with Runes support"`
- Include changelog in tag description
- Use consistent naming: `v1.0.0` not `1.0.0` or `version-1.0.0`

### 2. Release Process
1. **Test thoroughly** on development branch
2. **Merge to main** via pull request
3. **Create tag** from main branch
4. **Monitor CI/CD** build process
5. **Update production** environment variables
6. **Verify deployment** functionality

### 3. Environment Strategy
- **Production**: Always use specific versions (`v1.0.0`)
- **Staging**: Use `main` branch for latest features
- **Development**: Use `docker-compose.dev.yml` for local builds

### 4. Monitoring
- Monitor GitHub Actions for build status
- Check image availability before deploying
- Keep rollback versions readily available

## 🛠️ Automation Scripts

### Version Bump Script
```bash
#!/bin/bash
# version-bump.sh
CURRENT=$(git describe --tags --abbrev=0)
echo "Current version: $CURRENT"
echo "Enter new version (e.g., v1.0.1):"
read NEW_VERSION

git tag -a $NEW_VERSION -m "Release $NEW_VERSION"
git push origin $NEW_VERSION
echo "Created and pushed tag: $NEW_VERSION"
```

### Deploy Script
```bash
#!/bin/bash
# deploy.sh
VERSION=$1
if [ -z "$VERSION" ]; then
    echo "Usage: ./deploy.sh v1.0.0"
    exit 1
fi

# Update .env file
sed -i "s/DOCKER_TAG=.*/DOCKER_TAG=$VERSION/" .env

# Deploy
docker-compose down
docker-compose pull
docker-compose up -d

echo "Deployed version: $VERSION"
```

## 📊 Version History

Keep track of your releases:

| Version | Date | Description |
|---------|------|-------------|
| v1.0.0 | 2024-01-15 | Initial production release |
| v1.0.1 | 2024-01-16 | Bug fixes for RPC connection |
| v1.1.0 | 2024-01-20 | Added mempool-level indexing 