#!/usr/bin/env bash

set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

failures=()

run_contract() {
    local label=$1
    shift

    echo "=== $label ==="
    if "$@"; then
        return 0
    fi

    failures+=("$label")
    return 0
}

run_contract "canonical service names" ./scripts/check-service-names.sh
run_contract "SHM-only runtime composition" ./scripts/check-shm-only-runtime.sh
run_contract "fail-safe default configuration" ./scripts/check-safe-default-config.sh
run_contract "runtime manifest" ./scripts/check-runtime-manifest.sh
run_contract "Energy Pack boundary" ./scripts/check-energy-pack-boundary.sh
run_contract "installer layout" ./scripts/test-installer-layout.sh

if ((${#failures[@]} > 0)); then
    printf 'ERROR: distribution contract failures:\n' >&2
    printf '  - %s\n' "${failures[@]}" >&2
    exit 1
fi

echo "Distribution contracts passed"
