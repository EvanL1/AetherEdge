#!/usr/bin/env bash

set -euo pipefail

readonly CORE_MANIFEST_PATTERN='^(redis|sqlx|bb8|bb8-redis|axum|reqwest|rumqttc|aether-http-data-processor|aether-http-history-query|aether-sqlite-history-query|workspace-hack)[[:space:]]*='
readonly DEFAULT_GRAPH_PATTERN='^(redis|sqlx|sqlx-core|sqlx-postgres|bb8|bb8-redis|workspace-hack) v'
readonly PERIPHERAL_GRAPH_PATTERN='^(redis|sqlx-postgres|tokio-postgres|postgres-types|postgres-protocol|bb8|bb8-redis|workspace-hack) v'
readonly ACTION_ROUTING_MUTATION_SQL_PATTERN='(?i)(?:r#{0,8})?"[[:space:]]*(?:INSERT(?:[[:space:]]+OR[[:space:]]+[A-Z_]+)?[[:space:]]+INTO|REPLACE[[:space:]]+INTO|UPDATE|DELETE[[:space:]]+FROM)[[:space:]]+action_routing\b'
readonly LEGACY_ACTION_ROUTING_MANAGER_PATTERN='\.[[:space:]]*(?:upsert_action_routing|delete_action_routing|toggle_action_routing|delete_all_routing)[[:space:]]*\('
readonly AUTOMATION_CONFIGURATION_MUTATION_SQL_PATTERN='(?i)(?:r#{0,8})?"[[:space:]]*(?:INSERT(?:[[:space:]]+OR[[:space:]]+[A-Z_]+)?[[:space:]]+INTO|REPLACE[[:space:]]+INTO|UPDATE|DELETE[[:space:]]+FROM)[[:space:]]+(?:measurement_routing|action_routing|rules|instances|instance_properties)\b'
readonly POINT_CONFIGURATION_MUTATION_SQL_PATTERN='(?i)(?:r#{0,8})?"[[:space:]]*(?:INSERT(?:[[:space:]]+OR[[:space:]]+[A-Z_]+)?[[:space:]]+INTO|REPLACE[[:space:]]+INTO|UPDATE|DELETE[[:space:]]+FROM)[[:space:]]+(?:telemetry_points|signal_points|control_points|adjustment_points)\b'

