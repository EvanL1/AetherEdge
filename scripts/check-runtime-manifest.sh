#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "Checking runtime-manifest schema and generated artifact..."
python3 -m json.tool contracts/runtime/runtime-manifest.v1.schema.json >/dev/null
python3 -m json.tool config.template/runtime-manifest.json >/dev/null
if command -v uvx >/dev/null 2>&1; then
    uvx --from check-jsonschema check-jsonschema --check-metaschema \
        contracts/runtime/runtime-manifest.v1.schema.json
    uvx --from check-jsonschema check-jsonschema \
        --schemafile contracts/runtime/runtime-manifest.v1.schema.json \
        config.template/runtime-manifest.json config.e2e/runtime-manifest.json
fi

AETHER_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
if [[ -z "$AETHER_VERSION" ]]; then
    echo "ERROR: workspace Aether version is unavailable" >&2
    exit 1
fi
cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    verify --path config.template/runtime-manifest.json \
    --aether-version "$AETHER_VERSION" >/dev/null

cargo test --quiet -p aether-runtime-catalog --lib --bins --tests
./scripts/test-runtime-manifest.sh

if rg -n 'full_distribution_pack_runtime|FULL_DISTRIBUTION_PROTOCOLS' \
    services tools examples libs --glob '*.rs'; then
    echo "ERROR: a production or composition root still assumes a full runtime catalog" >&2
    exit 1
fi

for source in services/automation/src/bootstrap.rs tools/aether/src/mcp.rs; do
    if ! rg -q 'load_runtime_manifest_for_current_process' "$source"; then
        echo "ERROR: $source does not fail closed on the shared runtime manifest" >&2
        exit 1
    fi
done

if ! rg -q 'print-default-features' scripts/build-installer.sh \
    || ! rg -q 'RUNTIME_MANIFEST_PATH' scripts/build-installer.sh; then
    echo "ERROR: installer build does not share the manifest feature source" >&2
    exit 1
fi

echo "Runtime-manifest contract passed"
