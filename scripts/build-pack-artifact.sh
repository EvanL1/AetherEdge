#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ $# -ne 3 ]]; then
    echo "Usage: $0 <pack-root> <runtime-manifest.json> <output.bundle>" >&2
    exit 2
fi

PACK_ROOT=$1
RUNTIME_MANIFEST=$2
OUTPUT=$3

if [[ -n "${AETHER_CLI:-}" ]]; then
    "$AETHER_CLI" --json packs build \
        --pack-root "$PACK_ROOT" \
        --runtime-manifest "$RUNTIME_MANIFEST" \
        --output "$OUTPUT"
else
    cargo run --quiet -p aether -- --json packs build \
        --pack-root "$PACK_ROOT" \
        --runtime-manifest "$RUNTIME_MANIFEST" \
        --output "$OUTPUT"
fi
