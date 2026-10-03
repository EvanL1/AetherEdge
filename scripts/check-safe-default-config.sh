#!/usr/bin/env bash

set -euo pipefail

readonly DEFAULT_IO="config.template/io/io.yaml"
readonly DEFAULT_AUTOMATION="config.template/automation/automation.yaml"
readonly DEFAULT_INSTANCES="config.template/automation/instances.yaml"

fail() {
    echo "ERROR: $*"
    exit 1
}

require_exact_setting() {
    local file="$1"
    local pattern="$2"
    local description="$3"

    if ! rg -q "$pattern" "$file"; then
        fail "$description: $file"
    fi
}

find_enabled_rules() {
    local path="$1"

    [[ -d "$path" ]] || return 0
    rg -n '"enabled"[[:space:]]*:[[:space:]]*true' "$path" --glob '*.json' || true
}

echo "Checking fail-safe default distribution config..."

require_exact_setting "$DEFAULT_IO" '^channels:[[:space:]]*\[\][[:space:]]*$' \
    "default io config must start with no channels"
require_exact_setting "$DEFAULT_AUTOMATION" '^auto_load_instances:[[:space:]]*false([[:space:]#]|$)' \
    "default automation config must not auto-load instances"
require_exact_setting "$DEFAULT_INSTANCES" '^instances:[[:space:]]*\{\}[[:space:]]*$' \
    "default automation config must contain no device instances"

if unsafe_endpoints=$(rg -n '(192\.168\.|/dev/tty|device:[[:space:]]*"?can[0-9])' config.template || true) \
    && [[ -n "$unsafe_endpoints" ]]; then
    echo "$unsafe_endpoints"
    fail "default config contains a concrete network or hardware endpoint"
fi

if energy_defaults=$(rg -ni '\b(PCS|BAMS|battery|diesel|PVInverter|generator|SOC)\b' config.template || true) \
    && [[ -n "$energy_defaults" ]]; then
    echo "$energy_defaults"
    fail "energy-domain examples must live in the opt-in energy pack"
fi

if enabled_rules=$(find_enabled_rules config.template/automation/rules) \
    && [[ -n "$enabled_rules" ]]; then
    echo "$enabled_rules"
    fail "default config contains an enabled control rule"
fi

echo "Fail-safe default distribution config passed"
