#!/bin/bash
set -uo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default values
COMPOSE_FILE="docker-compose.yml"
ENV_FILE=".env"

# Function to show usage
usage() {
    echo -e "${GREEN}🚀 Titan Deployment Tool${NC}"
    echo "Usage: $0 <version> [options]"
    echo ""
    echo "Arguments:"
    echo "  version     Docker image tag (e.g., 1.0.0, latest, main, sha-abc123)."
    echo "              A release tag like v1.0.0 is mapped to the image tag 1.0.0."
    echo ""
    echo "Options:"
    echo "  -f, --file      Docker compose file (default: docker-compose.yml)"
    echo "  -e, --env       Environment file (default: .env)"
    echo "  -d, --dev       Use development compose file (builds the image locally)"
    echo "  --dry-run       Show what would be done without executing"
    echo "  -h, --help      Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0 v1.0.0                    # Deploy image tag 1.0.0"
    echo "  $0 latest --dev              # Deploy latest with dev compose"
    echo "  $0 main --dry-run            # Show what would happen"
}

# Function to check if docker-compose is available
check_docker_compose() {
    if docker compose version &> /dev/null; then
        DOCKER_COMPOSE=(docker compose)
    elif command -v docker-compose &> /dev/null; then
        DOCKER_COMPOSE=(docker-compose)
    else
        echo -e "${RED}Error: 'docker compose' or docker-compose not found${NC}"
        exit 1
    fi
}

# Run compose with the selected compose and env files
compose() {
    "${DOCKER_COMPOSE[@]}" --env-file "$ENV_FILE" -f "$COMPOSE_FILE" "$@"
}

