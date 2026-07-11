#!/usr/bin/env bash

set -euo pipefail

readonly CORE_MANIFEST_PATTERN='^(redis|sqlx|bb8|bb8-redis|axum|rumqttc|workspace-hack)[[:space:]]*='
readonly DEFAULT_GRAPH_PATTERN='^(redis|sqlx|sqlx-core|sqlx-postgres|bb8|bb8-redis|workspace-hack) v'
readonly PERIPHERAL_GRAPH_PATTERN='^(redis|sqlx-postgres|tokio-postgres|postgres-types|postgres-protocol|bb8|bb8-redis|workspace-hack) v'

echo "Checking core manifests for infrastructure dependencies..."
if rg -n "$CORE_MANIFEST_PATTERN" crates --glob 'Cargo.toml'; then
    echo "ERROR: core crates contain a forbidden infrastructure dependency"
    exit 1
fi

echo "Checking core source for legacy RTDB coupling..."
if rg -n '\b(Rtdb|RedisRtdb)\b' crates --glob '*.rs'; then
    echo "ERROR: core crates reference the legacy Redis-shaped RTDB abstraction"
    exit 1
fi

echo "Checking extracted SHM boundary..."
if rg -n 'aether-rtdb-shm' extensions/shm-bridge/Cargo.toml; then
    echo "ERROR: SHM bridge depends on the legacy aggregation crate"
    exit 1
fi
legacy_core_file=$(find libs/aether-rtdb-shm/src/core -maxdepth 1 -type f -name '*.rs' -print -quit 2>/dev/null || true)
if [[ -e libs/aether-rtdb-shm/src/core.rs || -n "$legacy_core_file" ]]; then
    echo "ERROR: physical SHM core still exists in the legacy crate"
    exit 1
fi
if git check-ignore -q examples/minimal-gateway/Cargo.toml; then
    echo "ERROR: minimal gateway example is ignored by git"
    exit 1
fi
if git check-ignore -q examples/energy-gateway/Cargo.toml; then
    echo "ERROR: energy gateway example is ignored by git"
    exit 1
fi

echo "Checking default Cargo graph..."
dependency_tree=$(mktemp)
trap 'rm -f "$dependency_tree"' EXIT
cargo tree --edges normal --prefix none > "$dependency_tree"
if rg -n "$DEFAULT_GRAPH_PATTERN" "$dependency_tree"; then
    echo "ERROR: default Cargo graph includes an external database dependency"
    exit 1
fi

echo "Checking kernel/distribution composition boundary..."
cargo tree -p aether-example-minimal-gateway --edges normal --prefix none > "$dependency_tree"
if rg -ni '(aether-example-energy-gateway|packs?/energy|aether-ems)' "$dependency_tree"; then
    echo "ERROR: the industry-neutral gateway depends on the energy distribution"
    exit 1
fi
cargo tree -p aether-example-energy-gateway --edges normal --prefix none > "$dependency_tree"
if ! rg -q '^aether-edge-sdk v' "$dependency_tree"; then
    echo "ERROR: the energy distribution does not compose the Aether SDK"
    exit 1
fi

echo "Checking isolated peripheral service graphs..."
for service in aether-alarm aether-api aether-history aether-uplink; do
    cargo tree -p "$service" --edges normal --prefix none > "$dependency_tree"
    if rg -n "$PERIPHERAL_GRAPH_PATTERN" "$dependency_tree"; then
        echo "ERROR: $service default graph includes Redis/PostgreSQL/workspace-hack"
        exit 1
    fi
done

echo "Checking canonical service names..."
./scripts/check-service-names.sh

echo "Checking SHM-only core runtime..."
./scripts/check-shm-only-runtime.sh

./scripts/check-safe-default-config.sh
./scripts/test-installer-layout.sh

echo "Checking fresh-checkout path contract..."
if rg -n 'LEGACY_INSTALL_ROOT' tools/aether/src/install_context.rs; then
    echo "ERROR: an unregistered old installation can still override fresh-checkout paths"
    exit 1
fi
if ! rg -Fq 'working_data_directory.join("config")' tools/aether/src/install_context.rs; then
    echo "ERROR: CLI checkout configuration does not default to data/config"
    exit 1
fi
compose_config_mounts=$(grep -Fc '${AETHER_BASE_PATH:-./data}/config' docker-compose.yml)
if [[ "$compose_config_mounts" -lt 2 ]]; then
    echo "ERROR: Compose configuration mounts no longer match the CLI checkout data root"
    exit 1
fi

echo "Checking no_std domain build..."
cargo check -p aether-domain --no-default-features

echo "Checking AI-native contract files..."
for contract in AGENTS.md ARCHITECTURE.md llms.txt ai/catalog.yaml ai/invariants.md ai/safety-policy.yaml; do
    if [[ ! -s "$contract" ]]; then
        echo "ERROR: required AI-native contract is missing or empty: $contract"
        exit 1
    fi
done

echo "Architecture boundaries passed"
