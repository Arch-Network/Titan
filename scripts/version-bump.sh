#!/bin/bash
# Bump the workspace version, commit it, then create and push a matching release tag.
# Tags must be stable vMAJOR.MINOR.PATCH: publish-crates.yml rejects anything else and
# requires the published crates' versions to equal the tag.
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

die() {
    echo -e "${RED}Error: $*${NC}" >&2
    exit 1
}

# Function to check if we're in a git repository and move to its root
check_git_repo() {
    local root
    root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "Not in a git repository"
    cd "$root"
}

# Function to check that the working tree has no uncommitted changes
check_clean_tree() {
    if [ -n "$(git status --porcelain)" ]; then
        die "Working tree has uncommitted changes; commit or stash them first"
    fi
}

# Function to get the current workspace version from Cargo.toml
get_workspace_version() {
    awk '
        /^\[/ { in_section = ($0 == "[workspace.package]") }
        in_section && /^version[[:space:]]*=/ {
            gsub(/^version[[:space:]]*=[[:space:]]*"|".*$/, "")
            print
            exit
        }
    ' Cargo.toml
}

# Function to validate version format (must match publish-crates.yml)
validate_version() {
    local version=$1
    if [[ ! $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo -e "${RED}Error: Invalid version format. Use vMAJOR.MINOR.PATCH (e.g. v1.2.3); pre-release tags are not published${NC}"
        return 1
    fi
    return 0
}

# Function to check if tag already exists locally or on origin
check_tag_exists() {
    local tag=$1
    if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
        echo -e "${RED}Error: Tag $tag already exists${NC}"
        return 1
    fi
    if git ls-remote --exit-code --tags origin "refs/tags/${tag}" >/dev/null 2>&1; then
        echo -e "${RED}Error: Tag $tag already exists on origin${NC}"
        return 1
    fi
    return 0
}

# Function to write the new version into the workspace manifest and lockfile
bump_workspace_version() {
    local version=$1
    local tmp
    tmp="$(mktemp)"

    awk -v version="$version" '
        /^\[/ { section = $0 }
        section == "[workspace.package]" && /^version[[:space:]]*=/ {
            $0 = "version = \"" version "\""
        }
        section == "[workspace.dependencies]" && /^arch-titan-[a-z-]+[[:space:]]*=/ {
            sub(/version[[:space:]]*=[[:space:]]*"[^"]*"/, "version = \"" version "\"")
        }
        { print }
    ' Cargo.toml >"$tmp"
    mv "$tmp" Cargo.toml

    [ "$(get_workspace_version)" = "$version" ] || die "Failed to update [workspace.package] version in Cargo.toml"

    # Refresh the workspace members' entries in Cargo.lock without touching other dependencies.
    cargo update --workspace

    # Verify every published crate now carries the new version (same check as publish-crates.yml).
    local metadata package package_version
    metadata="$(cargo metadata --no-deps --format-version 1)"
    for package in arch-titan-types-core arch-titan-types-api arch-titan-client; do
        package_version="$(jq -er --arg package "$package" '.packages[] | select(.name == $package) | .version' <<<"$metadata")"
        [ "$package_version" = "$version" ] || die "$package version is $package_version, expected $version"
    done
}

# Main script
main() {
    echo -e "${GREEN}🏷️  Titan Version Bump Tool${NC}"
    echo "================================"

    command -v cargo >/dev/null 2>&1 || die "cargo not found"
    command -v jq >/dev/null 2>&1 || die "jq not found"

    check_git_repo
    check_clean_tree

    local branch
    branch="$(git rev-parse --abbrev-ref HEAD)"
    if [ "$branch" != "main" ] && [ "$branch" != "master" ]; then
        echo -e "${YELLOW}Warning: releasing from branch '$branch' (not main/master)${NC}"
    fi

    local current_version
    current_version="$(get_workspace_version)"
    [ -n "$current_version" ] || die "Could not read [workspace.package] version from Cargo.toml"
    echo -e "Current workspace version: ${YELLOW}v$current_version${NC}"
    if latest_tag="$(git describe --tags --abbrev=0 2>/dev/null)"; then
        echo -e "Latest tag: ${YELLOW}$latest_tag${NC}"
    else
        echo -e "${YELLOW}No existing version tags found${NC}"
    fi

    echo ""
    echo "Enter new version (e.g., v1.0.0):"
    read -r -p "> " new_version

    validate_version "$new_version" || exit 1
    check_tag_exists "$new_version" || exit 1

    local version="${new_version#v}"

    # Get release notes
    echo ""
    echo "Enter release notes (press Enter when done):"
    read -r -p "> " release_notes

    if [ -z "$release_notes" ]; then
        release_notes="Release $new_version"
    fi

    # Confirm action
    echo ""
    echo -e "${YELLOW}About to:${NC}"
    if [ "$version" != "$current_version" ]; then
        echo "1. Set the workspace version to $version and commit Cargo.toml/Cargo.lock on '$branch'"
    else
        echo "1. Keep the workspace version $version (already set)"
    fi
    echo "2. Create tag $new_version with message: $release_notes"
    echo "3. Push '$branch' and $new_version to origin"
    echo ""
    read -r -p "Continue? (y/N): " confirm

    if [[ ! $confirm =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi

    if [ "$version" != "$current_version" ]; then
        echo ""
        echo -e "${GREEN}Bumping workspace version...${NC}"
        bump_workspace_version "$version"
        git add Cargo.toml Cargo.lock
        git commit -m "Release $new_version"
        echo -e "${GREEN}✓ Version bump committed${NC}"
    fi

    echo ""
    echo -e "${GREEN}Creating tag...${NC}"
    git tag -a "$new_version" -m "$release_notes"
    echo -e "${GREEN}✓ Tag created successfully${NC}"

    echo -e "${GREEN}Pushing...${NC}"
    git push origin "$branch"
    git push origin "$new_version"
    echo -e "${GREEN}✓ Branch and tag pushed successfully${NC}"
    echo ""
    echo -e "${GREEN}🚀 Version $new_version has been released!${NC}"
    echo "Monitor the build at: https://github.com/Arch-Network/Titan/actions"
    echo "Image will be available at: ghcr.io/arch-network/titan:${version}"
}

main "$@"
