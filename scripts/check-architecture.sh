#!/usr/bin/env bash

set -euo pipefail

readonly DEFAULT_GRAPH_PATTERN='^(redis|sqlx|sqlx-core|sqlx-postgres|bb8|bb8-redis|workspace-hack) v'
readonly PERIPHERAL_GRAPH_PATTERN='^(redis|sqlx-postgres|tokio-postgres|postgres-types|postgres-protocol|bb8|bb8-redis|workspace-hack) v'

echo "Checking typed repository architecture contracts..."
cargo test -p aether-architecture-tests \
    --test workspace_boundaries \
    --test source_boundaries

echo "Checking governed application-boundary behavior..."
cargo test -p aether-acquisition-port --test port_contract
cargo test -p aether-shm-bridge \
    --test acquisition_writer_contract \
    --test channel_manifest_contract
cargo test -p aether-application --test safety_policy_contract
cargo test -p aether-automation \
    --test active_pack_products \
    --test test_action_routing_boundary \
    --test test_measurement_routing_boundary \
    --test test_instance_configuration_boundary \
    --test test_rule_execution_boundary
cargo test -p aether-io \
    --test automatic_reconciliation_contract \
    --test channel_mutator_contract
cargo test -p aether-store-local --all-features --test sqlite_physical_topology_contract
cargo test -p aether-io --lib channel_management
cargo test -p aether-io --lib channel_reconciliation
cargo test -p aether-io --lib confirmed_channel_requests_forward_exact_typed_mutations

echo "Checking default Cargo graph..."
dependency_tree=$(mktemp)
trap 'rm -f "$dependency_tree"' EXIT
cargo tree --edges normal --prefix none > "$dependency_tree"
if rg -n "$DEFAULT_GRAPH_PATTERN" "$dependency_tree"; then
    echo "ERROR: default Cargo graph includes an external database dependency"
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

echo "Checking no_std domain build..."
cargo check -p aether-domain --no-default-features

echo "Architecture boundaries passed"
