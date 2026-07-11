#!/usr/bin/env bash

set -euo pipefail

readonly DEFAULT_IO="config.template/io/io.yaml"
readonly DEFAULT_AUTOMATION="config.template/automation/automation.yaml"
readonly DEFAULT_INSTANCES="config.template/automation/instances.yaml"
readonly ENERGY_MANIFEST="packs/energy/pack.yaml"
readonly ENERGY_EXAMPLES="packs/energy/examples/config"

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

find_enabled_channels() {
    local file="$1"

    awk '
        /^[[:space:]]{2}- id:/ { in_channel = 1 }
        in_channel && /^[[:space:]]{4}enabled:[[:space:]]*true([[:space:]#]|$)/ {
            print FNR ":" $0
        }
    ' "$file"
}

require_all_channels_explicitly_disabled() {
    local file="$1"
    local channel_count
    local disabled_count

    channel_count=$(rg -c '^[[:space:]]{2}- id:' "$file" || true)
    disabled_count=$(rg -c '^[[:space:]]{4}enabled:[[:space:]]*false([[:space:]#]|$)' "$file" || true)
    if [[ "$channel_count" != "$disabled_count" ]]; then
        fail "every example channel must declare enabled: false: $file"
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

if rg -n 'config\.template/' "$ENERGY_MANIFEST"; then
    fail "energy pack must own its examples rather than reference default config"
fi
require_exact_setting "$ENERGY_MANIFEST" '^  aether:[[:space:]]*">=0\.5\.0,<0\.6\.0"[[:space:]]*$' \
    "energy distribution must declare its compatible Aether release range"
require_exact_setting "$ENERGY_MANIFEST" '^  composition:[[:space:]]*examples/energy-gateway[[:space:]]*$' \
    "energy distribution must declare its conformance composition"

for required_example in \
    "$ENERGY_EXAMPLES/io/io.yaml" \
    "$ENERGY_EXAMPLES/automation/automation.yaml" \
    "$ENERGY_EXAMPLES/automation/instances.yaml" \
    "$ENERGY_EXAMPLES/automation/rules/battery_soc_management.json"; do
    [[ -s "$required_example" ]] || fail "missing energy-pack example: $required_example"
done

if enabled_channels=$(find_enabled_channels "$ENERGY_EXAMPLES/io/io.yaml") \
    && [[ -n "$enabled_channels" ]]; then
    echo "$enabled_channels"
    fail "energy-pack examples must not enable hardware channels"
fi
require_all_channels_explicitly_disabled "$ENERGY_EXAMPLES/io/io.yaml"

if enabled_rules=$(find_enabled_rules "$ENERGY_EXAMPLES/automation/rules") \
    && [[ -n "$enabled_rules" ]]; then
    echo "$enabled_rules"
    fail "energy-pack examples must not enable control rules"
fi
for rule in "$ENERGY_EXAMPLES"/automation/rules/*.json; do
    require_exact_setting "$rule" '"enabled"[[:space:]]*:[[:space:]]*false' \
        "every energy-pack rule example must declare enabled: false"
done

if rg -n '^[[:space:]]{2}enabled:[[:space:]]*true([[:space:]#]|$)' \
    "$ENERGY_EXAMPLES/automation/instances" --glob '*.yaml'; then
    fail "energy-pack instance examples must not be enabled"
fi

require_exact_setting "$ENERGY_EXAMPLES/automation/automation.yaml" \
    '^auto_load_instances:[[:space:]]*false([[:space:]#]|$)' \
    "energy-pack examples must require explicit instance activation"

echo "Fail-safe default distribution config passed"
