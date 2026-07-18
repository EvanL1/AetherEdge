#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
BUILDER="$ROOT_DIR/scripts/build-installer.sh"
TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

DEFAULT_MANIFEST="$TEMP_DIR/default.json"
TRIMMED_MANIFEST="$TEMP_DIR/trimmed.json"
HOME_ASSISTANT_MANIFEST="$TEMP_DIR/home-assistant.json"

bash "$BUILDER" v0-contract amd64 --manifest-only="$DEFAULT_MANIFEST"
bash "$BUILDER" v0-contract amd64 --io-features=modbus --manifest-only="$TRIMMED_MANIFEST"
bash "$BUILDER" v0-contract amd64 \
    --io-features=home-assistant-integration-control \
    --manifest-only="$HOME_ASSISTANT_MANIFEST"

if bash "$BUILDER" v0-contract amd64 \
    --io-features=home-assistant,not-a-real-adapter \
    --manifest-only="$TEMP_DIR/unknown.json" >/dev/null 2>&1; then
    echo "installer accepted an unknown aether-io feature" >&2
    exit 1
fi

cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    verify --path "$DEFAULT_MANIFEST" --aether-version "$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)" >/dev/null
cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    verify --path "$TRIMMED_MANIFEST" --aether-version "$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)" >/dev/null
cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    verify --path "$HOME_ASSISTANT_MANIFEST" --aether-version "$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)" >/dev/null

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

for feature in \
    home-assistant \
    home-assistant-cloudlink \
    home-assistant-integration-control; do
    grep -Fq "\"aether-io/$feature\"" "$HOME_ASSISTANT_MANIFEST" \
        || {
            echo "Home Assistant installer manifest omitted normalized feature $feature" >&2
            exit 1
        }
done
grep -Fq '"aether.cloudlink.integration.v1alpha1"' "$HOME_ASSISTANT_MANIFEST"
grep -Fq '"aether.cloudlink.integration-control.v1alpha1"' "$HOME_ASSISTANT_MANIFEST"

echo "Runtime manifest installer contract passed"