production_rust_source() {
    local source_file=$1
    local test_module_line

    test_module_line=$(awk '
        /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\{/ {
            print NR
            exit
        }
        /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/ {
            test_attribute_line = NR
            next
        }
        test_attribute_line && /^[[:space:]]*mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\{/ {
            print test_attribute_line
            exit
        }
        test_attribute_line && /^[[:space:]]*#\[/ {
            next
        }
        test_attribute_line && $0 !~ /^[[:space:]]*$/ {
            test_attribute_line = 0
        }
    ' "$source_file")

    if [[ -n "$test_module_line" ]]; then
        sed -n "1,$((test_module_line - 1))p" "$source_file"
    else
        sed -n '1,$p' "$source_file"
    fi
}

check_action_routing_mutation_boundary() {
    local source_root=$1
    local source_directory
    local source_file
    local relative_source
    local mutation_matches
    local legacy_manager_matches
    local violations_found=0

    for source_directory in services/automation/src/api tools/aether/src; do
        if [[ ! -d "$source_root/$source_directory" ]]; then
            continue
        fi
        while IFS= read -r source_file; do
            relative_source=${source_file#"$source_root"/}
            mutation_matches=$(
                production_rust_source "$source_file" \
                    | rg -n -U "$ACTION_ROUTING_MUTATION_SQL_PATTERN" || true
            )
            if [[ -n "$mutation_matches" ]]; then
                printf '%s:%s\n' "$relative_source" "$mutation_matches"
                violations_found=1
            fi

            legacy_manager_matches=$(
                production_rust_source "$source_file" \
                    | rg -n -U "$LEGACY_ACTION_ROUTING_MANAGER_PATTERN" || true
            )
            if [[ -n "$legacy_manager_matches" ]]; then
                printf '%s:%s\n' "$relative_source" "$legacy_manager_matches"
                violations_found=1
            fi
        done < <(rg --files "$source_root/$source_directory" --glob '*.rs')
    done

    [[ "$violations_found" -eq 0 ]]
}

enforce_action_routing_mutation_boundary() {
    local source_root=$1

    echo "Checking governed action-routing mutation boundary..."
    if ! check_action_routing_mutation_boundary "$source_root"; then
        echo "ERROR: production API/CLI code bypasses the governed action-routing application boundary"
        return 1
    fi
}

check_channel_management_mutation_boundary() {
    local source_root=$1
    local handler="$source_root/services/io/src/api/handlers/channel_management_handlers.rs"
    local legacy_directory="$source_root/services/io/src/api/handlers/channel_management_handlers"
    local reload_handler="$legacy_directory/reload.rs"
    local point_helper="$source_root/services/io/src/api/handlers/point_handlers/point_helpers.rs"
    local control_handler="$source_root/services/io/src/api/handlers/control_handlers.rs"
    local obsolete_reload="$source_root/services/io/src/core/reload.rs"
    local legacy_cli_reload="$source_root/tools/aether/src/services.rs"
    local violations_found=0
    local matches

    for removed_module in lifecycle.rs migration.rs; do
        if [[ -e "$legacy_directory/$removed_module" ]]; then
            printf '%s\n' "${legacy_directory#"$source_root"/}/$removed_module"
            violations_found=1
        fi
    done

    if [[ -f "$handler" ]]; then
        matches=$(
            production_rust_source "$handler" \
                | rg -n '\b(AppState|ChannelManager|SqlitePool)\b|sqlx::|\.sqlite_pool\b|\.channel_manager\b|State[[:space:]]*\(' \
                || true
        )
        if [[ -n "$matches" ]]; then
            printf '%s:%s\n' "${handler#"$source_root"/}" "$matches"
            violations_found=1
        fi

        if [[ $(production_rust_source "$handler" | grep -Fc 'Extension<ChannelManagementHttpBoundary>') -ne 4 ]]; then
            printf '%s:%s\n' "${handler#"$source_root"/}" \
                "channel mutation routes must inject the governed application boundary"
            violations_found=1
        fi
    fi

    if [[ -f "$obsolete_reload" ]]; then
        printf '%s:%s\n' "${obsolete_reload#"$source_root"/}" \
            "duplicate ReloadableService runtime owner is forbidden"
        violations_found=1
    fi

    if [[ -f "$legacy_cli_reload" ]]; then
        matches=$(
            production_rust_source "$legacy_cli_reload" \
                | rg -n '\bReload[[:space:]]*\{|/api/channels/reload' || true
        )
        if [[ -n "$matches" ]]; then
            printf '%s:%s\n' "${legacy_cli_reload#"$source_root"/}" "$matches"
            violations_found=1
        fi
    fi

    if [[ -f "$reload_handler" ]]; then
        matches=$(
            production_rust_source "$reload_handler" \
                | rg -n 'sqlx::|\.create_channel\b|\.remove_channel\b|\.connect\(\)|\.disconnect\(\)|respawn_channel' \
                || true
        )
        if [[ -n "$matches" ]]; then
            printf '%s:%s\n' "${reload_handler#"$source_root"/}" "$matches"
            violations_found=1
        fi
        if [[ $(production_rust_source "$reload_handler" | grep -Fc 'Extension<ChannelManagementHttpBoundary>') -ne 3 ]]; then
            printf '%s:%s\n' "${reload_handler#"$source_root"/}" \
                "canonical, single-channel, and compatibility reconciliation routes must inject the governed boundary"
            violations_found=1
        fi
    fi

    for owner in "$point_helper" "$control_handler"; do
        if [[ -f "$owner" ]]; then
            matches=$(
                production_rust_source "$owner" \
                    | rg -n 'tokio::spawn|\.create_channel\b|\.remove_channel\b|\.connect\(\)|\.disconnect\(\)|respawn_channel' \
                    || true
            )
            if [[ -n "$matches" ]]; then
                printf '%s:%s\n' "${owner#"$source_root"/}" "$matches"
                violations_found=1
            fi
        fi
    done

    [[ "$violations_found" -eq 0 ]]
}

enforce_channel_management_mutation_boundary() {
    local source_root=$1

    echo "Checking governed channel CRUD/lifecycle mutation boundary..."
    if ! check_channel_management_mutation_boundary "$source_root"; then
        echo "ERROR: channel CRUD/lifecycle HTTP mutations bypass the governed application boundary"
        return 1
    fi
}

check_configuration_mutation_boundaries() {
    local source_file
    local relative_source
    local matches
    local violations_found=0

    while IFS= read -r source_file; do
        case "$source_file" in
            *_tests.rs) continue ;;
        esac
        relative_source=${source_file#./}
        matches=$(
            production_rust_source "$source_file" \
                | rg -n -U "$AUTOMATION_CONFIGURATION_MUTATION_SQL_PATTERN" || true
        )
        if [[ -n "$matches" ]]; then
            printf '%s:%s\n' "$relative_source" "$matches"
            violations_found=1
        fi
    done < <(rg --files services/automation/src/api --glob '*.rs')

    while IFS= read -r source_file; do
        case "$source_file" in
            *_tests.rs) continue ;;
        esac
        relative_source=${source_file#./}
        matches=$(
            production_rust_source "$source_file" \
                | rg -n -U "$POINT_CONFIGURATION_MUTATION_SQL_PATTERN" || true
        )
        if [[ -n "$matches" ]]; then
            printf '%s:%s\n' "$relative_source" "$matches"
            violations_found=1
        fi
    done < <(rg --files services/io/src/api --glob '*.rs')

    for source_file in \
        services/automation/src/instance_manager.rs \
        services/automation/src/instance_routing.rs; do
        matches=$(
            rg -n \
                '(pub )?async fn (create_instance|rename_instance|delete_instance|collect_descendants|delete_single_instance|upsert_single_property|delete_single_property|upsert_(measurement|action)_routing|delete_(measurement|action)_routing|toggle_(measurement|action)_routing|delete_all_routing)' \
                "$source_file" 2>&1 || true
        )
        if [[ -n "$matches" ]]; then
            printf '%s:%s\n' "$source_file" "$matches"
            violations_found=1
        fi
    done

    [[ "$violations_found" -eq 0 ]]
}

enforce_configuration_mutation_boundaries() {
    echo "Checking governed configuration mutation boundaries..."
    if ! check_configuration_mutation_boundaries; then
        echo "ERROR: production HTTP or legacy manager code bypasses a governed configuration application boundary"
        return 1
    fi
}

if [[ "${AETHER_ARCHITECTURE_ACTION_ROUTING_ONLY:-0}" == "1" ]]; then
    enforce_action_routing_mutation_boundary "${AETHER_ARCHITECTURE_SOURCE_ROOT:-.}"
    exit 0
fi

if [[ "${AETHER_ARCHITECTURE_CHANNEL_MANAGEMENT_ONLY:-0}" == "1" ]]; then
    enforce_channel_management_mutation_boundary "${AETHER_ARCHITECTURE_SOURCE_ROOT:-.}"
    exit 0
fi

if [[ "${AETHER_ARCHITECTURE_CONFIGURATION_MUTATION_ONLY:-0}" == "1" ]]; then
    cd "${AETHER_ARCHITECTURE_SOURCE_ROOT:-.}"
    enforce_configuration_mutation_boundaries
    exit 0
fi

echo "Checking ADR numbering..."
duplicate_adr_ids=$(
    find docs/adr -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-*.md' -print \
        | sed 's#.*/##; s/-.*//' \
        | sort \
        | uniq -d
)
if [[ -n "$duplicate_adr_ids" ]]; then
    echo "ERROR: duplicate ADR identifiers: $duplicate_adr_ids"
    exit 1
fi
while IFS= read -r adr_path; do
    adr_id=$(basename "$adr_path" | cut -d- -f1)
    if ! head -1 "$adr_path" | rg -q "^# ADR-${adr_id}: "; then
        echo "ERROR: ADR heading does not match its filename: $adr_path"
        exit 1
    fi
done < <(find docs/adr -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-*.md' -print | sort)

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

echo "Checking acquisition-writer authority..."
if rg -n '\bAcquisitionStateWriter\b' \
    services/api services/automation services/alarm services/history services/uplink tools \
    --glob '*.rs'; then
    echo "ERROR: an application/interface process references the acquisition-only writer port"
    exit 1
fi

echo "Checking rule command boundary..."
if rg -n '\bActionDispatch\b|with_action_dispatch' libs/aether-rules --glob '*.rs'; then
    echo "ERROR: rule execution bypasses the governed application command facade"
    exit 1
fi

enforce_action_routing_mutation_boundary "."
./scripts/test-action-routing-architecture-boundary.sh
enforce_channel_management_mutation_boundary "."
./scripts/test-channel-management-architecture-boundary.sh
enforce_configuration_mutation_boundaries

echo "Checking channel-management safety policy..."
ruby -ryaml -e '
  capabilities = YAML.safe_load(File.read(ARGV.fetch(0))).fetch("capabilities")
  expected = {
    "kind" => "command",
    "risk" => "high",
    "permission" => "io.channel.manage",
    "idempotent" => false,
    "confirmation" => "always",
    "audit" => "required"
  }
  ["io.channel.manage", "io.channel.reconcile"].each do |name|
    policy = capabilities.fetch(name)
    abort "#{name} safety policy drifted: #{policy.inspect}" unless policy == expected
  end
' ai/safety-policy.yaml

echo "Checking production command transport boundary..."
if rg -n '\b(ActionDispatch|ShmDispatch|ActionWriter|ShmNotifier)\b' \
    crates extensions services libs tools \
    --glob '*.rs' \
    --glob '!libs/aether-rtdb-shm/**' \
    --glob '!**/tests/**' \
    --glob '!**/*_tests.rs' \
    --glob '!**/benches/**'; then
    echo "ERROR: production code calls the legacy command SHM compatibility surface"
    exit 1
fi

echo "Checking extracted SHM boundary..."
if rg -n 'aether-rtdb-shm' extensions/shm-bridge/Cargo.toml; then
    echo "ERROR: SHM bridge depends on the legacy aggregation crate"
    exit 1
fi
if [[ -e libs/aether-rtdb-shm ]]; then
    echo "ERROR: retired aether-rtdb-shm aggregation crate was restored"
    exit 1
fi
if rg -n 'aether-rtdb-shm' Cargo.toml .guppy/hakari.toml; then
    echo "ERROR: workspace metadata still references the retired aether-rtdb-shm crate"
    exit 1
fi
if rg -n '(aether-routing|aether_routing|RoutingCache)' \
    libs/aether-rules/Cargo.toml libs/aether-rules/src libs/aether-rules/tests \
    --glob '*.rs' --glob 'Cargo.toml'; then
    echo "ERROR: aether-rules restored the independently mutable legacy routing cache"
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
if git check-ignore -q libs/aether-runtime-catalog/src/bin/aether-runtime-manifest.rs; then
    echo "ERROR: runtime-manifest binary source is ignored by git"
    exit 1
fi
if [[ ! -s distributions/aetherems/runtime-io-features.txt ]]; then
    echo "ERROR: AetherEMS runtime IO feature authority is missing"
    exit 1
fi
if ! rg -q 'distributions/aetherems/runtime-io-features.txt' \
    scripts/check-extraction-readiness.sh; then
    echo "ERROR: extraction gate does not use the AetherEMS runtime feature authority"
    exit 1
fi
if rg -q 'distributions/aetherems|aetherems-energy-pack' \
    .github/workflows/release.yml; then
    echo "ERROR: Kernel release workflow must not publish an AetherEMS composition"
    exit 1
fi

echo "Checking removed topology and product compatibility entry points..."
if [[ -e services/io/src/store/shm_manifest.rs \
    || -e services/automation/src/infra/shm_manifest.rs ]]; then
    echo "ERROR: a service-local SHM manifest forwarding shim was restored"
    exit 1
fi
if rg -n '\b(LegacyRoutingTables|RoutingCache|compatibility_routing|routing_cache)\b|aether_routing' \
    services/automation/src libs/aether-rules/src; then
    echo "ERROR: automation restored the mutable legacy routing projection"
    exit 1
fi
if rg -n '\b(get_builtin_products|get_builtin_product|get_product_names|get_child_products|builtin_only)\b' \
    libs/aether-model/src services/automation/src tools/aether/src \
    --glob '*.rs'; then
    echo "ERROR: removed built-in product compatibility API was restored"
    exit 1
fi
if rg -n '\bpoint_mappings\b' \
    services/automation/src/product_loader.rs \
    services/io/src/channel_mutator.rs \
    services/io/src/automatic_reconciliation.rs; then
    echo "ERROR: removed point_mappings compatibility projection was restored"
    exit 1
fi
legacy_manifest_slot_count=$(rg -c \
    'pub fn slot\(&self, channel_id: u32, kind: PointKind, point_id: u32\)' \
    extensions/shm-bridge/src/manifest.rs || true)
if [[ "$legacy_manifest_slot_count" -ne 1 ]]; then
    echo "ERROR: published ChannelPointManifest::slot compatibility surface changed"
    exit 1
fi
if ! rg -q -U \
    'pub fn slot\(&self, channel_id: u32, kind: PointKind, point_id: u32\) -> Option<usize> \{[[:space:]]+self\.slot_for\(PhysicalPointAddress::from_legacy_raw\([[:space:]]+channel_id, kind, point_id,[[:space:]]+\)\)[[:space:]]+\}' \
    extensions/shm-bridge/src/manifest.rs; then
    echo "ERROR: ChannelPointManifest::slot must delegate directly to typed slot_for"
    exit 1
fi
if rg -n '(\b[A-Za-z_][A-Za-z0-9_]*manifest(?:\(\))?\.slot\(|\bChannelPointManifest::slot\()' \
    crates services extensions tools examples libs/aether-rules \
    --glob '*.rs' \
    --glob '!extensions/shm-bridge/src/manifest.rs' \
    --glob '!**/tests/**' \
    --glob '!**/*tests.rs' \
    --glob '!**/test_utils.rs'; then
    echo "ERROR: production code called the raw-ID ChannelPointManifest::slot compatibility shim"
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

# `--edges normal` is intentional: every shipped service and CLI graph must be
# independent of the deleted aggregation crate.
echo "Checking production graphs for the legacy SHM aggregate..."
if rg -n 'aether[_-]rtdb[_-]shm' \
    services/io/src services/automation/src services/alarm/src services/api/src \
    services/history/src services/uplink/src libs/aether-rules/src tools/aether/src \
    --glob '*.rs' \
    --glob '!**/*tests.rs' \
    --glob '!**/test_utils.rs'; then
    echo "ERROR: production source still imports the legacy SHM aggregate"
    exit 1
fi
for package in \
    aether-io aether-automation aether-alarm aether-api aether-history aether-uplink \
    aether-rules aether; do
    cargo tree -p "$package" --edges normal --prefix none > "$dependency_tree"
    if rg -n '^aether-rtdb-shm v' "$dependency_tree"; then
        echo "ERROR: $package production graph still includes aether-rtdb-shm"
        exit 1
    fi
done

echo "Checking kernel/distribution composition boundary..."
if [[ -e apps || -e scripts/systemd/aether-apps.service ]]; then
    echo "ERROR: the headless Kernel repository restored an EMS Console owner"
    exit 1
fi
if rg -n 'aether-apps|apps/(dist|nginx)|FRONTEND_INCLUDED|INCLUDE_FRONTEND|INCLUDE_NGINX' \
    docker-compose.yml \
    scripts/build-installer.sh \
    scripts/build-static-deps.sh \
    scripts/install-baremetal.sh \
    scripts/install.sh \
    scripts/offline/build-docker-arm64.sh \
    tools/aether/src/services.rs; then
    echo "ERROR: the Kernel distribution still owns EMS Console integration"
    exit 1
fi
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

echo "Checking data-processing adapter direction..."
cargo tree -p aether-data-processing --edges normal --prefix none > "$dependency_tree"
if rg -q '^aether-http-data-processor v' "$dependency_tree"; then
    echo "ERROR: the transport-neutral data-processing codec depends on the HTTP adapter"
    exit 1
fi
cargo tree -p aether-http-data-processor --edges normal --prefix none > "$dependency_tree"
if ! rg -q '^aether-data-processing v' "$dependency_tree"; then
    echo "ERROR: the HTTP processor adapter does not compose the shared wire codec"
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
./scripts/check-runtime-manifest.sh
./scripts/check-energy-pack-boundary.sh
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
