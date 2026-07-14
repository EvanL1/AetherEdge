#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CATALOG="$SCRIPT_DIR/public-crates.txt"
EXECUTE=false

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

case "${1:-}" in
    '') ;;
    --execute) EXECUTE=true ;;
    -h | --help)
        echo "Usage: scripts/publish-public-crates.sh [--execute]"
        exit 0
        ;;
    *) fail "unknown argument: $1" ;;
esac
(($# <= 1)) || fail "unexpected extra arguments"

mapfile -t PUBLIC_PACKAGES < <(
    awk -F '\t' 'NF && $1 !~ /^#/ { print $1 }' "$CATALOG"
)
((${#PUBLIC_PACKAGES[@]} > 0)) || fail "public crate catalog is empty: $CATALOG"

WORKSPACE_VERSION="$(cargo metadata --format-version 1 --no-deps \
    | jq -r '.packages[] | select(.name == "aether-edge-sdk") | .version')"
[[ -n "$WORKSPACE_VERSION" && "$WORKSPACE_VERSION" != null ]] \
    || fail "could not resolve the public crate release version"

if [[ "$EXECUTE" != true ]]; then
    echo "publication plan only; pass --execute in the protected tag workflow"
    for package in "${PUBLIC_PACKAGES[@]}"; do
        echo "$package@$WORKSPACE_VERSION"
    done
    exit 0
fi

[[ "${GITHUB_ACTIONS:-}" == true ]] \
    || fail "publishing is allowed only in GitHub Actions"
[[ "${GITHUB_REF_TYPE:-}" == tag ]] \
    || fail "publishing is allowed only from a GitHub tag workflow"
[[ -n "${GITHUB_REF_NAME:-}" ]] || fail "GITHUB_REF_NAME is required"
[[ -n "${CARGO_REGISTRY_TOKEN:-}" ]] || fail "CARGO_REGISTRY_TOKEN is required"
[[ -z "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=no)" ]] \
    || fail "refusing to publish from a dirty tracked worktree"

"$SCRIPT_DIR/check-release-version.sh" "$GITHUB_REF_NAME"
"$SCRIPT_DIR/check-public-crate-release.sh"

registry_has_version() {
    local package=$1
    cargo info --registry crates-io "$package@$WORKSPACE_VERSION" >/dev/null 2>&1
}

wait_until_visible() {
    local package=$1
    local _
    for _ in {1..18}; do
        if registry_has_version "$package"; then
            return 0
        fi
        sleep 10
    done
    return 1
}

for package in "${PUBLIC_PACKAGES[@]}"; do
    if registry_has_version "$package"; then
        archive="$ROOT_DIR/target/package/${package}-${WORKSPACE_VERSION}.crate"
        "$SCRIPT_DIR/verify-published-crate-archive.sh" \
            "$package" "$WORKSPACE_VERSION" "$archive"
        echo "$package@$WORKSPACE_VERSION already exists on crates.io; verified safe to resume"
        continue
    fi

    echo "Publishing $package@$WORKSPACE_VERSION..."
    cargo publish --locked --package "$package"
    wait_until_visible "$package" \
        || fail "$package@$WORKSPACE_VERSION was not visible on crates.io after publication"
done

echo "Public crate publication completed for $WORKSPACE_VERSION."
