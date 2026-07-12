#!/usr/bin/env bash

set -euo pipefail

REPOSITORY_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
readonly REPOSITORY_ROOT
readonly ARCHITECTURE_CHECK="$REPOSITORY_ROOT/scripts/check-architecture.sh"
FIXTURE_PARENT=$(mktemp -d)
readonly FIXTURE_PARENT

trap 'rm -rf "$FIXTURE_PARENT"' EXIT

create_fixture() {
    local fixture_name=$1
    local fixture_root="$FIXTURE_PARENT/$fixture_name"

    mkdir -p "$fixture_root/services/io/src/api/handlers/channel_management_handlers"
    printf '%s\n' "$fixture_root"
}

write_governed_handler() {
    local fixture_root=$1

    cat > "$fixture_root/services/io/src/api/handlers/channel_management_handlers.rs" <<'RUST'
async fn create(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
async fn update(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
async fn set_enabled(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
async fn delete(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
RUST
}

write_governed_reconciliation() {
    local fixture_root=$1

    cat > "$fixture_root/services/io/src/api/handlers/channel_management_handlers/reload.rs" <<'RUST'
async fn reconcile_all(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
async fn reconcile_one(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
async fn reload_alias(Extension(_: Extension<ChannelManagementHttpBoundary>)) {}
RUST
}

run_boundary_check() {
    local fixture_root=$1

    AETHER_ARCHITECTURE_SOURCE_ROOT="$fixture_root" \
        AETHER_ARCHITECTURE_CHANNEL_MANAGEMENT_ONLY=1 \
        "$ARCHITECTURE_CHECK"
}

assert_allowed() {
    local fixture_root=$1
    local output

    if ! output=$(run_boundary_check "$fixture_root" 2>&1); then
        printf 'expected channel boundary fixture to pass: %s\n%s\n' \
            "$fixture_root" "$output" >&2
        exit 1
    fi
}

assert_rejected() {
    local fixture_root=$1
    local expected_path=$2
    local output

    if output=$(run_boundary_check "$fixture_root" 2>&1); then
        printf 'expected channel boundary fixture to fail: %s\n' "$fixture_root" >&2
        exit 1
    fi
    if [[ "$output" != *"$expected_path"* ]]; then
        printf 'expected failure to identify %s, got:\n%s\n' \
            "$expected_path" "$output" >&2
        exit 1
    fi
}

allowed_fixture=$(create_fixture allowed_boundary)
write_governed_handler "$allowed_fixture"
write_governed_reconciliation "$allowed_fixture"
assert_allowed "$allowed_fixture"

direct_fixture=$(create_fixture direct_sqlite_and_runtime_access)
write_governed_handler "$direct_fixture"
write_governed_reconciliation "$direct_fixture"
cat >> "$direct_fixture/services/io/src/api/handlers/channel_management_handlers.rs" <<'RUST'
async fn bypass(State(state): State<AppState>) {
    sqlx::query("DELETE FROM channels").execute(&state.sqlite_pool).await;
    state.channel_manager.write().await.clear();
}
RUST
assert_rejected "$direct_fixture" \
    "services/io/src/api/handlers/channel_management_handlers.rs"

reload_fixture=$(create_fixture direct_reload_runtime_owner)
write_governed_handler "$reload_fixture"
write_governed_reconciliation "$reload_fixture"
cat >> "$reload_fixture/services/io/src/api/handlers/channel_management_handlers/reload.rs" <<'RUST'
async fn bypass(state: AppState) {
    state.channel_manager.remove_channel(7).await;
}
RUST
assert_rejected "$reload_fixture" \
    "services/io/src/api/handlers/channel_management_handlers/reload.rs"

legacy_fixture=$(create_fixture legacy_lifecycle_module)
write_governed_handler "$legacy_fixture"
cat > "$legacy_fixture/services/io/src/api/handlers/channel_management_handlers/lifecycle.rs" <<'RUST'
async fn direct_lifecycle() {}
RUST
assert_rejected "$legacy_fixture" \
    "services/io/src/api/handlers/channel_management_handlers/lifecycle.rs"

echo "Channel-management architecture boundary tests passed"
