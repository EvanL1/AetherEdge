#!/usr/bin/env bash

set -euo pipefail

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

(($# == 3)) || fail "usage: verify-published-crate-archive.sh <package> <version> <local-archive>"

PACKAGE=$1
VERSION=$2
LOCAL_ARCHIVE=$3

[[ "$PACKAGE" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || fail "invalid crate package name: $PACKAGE"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] \
    || fail "invalid crate version: $VERSION"
[[ -s "$LOCAL_ARCHIVE" ]] || fail "local release archive is missing or empty: $LOCAL_ARCHIVE"
command -v curl >/dev/null 2>&1 || fail "curl is required to verify a published crate"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fail "sha256sum or shasum is required to verify a published crate"
    fi
}

REMOTE_ARCHIVE="$(mktemp)"
trap 'rm -f "$REMOTE_ARCHIVE"' EXIT

curl -fsSL --retry 3 \
    -o "$REMOTE_ARCHIVE" \
    "https://crates.io/api/v1/crates/$PACKAGE/$VERSION/download"

LOCAL_DIGEST="$(sha256_file "$LOCAL_ARCHIVE")"
REMOTE_DIGEST="$(sha256_file "$REMOTE_ARCHIVE")"
[[ "$LOCAL_DIGEST" == "$REMOTE_DIGEST" ]] \
    || fail "$PACKAGE@$VERSION published archive digest differs from the local release archive"

echo "$PACKAGE@$VERSION published archive matches the local release archive ($LOCAL_DIGEST)"
