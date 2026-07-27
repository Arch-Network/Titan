#!/bin/bash

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to check if we're in a git repository
check_git_repo() {
    if ! git rev-parse --git-dir > /dev/null 2>&1; then
        echo -e "${RED}Error: Not in a git repository${NC}"
        exit 1
    fi
}

# Function to get current version
get_current_version() {
    local current=$(git describe --tags --abbrev=0 2>/dev/null)
    if [ -z "$current" ]; then
        echo "No tags found"
        return 1
    fi
    echo $current
}

# Function to validate version format
validate_version() {
    local version=$1
    if [[ ! $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9]+(\.[0-9]+)?)?$ ]]; then
        echo -e "${RED}Error: Invalid version format. Use vX.Y.Z or vX.Y.Z-alpha.N${NC}"
        return 1
    fi
    return 0
}

# Function to check if tag already exists
check_tag_exists() {
    local tag=$1
    if git tag -l | grep -q "^${tag}$"; then
        echo -e "${RED}Error: Tag $tag already exists${NC}"
        return 1
    fi
    return 0
}

# Main script
main() {
    echo -e "${GREEN}🏷️  Titan Version Bump Tool${NC}"
    echo "================================"
    
    check_git_repo
    
    local current_version=$(get_current_version)
    if [ $? -eq 0 ]; then
        echo -e "Current version: ${YELLOW}$current_version${NC}"
    else
        echo -e "${YELLOW}No existing version tags found${NC}"
    fi
    
    echo ""
    echo "Enter new version (e.g., v1.0.0, v1.0.0-alpha.1, v1.0.0-beta.1):"
    read -p "> " new_version
    
    # Validate version format
    if ! validate_version "$new_version"; then
        exit 1
    fi
    
    # Check if tag already exists
    if ! check_tag_exists "$new_version"; then
        exit 1
    fi
    
    # Get release notes
    echo ""
    echo "Enter release notes (press Enter when done):"
    read -p "> " release_notes
    
    if [ -z "$release_notes" ]; then
        release_notes="Release $new_version"
    fi
    
    # Confirm action
    echo ""
    echo -e "${YELLOW}About to create and push tag:${NC}"
    echo "Tag: $new_version"
    echo "Message: $release_notes"
    echo ""
    read -p "Continue? (y/N): " confirm
    
    if [[ ! $confirm =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi
    
    # Create and push tag
    echo ""
    echo -e "${GREEN}Creating tag...${NC}"
    if git tag -a "$new_version" -m "$release_notes"; then
        echo -e "${GREEN}✓ Tag created successfully${NC}"
        
        echo -e "${GREEN}Pushing tag...${NC}"
        if git push origin "$new_version"; then
            echo -e "${GREEN}✓ Tag pushed successfully${NC}"
            echo ""
            echo -e "${GREEN}🚀 Version $new_version has been released!${NC}"
            echo "Monitor the build at: https://github.com/Arch-Network/Titan/actions"
            echo "Image will be available at: ghcr.io/arch-network/titan:$new_version"
        else
            echo -e "${RED}✗ Failed to push tag${NC}"
            exit 1
        fi
    else
        echo -e "${RED}✗ Failed to create tag${NC}"
        exit 1
    fi
}

main "$@" 