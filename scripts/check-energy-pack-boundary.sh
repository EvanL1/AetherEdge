#!/usr/bin/env bash

set -euo pipefail

readonly LEGACY_MODELS="libs/aether-model/src/products"
readonly PACK_MODELS="packs/energy/models"
readonly PACK_KNOWLEDGE="packs/energy/knowledge"
readonly ENERGY_HOMEPAGE_PRESET="packs/energy/examples/config/api/calculated_points.sql"
readonly FORMAL_ASSET_CATEGORIES=(mappings rules evaluations)

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

if legacy_model=$(find "$LEGACY_MODELS" -maxdepth 1 -type f -name '*.json' -print -quit); then
    if [[ -n "$legacy_model" ]]; then
        fail "energy model remains outside the pack: $legacy_model"
    fi
fi

model_count=$(find "$PACK_MODELS" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' ')
[[ "$model_count" == 13 ]] || fail "energy pack must own exactly 13 model JSON files"

knowledge_count=$(find "$PACK_KNOWLEDGE" -maxdepth 1 -type f -name '*.md' | wc -l | tr -d ' ')
[[ "$knowledge_count" == 5 ]] || fail "energy pack must own exactly 5 knowledge pages"

[[ ! -e services/api/assets/calculated_points.sql ]] \
    || fail "core API still owns the Energy homepage calculated-point preset"
[[ -s "$ENERGY_HOMEPAGE_PRESET" ]] \
    || fail "Energy Pack homepage commissioning preset is missing"
preset_point_count=$(rg -c '^\(' "$ENERGY_HOMEPAGE_PRESET" || true)
[[ "$preset_point_count" == 19 ]] \
    || fail "Energy Pack homepage commissioning preset must preserve exactly 19 legacy points"

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
    --glob '!scripts/check-energy-pack-boundary.sh' \
    --glob '!docs/plans/**' \
    --glob '!docs/superpowers/**'; then
    fail "an executable or build-time reference still resolves a legacy energy asset path"
fi

if rg -n 'include(_str)?!\([^)]*packs/energy|product_includes\.rs' \
    libs/aether-model tools/aether --glob '*.rs'; then
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
if rg -n 'aether://docs/domain/' README.md docs docs-site tools services libs examples packs \
    --glob '!node_modules/**' \
    --glob '!dist/**' \
    --glob '!docs/plans/**' \
    --glob '!docs/superpowers/**' \
    --glob '!docs/specs/**'; then
    fail "current documentation or tests still publish the pre-Pack MCP URI namespace"
fi

if ! rg -q '^  models:[[:space:]]*models[[:space:]]*$' packs/energy/pack.yaml; then
    fail "Pack v1 manifest does not declare the models directory"
fi
if ! rg -q '^  knowledge:[[:space:]]*knowledge[[:space:]]*$' packs/energy/pack.yaml; then
    fail "Pack v1 manifest does not declare the knowledge directory"
fi

for category in "${FORMAL_ASSET_CATEGORIES[@]}"; do
    if ! rg -q "^  ${category}:[[:space:]]*${category}[[:space:]]*$" packs/energy/pack.yaml; then
        fail "Pack v1 manifest does not declare the ${category} directory"
    fi
    [[ -f "packs/energy/${category}/index.yaml" ]] \
        || fail "energy ${category} asset index is missing"
done

if ! rg -q '^  data_processing:[[:space:]]*data-processing/tasks[[:space:]]*$' \
    packs/energy/pack.yaml; then
    fail "Pack v1 manifest does not declare the Data Processing task directory"
fi
[[ -f packs/energy/data-processing/tasks/index.yaml ]] \
    || fail "Energy Data Processing task index is missing"

if [[ -e packs/energy/examples/config/automation/rules/battery_soc_management.json ]]; then
    fail "formal energy rule remains embedded under examples/config"
fi

if rg -n \
    'battery_pack|diesel_generator|pv_inverter|PV_DCDC|PV DCDC|Legacy product name|normalize_product_name|get_builtin_product\(' \
    tools/aether/src/core --glob '*.rs'; then
    fail "generic CLI/schema still hard-codes Energy product compatibility names"
fi

for migration in \
    packs/energy/mappings/product-name-aliases.yaml \
    packs/energy/mappings/legacy-instance-properties-v5.yaml; do
    if ! rg -q '^  removed_from_kernel:[[:space:]]*0\.5\.0[[:space:]]*$' "$migration"; then
        fail "Energy compatibility mapping lacks an explicit kernel-removal version: $migration"
    fi
done

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
