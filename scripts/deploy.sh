#!/bin/bash

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
    echo "  version     Docker image version (e.g., v1.0.0, latest, main, sha-abc123)"
    echo ""
    echo "Options:"
    echo "  -f, --file      Docker compose file (default: docker-compose.yml)"
    echo "  -e, --env       Environment file (default: .env)"
    echo "  -d, --dev       Use development compose file"
    echo "  --dry-run       Show what would be done without executing"
    echo "  -h, --help      Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0 v1.0.0                    # Deploy version v1.0.0"
    echo "  $0 latest --dev              # Deploy latest with dev compose"
    echo "  $0 main --dry-run            # Show what would happen"
}

# Function to check if docker-compose is available
check_docker_compose() {
    if ! command -v docker-compose &> /dev/null; then
        if ! command -v docker compose &> /dev/null; then
            echo -e "${RED}Error: docker-compose or 'docker compose' not found${NC}"
            exit 1
        fi
        DOCKER_COMPOSE="docker compose"
    else
        DOCKER_COMPOSE="docker-compose"
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
    
    # Update DOCKER_TAG in .env file
    if grep -q "^DOCKER_TAG=" "$env_file"; then
        sed -i "s/^DOCKER_TAG=.*/DOCKER_TAG=$version/" "$env_file"
    else
        echo "DOCKER_TAG=$version" >> "$env_file"
    fi
    
    echo -e "${GREEN}✓ Updated $env_file with DOCKER_TAG=$version${NC}"
}

# Function to check if image exists
check_image_exists() {
    local version=$1
    local image="ghcr.io/arch-network/titan:$version"
    
    echo -e "${BLUE}Checking if image exists: $image${NC}"
    
    if docker manifest inspect "$image" &> /dev/null; then
        echo -e "${GREEN}✓ Image found${NC}"
        return 0
    else
        echo -e "${YELLOW}⚠ Image not found locally, will attempt to pull during deployment${NC}"
        return 1
    fi
}

# Function to deploy
deploy() {
    local version=$1
    local compose_file=$2
    local dry_run=$3
    
    if [ "$dry_run" = true ]; then
        echo -e "${YELLOW}DRY RUN MODE - No changes will be made${NC}"
        echo ""
    fi
    
    echo -e "${BLUE}Deployment Plan:${NC}"
    echo "Version: $version"
    echo "Compose file: $compose_file"
    echo "Image: ghcr.io/arch-network/titan:$version"
    echo ""
    
    if [ "$dry_run" = true ]; then
        echo "Would execute:"
        echo "1. $DOCKER_COMPOSE -f $compose_file down"
        echo "2. $DOCKER_COMPOSE -f $compose_file pull titan"
        echo "3. $DOCKER_COMPOSE -f $compose_file up -d"
        return 0
    fi
    
    # Stop services
    echo -e "${BLUE}Stopping services...${NC}"
    if $DOCKER_COMPOSE -f "$compose_file" down; then
        echo -e "${GREEN}✓ Services stopped${NC}"
    else
        echo -e "${RED}✗ Failed to stop services${NC}"
        exit 1
    fi
    
    # Pull new image
    echo -e "${BLUE}Pulling image...${NC}"
    if $DOCKER_COMPOSE -f "$compose_file" pull titan; then
        echo -e "${GREEN}✓ Image pulled${NC}"
    else
        echo -e "${YELLOW}⚠ Pull failed, will use cached image if available${NC}"
    fi
    
    # Start services
    echo -e "${BLUE}Starting services...${NC}"
    if $DOCKER_COMPOSE -f "$compose_file" up -d; then
        echo -e "${GREEN}✓ Services started${NC}"
        echo ""
        echo -e "${GREEN}🎉 Deployment complete!${NC}"
        echo ""
        echo "Check status with:"
        echo "  $DOCKER_COMPOSE -f $compose_file ps"
        echo "  $DOCKER_COMPOSE -f $compose_file logs -f titan"
    else
        echo -e "${RED}✗ Failed to start services${NC}"
        exit 1
    fi
}

# Function to show current status
show_status() {
    local compose_file=$1
    
    echo -e "${BLUE}Current Status:${NC}"
    if [ -f "$ENV_FILE" ] && grep -q "^DOCKER_TAG=" "$ENV_FILE"; then
        local current_version=$(grep "^DOCKER_TAG=" "$ENV_FILE" | cut -d'=' -f2)
        echo "Current version: $current_version"
    fi
    
    echo ""
    $DOCKER_COMPOSE -f "$compose_file" ps
}

# Main script
main() {
    local version=""
    local dry_run=false
    local show_help=false
    
    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            -f|--file)
                COMPOSE_FILE="$2"
                shift 2
                ;;
            -e|--env)
                ENV_FILE="$2"
                shift 2
                ;;
            -d|--dev)
                COMPOSE_FILE="docker-compose.dev.yml"
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
    
    check_docker_compose
    
    # Show current status
    if [ -f "$COMPOSE_FILE" ]; then
        show_status "$COMPOSE_FILE"
        echo ""
    fi
    
    # Update .env file
    if [ "$dry_run" != true ]; then
        update_env_file "$version" "$ENV_FILE"
    else
        echo -e "${YELLOW}Would update $ENV_FILE with DOCKER_TAG=$version${NC}"
    fi
    
    # Check if image exists (optional, deployment will pull if needed)
    check_image_exists "$version"
    
    # Deploy
    deploy "$version" "$COMPOSE_FILE" "$dry_run"
}

main "$@" 