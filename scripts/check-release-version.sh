#!/usr/bin/env bash
# Verify that a release tag matches the workspace version and changelog.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TAG_NAME="${1:-${GITHUB_REF_NAME:-}}"
if [[ -z "$TAG_NAME" ]]; then
    echo "error: release tag is required (argument 1 or GITHUB_REF_NAME)" >&2
    exit 1
fi

if [[ ! "$TAG_NAME" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]]; then
    echo "error: invalid release tag '$TAG_NAME'; expected vX.Y.Z or vX.Y.Z-prerelease" >&2
    exit 1
fi

WORKSPACE_VERSION="$({
    awk '
        /^\[workspace\.package\][[:space:]]*$/ { in_workspace_package = 1; next }
        in_workspace_package && /^\[/ { exit }
        in_workspace_package && /^[[:space:]]*version[[:space:]]*=/ {
            line = $0
            sub(/^[^\"]*\"/, "", line)
            sub(/\".*/, "", line)
            print line
            exit
        }
    ' "$ROOT_DIR/Cargo.toml"
} || true)"

if [[ -z "$WORKSPACE_VERSION" ]]; then
    echo "error: workspace.package.version not found in Cargo.toml" >&2
    exit 1
fi

TAG_VERSION="${TAG_NAME#v}"
if [[ "$TAG_VERSION" != "$WORKSPACE_VERSION" ]]; then
    echo "error: tag version '$TAG_VERSION' does not match workspace version '$WORKSPACE_VERSION'" >&2
    exit 1
fi

if ! awk -v expected="## [$WORKSPACE_VERSION]" '
    $0 == expected || index($0, expected " ") == 1 { found = 1 }
    END { exit(found ? 0 : 1) }
' "$ROOT_DIR/CHANGELOG.md"; then
    echo "error: CHANGELOG.md has no release heading for [$WORKSPACE_VERSION]" >&2
    exit 1
fi

TOOLCHAIN_VERSION="$({
    awk '
        /^\[toolchain\][[:space:]]*$/ { in_toolchain = 1; next }
        in_toolchain && /^\[/ { exit }
        in_toolchain && /^[[:space:]]*channel[[:space:]]*=/ {
            line = $0
            sub(/^[^\"]*\"/, "", line)
            sub(/\".*/, "", line)
            print line
            exit
        }
    ' "$ROOT_DIR/rust-toolchain.toml"
} || true)"

if [[ "$TOOLCHAIN_VERSION" != "1.90.0" ]]; then
    echo "error: rust-toolchain.toml must pin channel 1.90.0 (found '${TOOLCHAIN_VERSION:-missing}')" >&2
    exit 1
fi

echo "release metadata verified: $TAG_NAME (Rust $TOOLCHAIN_VERSION)"