# Map a release tag (vX.Y.Z) to the image tag published by docker-publish.yml (X.Y.Z)
normalize_version() {
    local version=$1
    if [[ $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        version="${version#v}"
    fi
    if [[ ! $version =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
        echo -e "${RED}Error: invalid image tag '$version'${NC}" >&2
        exit 1
    fi
    echo "$version"
}

# Read a variable from the env file (last assignment wins)
env_file_value() {
    local name=$1
    grep -E "^${name}=" "$ENV_FILE" 2>/dev/null | tail -n 1 | cut -d'=' -f2-
}

# Refuse to deploy without an RPC password (compose would fail anyway)
check_required_env() {
    local password
    password="${BITCOIN_RPC_PASSWORD:-$(env_file_value BITCOIN_RPC_PASSWORD)}"
    if [ -z "$password" ]; then
        echo -e "${RED}Error: BITCOIN_RPC_PASSWORD is not set in $ENV_FILE (generate one with: openssl rand -hex 32)${NC}"
        exit 1
    fi
}

# Function to update .env file
update_env_file() {
    local version=$1
    local env_file=$2
    
    if [ ! -f "$env_file" ]; then
        echo -e "${YELLOW}Warning: $env_file not found, creating from example${NC}"
        if [ -f ".env.example" ]; then
            cp .env.example "$env_file"
        else
            echo -e "${RED}Error: .env.example not found${NC}"
            exit 1
        fi
    fi
    
    # Update DOCKER_TAG in .env file (version is validated by normalize_version)
    if grep -q "^DOCKER_TAG=" "$env_file"; then
        sed -i "s/^DOCKER_TAG=.*/DOCKER_TAG=$version/" "$env_file"
    else
        echo "DOCKER_TAG=$version" >> "$env_file"
    fi
    
    echo -e "${GREEN}✓ Updated $env_file with DOCKER_TAG=$version${NC}"
}

# Function to report whether the image exists in the registry
check_image_exists() {
    local version=$1
    local image
    image="$(image_name "$version")"

    echo -e "${BLUE}Checking if image exists: $image${NC}"

    if docker manifest inspect "$image" &> /dev/null; then
        echo -e "${GREEN}✓ Image found${NC}"
    else
        echo -e "${YELLOW}⚠ Image not found in the registry (or not accessible); the pull may fail${NC}"
    fi
}

# Full image reference for a tag, honouring GITHUB_REPOSITORY
image_name() {
    local version=$1
    local repository="${GITHUB_REPOSITORY:-$(env_file_value GITHUB_REPOSITORY)}"
    repository="${repository:-arch-network/titan}"
    echo "ghcr.io/${repository,,}:$version"
}

# Function to deploy
deploy() {
    local version=$1
    local dry_run=$2
    local is_dev=$3

    if [ "$dry_run" = true ]; then
        echo -e "${YELLOW}DRY RUN MODE - No changes will be made${NC}"
        echo ""
    fi

    echo -e "${BLUE}Deployment Plan:${NC}"
    echo "Version: $version"
    echo "Compose file: $COMPOSE_FILE"
    echo "Env file: $ENV_FILE"
    if [ "$is_dev" = true ]; then
        echo "Image: built locally from the Dockerfile"
    else
        echo "Image: $(image_name "$version")"
    fi
    echo ""

    local compose_cmd="${DOCKER_COMPOSE[*]} --env-file $ENV_FILE -f $COMPOSE_FILE"
    if [ "$dry_run" = true ]; then
        echo "Would execute:"
        if [ "$is_dev" = true ]; then
            echo "1. $compose_cmd up -d --build"
        else
            echo "1. $compose_cmd pull titan"
            echo "2. $compose_cmd up -d"
        fi
        return 0
    fi

    if [ "$is_dev" = true ]; then
        echo -e "${BLUE}Building and starting services...${NC}"
        compose up -d --build || { echo -e "${RED}✗ Failed to start services${NC}"; exit 1; }
    else
        # Pull first so the running services keep serving while the image downloads.
        echo -e "${BLUE}Pulling image...${NC}"
        compose pull titan || { echo -e "${RED}✗ Failed to pull $(image_name "$version")${NC}"; exit 1; }
        echo -e "${GREEN}✓ Image pulled${NC}"

        echo -e "${BLUE}Starting services...${NC}"
        compose up -d || { echo -e "${RED}✗ Failed to start services${NC}"; exit 1; }
    fi

    echo -e "${GREEN}✓ Services started${NC}"
    echo ""
    echo -e "${GREEN}🎉 Deployment complete!${NC}"
    echo ""
    echo "Check status with:"
    echo "  $compose_cmd ps"
    echo "  $compose_cmd logs -f titan"
}

# Function to show current status
show_status() {
    echo -e "${BLUE}Current Status:${NC}"
    local current_version
    current_version="$(env_file_value DOCKER_TAG)"
    if [ -n "$current_version" ]; then
        echo "Current version: $current_version"
    fi

    echo ""
    compose ps || true
}

# Main script
main() {
    local version=""
    local dry_run=false
    local show_help=false
    local is_dev=false

    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            -f|--file)
                [ $# -ge 2 ] || { echo -e "${RED}$1 requires a value${NC}"; exit 1; }
                COMPOSE_FILE="$2"
                shift 2
                ;;
            -e|--env)
                [ $# -ge 2 ] || { echo -e "${RED}$1 requires a value${NC}"; exit 1; }
                ENV_FILE="$2"
                shift 2
                ;;
            -d|--dev)
                COMPOSE_FILE="docker-compose.dev.yml"
                is_dev=true
                shift
                ;;
            --dry-run)
                dry_run=true
                shift
                ;;
            -h|--help)
                show_help=true
                shift
                ;;
            -*)
                echo -e "${RED}Unknown option $1${NC}"
                usage
                exit 1
                ;;
            *)
                if [ -z "$version" ]; then
                    version="$1"
                else
                    echo -e "${RED}Too many arguments${NC}"
                    usage
                    exit 1
                fi
                shift
                ;;
        esac
    done

    if [ "$show_help" = true ]; then
        usage
        exit 0
    fi

    if [ -z "$version" ]; then
        echo -e "${RED}Error: Version argument is required${NC}"
        echo ""
        usage
        exit 1
    fi

    version="$(normalize_version "$version")" || exit 1

    check_docker_compose

    # Update .env file
    if [ "$dry_run" != true ]; then
        update_env_file "$version" "$ENV_FILE"
    else
        echo -e "${YELLOW}Would update $ENV_FILE with DOCKER_TAG=$version${NC}"
    fi

    check_required_env

    # Show current status
    if [ -f "$COMPOSE_FILE" ]; then
        show_status
        echo ""
    fi

    if [ "$is_dev" != true ]; then
        check_image_exists "$version"
    fi

    # Deploy
    deploy "$version" "$dry_run" "$is_dev"
}

main "$@" 