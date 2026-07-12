#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
BUILDER="$ROOT_DIR/scripts/build-installer.sh"
TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

DEFAULT_MANIFEST="$TEMP_DIR/default.json"
TRIMMED_MANIFEST="$TEMP_DIR/trimmed.json"

bash "$BUILDER" v0-contract amd64 --manifest-only="$DEFAULT_MANIFEST"
bash "$BUILDER" v0-contract amd64 --io-features=modbus --manifest-only="$TRIMMED_MANIFEST"

cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    verify --path "$DEFAULT_MANIFEST" --aether-version "$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)" >/dev/null
cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    verify --path "$TRIMMED_MANIFEST" --aether-version "$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)" >/dev/null

grep -Fq '"modbus_tcp"' "$DEFAULT_MANIFEST"
grep -Fq '"target_triple": "x86_64-unknown-linux-musl"' "$DEFAULT_MANIFEST"
if grep -Eq '"(mqtt|http)"' "$DEFAULT_MANIFEST"; then
    echo "default installer manifest over-advertises disabled MQTT/HTTP adapters" >&2
    exit 1
fi
grep -Fq '"modbus_tcp"' "$TRIMMED_MANIFEST"
grep -Fq '"aether-io/modbus"' "$TRIMMED_MANIFEST"
if grep -Eq '"(aether_485|can|di_do|iec61850|mqtt|http)"' "$TRIMMED_MANIFEST"; then
    echo "trimmed installer manifest advertises an unselected adapter" >&2
    exit 1
fi

echo "Runtime manifest installer contract passed"
