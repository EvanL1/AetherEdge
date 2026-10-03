#!/usr/bin/env bash

set -euo pipefail

readonly RETIRED_MODEL_CRATE="libs/aether-model"

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

[[ ! -e "$RETIRED_MODEL_CRATE" ]] \
    || fail "retired aether-model compatibility crate was restored"

[[ ! -e packs/energy ]] \
    || fail "Energy Pack assets belong in AetherEMS"
[[ ! -e examples/energy-gateway ]] \
    || fail "Energy composition and conformance tests belong in AetherEMS"

[[ ! -e services/api/assets/calculated_points.sql ]] \
    || fail "core API still owns the Energy homepage calculated-point preset"

if sed '/^#\[cfg(test)\]/,$d' services/api/src/db.rs | rg -n \
    'include_(str|bytes)!\([^)]*calculated_points|INSERT[[:space:]]+INTO[[:space:]]+calculated_points|PV Energy|Diesel Energy|Saving Billing|icon-(pv|diesel|ess)-energy|\bSOC\b' \
    -; then
    fail "core API initialization path embeds domain-specific homepage defaults"
fi
if [[ -d services/api/assets ]] && rg -n \
    'INSERT[[:space:]]+INTO[[:space:]]+calculated_points|PV Energy|Diesel Energy|Saving Billing|icon-(pv|diesel|ess)-energy|\bSOC\b' \
    services/api/assets; then
    fail "core API asset directory embeds domain-specific homepage defaults"
fi

if rg -n \
    'include_str!\([^)]*docs/domain/|join\("src/products"\)|rerun-if-changed=src/products' \
    . \
    --glob '!target/**' \
    --glob '!.git/**' \
    --glob '!scripts/check-energy-pack-boundary.sh'; then
    fail "an executable or build-time reference still resolves a legacy energy asset path"
fi

if rg -n 'include(_str)?!\([^)]*packs/energy|product_includes\.rs' \
    crates libs services tools --glob '*.rs'; then
    fail "kernel/model or CLI source still compiles Energy Pack assets into a binary"
fi

if ! rg -q '^packs:[[:space:]]*\[\][[:space:]]*$' config.template/global.yaml; then
    fail "safe global configuration must activate no domain Pack"
fi
if ! rg -q 'load_active_packs' services/automation/src/bootstrap.rs; then
    fail "automation does not consume the validated active Pack set"
fi
if ! rg -q 'from_active_pack_config' tools/aether/src/main.rs; then
    fail "aether MCP does not consume the shared active Pack configuration"
fi
if rg -n 'aether://docs/domain/' README.md docs tools services libs examples \
    --glob '!node_modules/**' \
    --glob '!dist/**' \
    --glob '!docs/specs/**'; then
    fail "current documentation or tests still publish the pre-Pack MCP URI namespace"
fi

if rg -n \
    'battery_pack|diesel_generator|pv_inverter|PV_DCDC|PV DCDC|Legacy product name|normalize_product_name|get_builtin_product\(' \
    tools/aether/src/core --glob '*.rs'; then
    fail "generic CLI/schema still hard-codes Energy product compatibility names"
fi

for schema in \
    contracts/pack/pack-artifact.v1.schema.json \
    contracts/pack/pack-manifest.v1.schema.json \
    contracts/pack/pack-asset-index.v1.schema.json \
    contracts/pack/mapping-set.v1.schema.json \
    contracts/pack/rule.v1.schema.json \
    contracts/pack/evaluation-suite.v1.schema.json \
    contracts/pack/data-processing-task.v1.schema.json; do
    [[ -s "$schema" ]] || fail "Pack asset schema is missing: $schema"
done

[[ -x scripts/build-pack-artifact.sh ]] \
    || fail "Pack-only artifact builder is missing or not executable"
if ! rg -q 'PackCommands::Install' tools/aether/src/pack_artifact.rs \
    || ! rg -q 'runtime_manifest_digest' tools/aether/src/pack_artifact.rs; then
    fail "Pack-only installer does not enforce the runtime binding"
fi

if rg -n '"\$ref":[[:space:]]*"[^#]' contracts/pack --glob '*.json'; then
    fail "Pack schemas must resolve locally without downloading remote references"
fi

echo "Energy pack asset boundary passed"
