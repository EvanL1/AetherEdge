#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

readonly FORBIDDEN_GRAPH_PATTERN='^(aether-rtdb|redis|bb8|bb8-redis|sqlx-postgres|tokio-postgres|postgres-types|postgres-protocol|workspace-hack) v'
readonly FORBIDDEN_SOURCE_PATTERN='redis|aether_rtdb::|\bRtdbStateStore\b'
readonly RUNTIME_SERVICES=(
    aether-io
    aether-automation
    aether-history
    aether-api
    aether-uplink
    aether-alarm
    aether
)
readonly RUNTIME_SOURCE_DIRS=(
    services/io
    services/automation
    services/history
    services/api
    services/uplink
    services/alarm
)

dependency_tree=$(mktemp)
trap 'rm -f "$dependency_tree"' EXIT

for service in "${RUNTIME_SERVICES[@]}"; do
    cargo tree -p "$service" --edges normal --prefix none > "$dependency_tree"
    if rg -n "$FORBIDDEN_GRAPH_PATTERN" "$dependency_tree"; then
        echo "ERROR: $service default graph contains a legacy RTDB/Redis dependency" >&2
        exit 1
    fi
done

if rg -n --ignore-case \
    'common::redis|RedisRtdb|aether_rtdb::|AETHER_REDIS_URL|\bredis_key\b' \
    tools/aether/src --glob '*.rs'; then
    echo "ERROR: aether CLI still contains a direct Redis/legacy RTDB runtime path" >&2
    exit 1
fi

if rg -n 'ensure_shm_file_exists|Creating shared memory file|remove_dir_all\(shm_path' \
    tools/aether/src --glob '*.rs'; then
    echo "ERROR: aether CLI mutates the IO-owned SHM authority before startup" >&2
    exit 1
fi

for source_dir in "${RUNTIME_SOURCE_DIRS[@]}"; do
    if rg --files "$source_dir" | rg --ignore-case 'redis'; then
        echo "ERROR: $source_dir still contains a Redis-named source path" >&2
        exit 1
    fi
    if rg -n --ignore-case "$FORBIDDEN_SOURCE_PATTERN" "$source_dir" \
        --glob '*.rs' --glob 'Cargo.toml'; then
        echo "ERROR: $source_dir still contains a Redis-shaped runtime path" >&2
        exit 1
    fi
    if rg -n --ignore-case 'requires[[:space:]]+redis' "$source_dir" \
        --glob '*.rs'; then
        echo "ERROR: $source_dir still has Redis-gated tests" >&2
        exit 1
    fi
done

for source_dir in "${RUNTIME_SOURCE_DIRS[@]}"; do
    manifest="$source_dir/Cargo.toml"
    if ! rg -q '^aether-(rtdb-shm|shm-bridge)[[:space:]]*=' "$manifest"; then
        echo "ERROR: $manifest does not depend on the authoritative SHM data plane" >&2
        exit 1
    fi
done

if ! command -v openssl >/dev/null 2>&1; then
    echo "ERROR: openssl is required to generate ephemeral Compose test credentials" >&2
    exit 1
fi
readonly COMPOSE_TEST_SECRET="$(openssl rand -hex 32)"
readonly COMPOSE_TEST_UPLINK_TOKEN="$(openssl rand -hex 32)"
if [[ "$COMPOSE_TEST_SECRET" == "$COMPOSE_TEST_UPLINK_TOKEN" ]]; then
    echo "ERROR: generated Compose test credentials must be distinct" >&2
    exit 1
fi

default_services=""
if ! default_services=$(
    JWT_SECRET_KEY="$COMPOSE_TEST_SECRET" \
        AETHER_UPLINK_CONTROL_TOKEN="$COMPOSE_TEST_UPLINK_TOKEN" \
        docker compose config --services
); then
    echo "ERROR: default Compose runtime is invalid" >&2
    exit 1
fi
if rg -q '^aether-redis$' <<< "$default_services"; then
    echo "ERROR: Redis is part of the default Compose runtime" >&2
    exit 1
fi

redis_services=""
if ! redis_services=$(
    JWT_SECRET_KEY="$COMPOSE_TEST_SECRET" \
        AETHER_UPLINK_CONTROL_TOKEN="$COMPOSE_TEST_UPLINK_TOKEN" \
        docker compose --profile redis config --services
); then
    echo "ERROR: optional Redis infrastructure profile is invalid" >&2
    exit 1
fi
if ! rg -q '^aether-redis$' <<< "$redis_services"; then
    echo "ERROR: optional Redis infrastructure profile is missing" >&2
    exit 1
fi
