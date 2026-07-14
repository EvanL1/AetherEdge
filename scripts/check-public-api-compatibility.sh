#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CATALOG="$SCRIPT_DIR/public-crates.txt"

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

case "${1:-}" in
    '') ;;
    --catalog)
        (($# == 2)) || fail "--catalog requires exactly one path"
        CATALOG=$2
        ;;
    -h | --help)
        echo "Usage: scripts/check-public-api-compatibility.sh [--catalog PATH]"
        exit 0
        ;;
    *) fail "unknown argument: $1" ;;
esac

command -v cargo >/dev/null 2>&1 || fail "cargo is required"
command -v jq >/dev/null 2>&1 || fail "jq is required"
[[ -s "$CATALOG" ]] || fail "public crate catalog is missing or empty: $CATALOG"

mapfile -t PUBLIC_PACKAGES < <(
    awk -F '\t' 'NF && $1 !~ /^#/ { print $1 }' "$CATALOG"
)
((${#PUBLIC_PACKAGES[@]} > 0)) || fail "public crate catalog is empty: $CATALOG"

METADATA="$(cargo metadata --format-version 1 --no-deps)"

for package in "${PUBLIC_PACKAGES[@]}"; do
    version="$(jq -r --arg package "$package" \
        '.packages[] | select(.name == $package) | .version' <<< "$METADATA")"
    [[ -n "$version" && "$version" != null ]] \
        || fail "catalog package is missing from Cargo metadata: $package"

    if cargo info --registry crates-io "$package" >/dev/null 2>&1; then
        echo "Checking $package@$version against its latest crates.io baseline..."
        cargo semver-checks --package "$package"
    else
        echo "$package@$version has no crates.io baseline; treating it as a first release"
    fi
done

echo "Public API compatibility checks passed."
