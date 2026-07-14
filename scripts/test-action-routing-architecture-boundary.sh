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

    mkdir -p \
        "$fixture_root/services/automation/src/api" \
        "$fixture_root/services/automation/src/infra" \
        "$fixture_root/tools/aether/src/core"
    printf '%s\n' "$fixture_root"
}

run_boundary_check() {
    local fixture_root=$1

    (
        cd "$fixture_root"
        AETHER_ARCHITECTURE_SOURCE_ROOT="$fixture_root" \
            AETHER_ARCHITECTURE_ACTION_ROUTING_ONLY=1 \
            "$ARCHITECTURE_CHECK"
    )
}

run_configuration_boundary_check() {
    local fixture_root=$1

    (
        cd "$fixture_root"
        AETHER_ARCHITECTURE_SOURCE_ROOT="$fixture_root" \
            AETHER_ARCHITECTURE_CONFIGURATION_MUTATION_ONLY=1 \
            "$ARCHITECTURE_CHECK"
    )
}

assert_allowed() {
    local fixture_root=$1
    local output

    if ! output=$(run_boundary_check "$fixture_root" 2>&1); then
        printf 'expected boundary fixture to pass: %s\n%s\n' "$fixture_root" "$output" >&2
        exit 1
    fi
}

assert_rejected() {
    local fixture_root=$1
    local expected_path=$2
    local output

    if output=$(run_boundary_check "$fixture_root" 2>&1); then
        printf 'expected boundary fixture to fail: %s\n' "$fixture_root" >&2
        exit 1
    fi
    if [[ "$output" != *"$expected_path"* ]]; then
        printf 'expected failure to identify %s, got:\n%s\n' "$expected_path" "$output" >&2
        exit 1
    fi
}

allowed_fixture=$(create_fixture allowed_boundaries)
cat > "$allowed_fixture/services/automation/src/api/read_only.rs" <<'RUST'
async fn read_routes(pool: &sqlx::SqlitePool) {
    sqlx::query("SELECT action_id FROM action_routing WHERE instance_id = ?")
        .fetch_all(pool)
        .await;
}
RUST
cat > "$allowed_fixture/services/automation/src/infra/action_routing.rs" <<'RUST'
async fn mutate_in_formal_adapter(pool: &sqlx::SqlitePool) {
    sqlx::query("DELETE FROM action_routing WHERE instance_id = ?")
        .execute(pool)
        .await;
}
RUST
cat > "$allowed_fixture/tools/aether/src/routing.rs" <<'RUST'
async fn use_governed_http(client: &RoutingClient) {
    client.upsert_action_route(7, 1, 3, "A", 5, true, true).await;
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod atomic_sync_tests {
    async fn fixture_sql(pool: &sqlx::SqlitePool) {
        sqlx::query("INSERT INTO action_routing (instance_id) VALUES (7)")
            .execute(pool)
            .await;
    }
}
RUST
cat > "$allowed_fixture/tools/aether/src/core/schema.rs" <<'RUST'
async fn install_cleanup_trigger(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "CREATE TRIGGER cleanup_action_route AFTER DELETE ON control_points
         BEGIN
             DELETE FROM action_routing WHERE channel_id = OLD.channel_id;
         END",
    )
    .execute(pool)
    .await;
}
RUST
assert_allowed "$allowed_fixture"

api_sql_fixture=$(create_fixture api_direct_mutation_sql)
cat > "$api_sql_fixture/services/automation/src/api/handler.rs" <<'RUST'
async fn bypass(pool: &sqlx::SqlitePool) {
    sqlx::query("DELETE FROM action_routing WHERE instance_id = ?")
        .execute(pool)
        .await;
}
RUST
assert_rejected "$api_sql_fixture" "services/automation/src/api/handler.rs"

tool_sql_fixture=$(create_fixture tool_direct_mutation_sql)
cat > "$tool_sql_fixture/tools/aether/src/core/syncer.rs" <<'RUST'
async fn bypass(pool: &sqlx::SqlitePool) {
    sqlx::query(
        r#"
        INSERT OR REPLACE INTO action_routing (instance_id, action_id)
        VALUES (?, ?)
        "#,
    )
    .execute(pool)
    .await;
}
RUST
assert_rejected "$tool_sql_fixture" "tools/aether/src/core/syncer.rs"

manager_fixture=$(create_fixture legacy_manager_mutation)
cat > "$manager_fixture/services/automation/src/api/handler.rs" <<'RUST'
async fn bypass(state: &AppState) {
    state
        .instance_manager
        .toggle_action_routing(7, 1, true)
        .await;
}
RUST
assert_rejected "$manager_fixture" "services/automation/src/api/handler.rs"

test_only_manager_fixture=$(create_fixture retired_test_only_manager_mutation)
mkdir -p "$test_only_manager_fixture/services/io/src/api"
: > "$test_only_manager_fixture/services/automation/src/instance_routing.rs"
cat > "$test_only_manager_fixture/services/automation/src/instance_manager.rs" <<'RUST'
impl InstanceManager {
    #[cfg(test)]
    pub async fn create_instance(&self, request: CreateInstanceRequest) {
        self.pool.execute(request).await;
    }
}
RUST
if output=$(run_configuration_boundary_check "$test_only_manager_fixture" 2>&1); then
    printf 'expected retired test-only manager mutation to fail\n' >&2
    exit 1
fi
if [[ "$output" != *"services/automation/src/instance_manager.rs"* ]]; then
    printf 'expected test-only manager failure path, got:\n%s\n' "$output" >&2
    exit 1
fi

echo "Action-routing architecture boundary tests passed"
