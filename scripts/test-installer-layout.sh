#!/usr/bin/env bash
# shellcheck disable=SC2016,SC2034 # Literal snippets and sourced-helper globals are intentional.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT_DIR
readonly DOCKER_INSTALLER="$ROOT_DIR/scripts/install.sh"
readonly BARE_METAL_INSTALLER="$ROOT_DIR/scripts/install-baremetal.sh"
readonly INSTALLER_BUILDER="$ROOT_DIR/scripts/build-installer.sh"
readonly STATIC_DEP_BUILDER="$ROOT_DIR/scripts/build-static-deps.sh"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file=$1
    local expected=$2

    grep -Fq -- "$expected" "$file" \
        || fail "$file does not contain: $expected"
}

assert_not_contains() {
    local file=$1
    local unexpected=$2

    if grep -Fq -- "$unexpected" "$file"; then
        fail "$file still contains obsolete text: $unexpected"
    fi
}

# The production installer exposes only its filesystem helpers in this mode;
# no Docker, sudo, service, or host mutation is allowed while these tests run.
assert_contains "$DOCKER_INSTALLER" 'AETHER_INSTALLER_FUNCTIONS_ONLY'

# shellcheck disable=SC1090
AETHER_INSTALLER_FUNCTIONS_ONLY=true source "$DOCKER_INSTALLER"
# shellcheck disable=SC2034 # Sourced installer helpers consume this global.
SUDO=""

TEST_ROOT="$(mktemp -d)"
# macOS exposes /var as a compatibility symlink to /private/var. Use the
# physical temporary path so ancestor-symlink tests exercise only fixtures
# created by this test, matching the Linux target environment.
TEST_ROOT="$(cd "$TEST_ROOT" && pwd -P)"
trap 'rm -rf "$TEST_ROOT"' EXIT

file_mode() {
    local path=$1

    stat -c '%a' "$path" 2>/dev/null || stat -f '%Lp' "$path"
}

echo "Testing bootstrap administrator secrets are strong and install-only..."
is_valid_bootstrap_admin_password 'correct-horse-battery-staple' \
    || fail "a strong bootstrap administrator password was rejected"
if is_valid_bootstrap_admin_password 'change-me-in-production'; then
    fail "a documented bootstrap administrator default was accepted"
fi
if is_valid_bootstrap_admin_password ' leading-and-long-enough'; then
    fail "bootstrap administrator password accepted leading whitespace"
fi

bootstrap_env="$TEST_ROOT/bootstrap/.env"
bootstrap_db="$TEST_ROOT/bootstrap/aether.db"
mkdir -p "$(dirname "$bootstrap_env")"
printf 'JWT_SECRET_KEY=%064d\n' 0 > "$bootstrap_env"
ACTUAL_UID=$(id -u)
ACTUAL_GID=$(id -g)
bootstrap_output=$(ensure_compose_bootstrap_admin "$bootstrap_env" "$bootstrap_db")
bootstrap_password=$(sed -n 's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' "$bootstrap_env")
is_valid_bootstrap_admin_password "$bootstrap_password" \
    || fail "first Docker install did not persist a strong bootstrap password"
[[ "$(file_mode "$bootstrap_env")" == 600 ]] \
    || fail "Docker bootstrap credential file is not mode 0600"
[[ "$bootstrap_output" != *"$bootstrap_password"* ]] \
    || fail "Docker installer printed the bootstrap password"
assert_contains "$bootstrap_env" 'AETHER_ALLOW_PUBLIC_REGISTRATION=false'

echo "Testing device-control credentials are generated separately and kept private..."
(
    INSTALL_DIR="$TEST_ROOT/control-credentials"
    unset JWT_SECRET_KEY AETHER_UPLINK_CONTROL_TOKEN
    mkdir -p "$INSTALL_DIR"
    credential_log="$INSTALL_DIR/install.log"
    {
        ensure_compose_jwt_secret
        ensure_compose_uplink_control_token
    } > "$credential_log"
    jwt_secret=$(sed -n 's/^JWT_SECRET_KEY=//p' "$INSTALL_DIR/.env")
    uplink_token=$(sed -n 's/^AETHER_UPLINK_CONTROL_TOKEN=//p' "$INSTALL_DIR/.env")
    is_valid_jwt_secret "$jwt_secret" || fail "Docker installer generated a weak JWT secret"
    is_valid_jwt_secret "$uplink_token" || fail "Docker installer generated a weak uplink token"
    [[ "$jwt_secret" != "$uplink_token" ]] \
        || fail "JWT and uplink device-control credentials must be distinct"
    [[ "$(file_mode "$INSTALL_DIR/.env")" == 600 ]] \
        || fail "Docker device-control credential file is not mode 0600"
    ! grep -Fq "$jwt_secret" "$credential_log" \
        || fail "Docker installer printed the JWT secret"
    ! grep -Fq "$uplink_token" "$credential_log" \
        || fail "Docker installer printed the uplink control credential"
)

printf 'existing database\n' > "$bootstrap_db"
printf '%s\n' \
    'JWT_SECRET_KEY=existing-secret-that-is-at-least-thirty-two-bytes' \
    'AETHER_BOOTSTRAP_ADMIN_PASSWORD=operator-owned-bootstrap-secret' \
    'AETHER_ALLOW_PUBLIC_REGISTRATION=true' > "$bootstrap_env"
ensure_compose_bootstrap_admin "$bootstrap_env" "$bootstrap_db"
assert_contains "$bootstrap_env" 'AETHER_BOOTSTRAP_ADMIN_PASSWORD=operator-owned-bootstrap-secret'
assert_contains "$bootstrap_env" 'AETHER_ALLOW_PUBLIC_REGISTRATION=true'

unknown_env="$TEST_ROOT/unknown-database/.env"
unknown_db="$TEST_ROOT/unknown-database/aether.db"
mkdir -p "$(dirname "$unknown_env")"
printf 'JWT_SECRET_KEY=%064d\n' 0 > "$unknown_env"
printf 'database state cannot be inspected\n' > "$unknown_db"
ensure_compose_bootstrap_admin "$unknown_env" "$unknown_db"
unknown_password=$(sed -n 's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' "$unknown_env")
is_valid_bootstrap_admin_password "$unknown_password" \
    || fail "unknown database state did not fail safe with a bootstrap credential"

timescale_env="$TEST_ROOT/timescale/.env"
mkdir -p "$(dirname "$timescale_env")"
printf 'JWT_SECRET_KEY=%064d\n' 0 > "$timescale_env"
timescale_output=$(ensure_compose_timescaledb_password "$timescale_env")
timescale_password=$(sed -n 's/^TIMESCALEDB_PASSWORD=//p' "$timescale_env")
[[ ${#timescale_password} -ge 32 ]] \
    || fail "TimescaleDB extension password was not generated strongly"
[[ "$timescale_output" != *"$timescale_password"* ]] \
    || fail "Docker installer printed the TimescaleDB password"
[[ "$(file_mode "$timescale_env")" == 600 ]] \
    || fail "TimescaleDB credential file is not mode 0600"

if command -v sqlite3 >/dev/null 2>&1; then
    partial_env="$TEST_ROOT/partial/.env"
    partial_db="$TEST_ROOT/partial/aether.db"
    mkdir -p "$(dirname "$partial_env")"
    printf 'JWT_SECRET_KEY=%064d\n' 0 > "$partial_env"
    sqlite3 "$partial_db" 'CREATE TABLE migration_marker (version INTEGER);'
    ensure_compose_bootstrap_admin "$partial_env" "$partial_db"
    partial_password=$(sed -n 's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' "$partial_env")
    is_valid_bootstrap_admin_password "$partial_password" \
        || fail "schema-only partial install did not receive a bootstrap credential"

    existing_users_env="$TEST_ROOT/existing-users/.env"
    existing_users_db="$TEST_ROOT/existing-users/aether.db"
    mkdir -p "$(dirname "$existing_users_env")"
    printf 'JWT_SECRET_KEY=%064d\n' 0 > "$existing_users_env"
    sqlite3 "$existing_users_db" \
        'CREATE TABLE users (id INTEGER PRIMARY KEY); INSERT INTO users DEFAULT VALUES;'
    ensure_compose_bootstrap_admin "$existing_users_env" "$existing_users_db"
    if grep -q '^AETHER_BOOTSTRAP_ADMIN_PASSWORD=' "$existing_users_env"; then
        fail "database with an existing user was forced to retain a bootstrap credential"
    fi
fi

echo "Testing Docker dependency-image staging remains rollback-safe..."
(
    old_id="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    new_id="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    public_id="$old_id"
    docker_log="$TEST_ROOT/docker-image-order.log"
    : > "$docker_log"
    IMAGE_BACKUP_TAGS=()
    IMAGE_PREVIOUS_IDS=()
    FRESH_IMAGE_TAGS=()
    BACKUP_TIMESTAMP="test"

    docker() {
        case "$1" in
            images)
                [[ -n "$public_id" ]] && printf '%s\n' "${public_id#sha256:}" | cut -c1-12
                ;;
            image)
                if [[ "$2" == inspect && -n "$public_id" ]]; then
                    printf '%s\n' "$public_id"
                fi
                ;;
            tag)
                printf 'tag %s %s\n' "$2" "$3" >> "$docker_log"
                if [[ "$3" == redis:8-alpine ]]; then
                    public_id="$old_id"
                fi
                ;;
            rmi)
                printf 'rmi %s\n' "$2" >> "$docker_log"
                [[ "$2" == redis:8-alpine ]] && public_id=""
                ;;
            *) return 0 ;;
        esac
    }
    get_tarball_image_id() {
        printf '%s\n' "${new_id#sha256:}" | cut -c1-12
    }
    timeout() {
        printf 'load\n' >> "$docker_log"
        public_id="$new_id"
    }

    smart_load_image "$TEST_ROOT/aether-redis.tar.gz"
    backup_line=$(grep -n '^tag sha256:aaaaaaaa' "$docker_log" | cut -d: -f1)
    remove_line=$(grep -n '^rmi redis:8-alpine' "$docker_log" | cut -d: -f1)
    load_line=$(grep -n '^load$' "$docker_log" | cut -d: -f1)
    [[ -n "$backup_line" && "$backup_line" -lt "$remove_line" \
        && "$remove_line" -lt "$load_line" ]] \
        || fail "dependency image was not tagged before public-tag replacement"
    [[ "${IMAGE_PREVIOUS_IDS[redis:8-alpine]}" == "$old_id" ]] \
        || fail "rollback state did not capture the previous dependency image"
    restore_image_tag_from_backup redis:8-alpine
    [[ "$public_id" == "$old_id" ]] \
        || fail "image rollback did not restore the exact dependency image ID"
)

echo "Testing a failed fresh Docker install removes every created footprint..."
(
    image_mutation_log="$TEST_ROOT/aether-image-toctou.log"
    : > "$image_mutation_log"
    get_local_image_id() { printf 'aaaaaaaaaaaa\n'; }
    get_tarball_image_id() { printf 'aaaaaaaaaaaa\n'; }
    docker() { printf '%s\n' "$*" >> "$image_mutation_log"; }
    if smart_load_image "$TEST_ROOT/aetherems.tar.gz"; then
        fail "image staging accepted an Aether tag that appeared after preflight"
    else
        load_status=$?
        [[ "$load_status" == 2 ]] \
            || fail "Aether image TOCTOU conflict returned the wrong status"
    fi
    [[ ! -s "$image_mutation_log" ]] \
        || fail "Aether image TOCTOU conflict mutated Docker state"
)

(
    INSTALL_DIR="$TEST_ROOT/docker-rollback/install"
    DATA_DIR="$TEST_ROOT/docker-rollback/data"
    LOG_DIR="$TEST_ROOT/docker-rollback/logs"
    INSTALL_CONTEXT_FILE="$TEST_ROOT/docker-rollback/etc/install.yaml"
    PROFILE_ENTRY="$TEST_ROOT/docker-rollback/etc/profile.d/aetheredge.sh"
    SHM_PATH="$TEST_ROOT/docker-rollback/shm/aether-rtdb.shm"
    TIMESCALE_DATA_DIR=""
    mkdir -p "$INSTALL_DIR" "$DATA_DIR" "$LOG_DIR" \
        "$(dirname "$INSTALL_CONTEXT_FILE")" \
        "$(dirname "$PROFILE_ENTRY")" "$(dirname "$SHM_PATH")"

    snapshot_runtime_data_for_rollback
    [[ "$RUNTIME_SNAPSHOT_COMPLETE" == true ]] \
        || fail "Docker rollback snapshot did not mark a complete asset set"
    mkdir -p "$DATA_DIR/config" "$DATA_DIR/cert" "$INSTALL_DIR/aux"
    printf 'new database\n' > "$DATA_DIR/aether.db"
    printf 'new config\n' > "$DATA_DIR/config/io.yaml"
    printf 'new certificate\n' > "$DATA_DIR/cert/device.pem"
    printf 'new outbox\n' > "$DATA_DIR/uplink.outbox"
    printf 'new env\n' > "$INSTALL_DIR/.env"
    printf 'new auxiliary file\n' > "$INSTALL_DIR/aux/trace"
    printf 'new log\n' > "$LOG_DIR/io.log"
    printf 'new context\n' > "$INSTALL_CONTEXT_FILE"
    printf 'new profile\n' > "$PROFILE_ENTRY"
    printf 'new shm\n' > "$SHM_PATH"

    restore_runtime_data_from_backup
    [[ -d "$INSTALL_DIR" && -z "$(find "$INSTALL_DIR" -mindepth 1 -print -quit)" ]] \
        || fail "Docker rollback retained files in the fresh install root"
    [[ -d "$DATA_DIR" && -z "$(find "$DATA_DIR" -mindepth 1 -print -quit)" ]] \
        || fail "Docker rollback retained fresh runtime data"
    [[ -d "$LOG_DIR" && -z "$(find "$LOG_DIR" -mindepth 1 -print -quit)" ]] \
        || fail "Docker rollback retained fresh logs"
    [[ ! -e "$INSTALL_CONTEXT_FILE" && ! -e "$PROFILE_ENTRY" \
        && ! -e "$SHM_PATH" ]] \
        || fail "Docker rollback retained context, profile, or SHM footprint"
    rm -rf "$DATABASE_BACKUP_DIR"
)

echo "Testing Docker rollback fails closed on an unsafe restore target..."
(
    INSTALL_DIR="$TEST_ROOT/docker-restore-failure/install"
    DATA_DIR="$TEST_ROOT/docker-restore-failure/data"
    INSTALL_CONTEXT_FILE="$TEST_ROOT/docker-restore-failure/etc/install.yaml"
    TIMESCALE_DATA_DIR=""
    DATABASE_BACKUP_DIR="$TEST_ROOT/docker-restore-failure/backup"
    RUNTIME_SNAPSHOT_COMPLETE=true
    mkdir -p "$INSTALL_DIR" "$DATA_DIR/aether.db" \
        "$(dirname "$INSTALL_CONTEXT_FILE")" "$DATABASE_BACKUP_DIR"
    touch "$DATABASE_BACKUP_DIR/data-aether.db.expected-file"

    if restore_runtime_data_from_backup; then
        fail "Docker rollback reported success after refusing an unsafe target"
    fi
    [[ -d "$DATABASE_BACKUP_DIR" && -d "$DATA_DIR/aether.db" ]] \
        || fail "failed Docker rollback discarded evidence needed for recovery"
)

echo "Testing Docker quiesce and health checks cover transitional states..."
(
    mock_state=restarting
    mock_health=starting
    known_aether_containers() {
        printf 'aether-io\n'
    }
    docker() {
        case "$1" in
            inspect)
                case "$*" in
                    *'.State.Status'*) printf '%s\n' "$mock_state" ;;
                    *'.State.Health'*) printf '%s\n' "$mock_health" ;;
                    *) return 1 ;;
                esac
                ;;
            unpause)
                mock_state=running
                ;;
            stop)
                mock_state=exited
                ;;
            *) return 0 ;;
        esac
    }

    quiesce_docker_services
    [[ "$mock_state" == exited ]] \
        || fail "Docker quiesce ignored a restarting container"

    mock_state=paused
    quiesce_docker_services
    [[ "$mock_state" == exited ]] \
        || fail "Docker quiesce ignored a paused container"

    mock_state=running
    mock_health=unhealthy
    if all_containers_ready aether-io; then
        fail "Docker commit gate accepted an unhealthy container"
    fi
    mock_health=healthy
    all_containers_ready aether-io \
        || fail "Docker commit gate rejected a running healthy container"
)

echo "Testing fixed composition roots and fresh-install preflight..."
validate_docker_install_dir /opt/AetherEdge
if validate_docker_install_dir /srv/aether; then
    fail "Docker installer accepted a root incompatible with packaged mounts"
fi
INSTALL_DIR="$TEST_ROOT/layout-install"
docker_root_guard_line=$(grep -nFx 'validate_docker_install_dir "$INSTALL_DIR"' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
docker_mutation_line=$(grep -nF 'Installing CLI tools' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
[[ "$docker_root_guard_line" -lt "$docker_mutation_line" ]] \
    || fail "Docker install root must be rejected before host mutation"
if AETHER_BASE_PATH=/ resolve_compose_data_directory >/dev/null; then
    fail "Docker installer accepted / as a recursive data/permission root"
fi
if AETHER_BASE_PATH=/var resolve_compose_data_directory >/dev/null; then
    fail "Docker installer accepted a system directory as its data root"
fi
for unsafe_root in // /// //// //var ///etc /var/// /etc//; do
    if AETHER_BASE_PATH="$unsafe_root" resolve_compose_data_directory >/dev/null; then
        fail "Docker installer accepted redundant-slash system root: $unsafe_root"
    fi
done
normalized_data_root=$(AETHER_BASE_PATH=/srv//aether-data/// resolve_compose_data_directory)
[[ "$normalized_data_root" == /srv/aether-data ]] \
    || fail "safe redundant-slash path was not normalized before persistence"
for dotenv_unsafe_path in '/tmp/aether #site' '/tmp/aether$site' '/tmp/aether site' '/tmp/aether:evil'; do
    if AETHER_BASE_PATH="$dotenv_unsafe_path" resolve_compose_data_directory >/dev/null; then
        fail "Docker installer accepted Compose-unsafe data path: $dotenv_unsafe_path"
    fi
done
symlink_root="$TEST_ROOT/data-link"
safe_but_external_target="$TEST_ROOT/safe-but-external-target"
mkdir -p "$safe_but_external_target"
ln -s "$safe_but_external_target" "$symlink_root"
if AETHER_BASE_PATH="$symlink_root/aether" resolve_compose_data_directory >/dev/null; then
    fail "Docker installer accepted a symlinked data-path ancestor with a safe-looking target"
fi
canonical_test_root=$(realpath "$TEST_ROOT")
external_root="$canonical_test_root/extp"
mkdir -p "$external_root"
is_safe_external_storage_root "$external_root" \
    || fail "ordinary external-storage directory was rejected"
external_link="$canonical_test_root/extp-link"
ln -s / "$external_link"
if is_safe_external_storage_root "$external_link"; then
    fail "symlinked external-storage root was accepted"
fi
ln -s "$safe_but_external_target" "$external_root/logs"
if validate_compose_data_directory "$external_root/logs"; then
    fail "symlinked external log directory was accepted"
fi
rm "$external_root/logs"
ln -s "$safe_but_external_target" "$external_root/timescaledb"
if validate_compose_data_directory "$external_root/timescaledb/data"; then
    fail "symlinked TimescaleDB storage ancestor was accepted"
fi
rm "$external_root/timescaledb"

fresh_data_root="$TEST_ROOT/fresh-data"
mkdir -p "$fresh_data_root"
require_empty_or_absent_directory "$fresh_data_root" "test data root"
printf 'runtime state\n' > "$fresh_data_root/aether.db"
if require_empty_or_absent_directory "$fresh_data_root" "test data root"; then
    fail "fresh Docker preflight accepted an existing runtime database"
fi

fresh_install_root="$TEST_ROOT/fresh-install-root"
mkdir -p "$fresh_install_root"
require_empty_or_absent_install_root "$fresh_install_root"
printf 'runtime state\n' > "$fresh_install_root/docker-compose.yml"
if require_empty_or_absent_install_root "$fresh_install_root"; then
    fail "fresh Docker preflight accepted existing install-root content"
fi

standalone_cli="$TEST_ROOT/standalone-aether"
printf '#!/bin/sh\nexit 0\n' > "$standalone_cli"
chmod +x "$standalone_cli"
validate_standalone_cli_for_fresh_install "$standalone_cli" \
    || fail "CLI-only precursor was rejected"
chmod -x "$standalone_cli"
if validate_standalone_cli_for_fresh_install "$standalone_cli"; then
    fail "unsafe non-executable standalone CLI was accepted"
fi

(
    existing_container=""
    packaged_image_loaded=false
    docker() {
        if [[ "$1" == inspect && "$2" == "$existing_container" \
            && -n "$existing_container" ]]; then
            return 0
        fi
        if [[ "$1" == image && "$2" == inspect \
            && "$packaged_image_loaded" == true ]]; then
            return 0
        fi
        return 1
    }
    reject_existing_docker_runtime_footprint
    existing_container=aether-io
    if reject_existing_docker_runtime_footprint; then
        fail "fresh Docker preflight accepted an existing canonical container"
    fi
    existing_container=""
    packaged_image_loaded=true
    reject_existing_docker_container_footprint \
        || fail "final Docker recheck rejected the image loaded by this package"
    if reject_existing_docker_runtime_footprint; then
        fail "initial Docker preflight accepted a pre-existing Aether image"
    fi
)

(
    REDIS_EXTENSION_SELECTED=false
    existing_volume=""
    transaction_volumes_exist=false
    removed_volumes="$TEST_ROOT/removed-redis-volumes.log"
    : > "$removed_volumes"
    docker() {
        if [[ "$1" == volume && "$2" == inspect ]]; then
            [[ "$3" == "$existing_volume" && -n "$existing_volume" ]] \
                || [[ "$transaction_volumes_exist" == true ]]
            return
        fi
        if [[ "$1" == volume && "$2" == rm ]]; then
            printf '%s\n' "$3" >> "$removed_volumes"
            return 0
        fi
        return 1
    }
    reject_existing_redis_volume_footprint
    existing_volume=aetheredge_redis-data
    if reject_existing_redis_volume_footprint; then
        fail "core-only fresh preflight accepted a stale Aether Redis volume"
    fi
    existing_volume=""
    REDIS_EXTENSION_SELECTED=true
    transaction_volumes_exist=true
    remove_fresh_redis_volumes
    [[ "$(wc -l < "$removed_volumes" | tr -d ' ')" == 2 ]] \
        || fail "fresh rollback did not remove both transaction-created Redis volumes"
)

(
    compose_log="$TEST_ROOT/compose-project.log"
    : > "$compose_log"
    docker() {
        if [[ "$1" == compose && "$2" == version ]]; then
            return 0
        fi
        printf '%s\n' "$*" >> "$compose_log"
    }
    COMPOSE_PROJECT_NAME=foreign-project run_docker_compose config
    assert_contains "$compose_log" 'compose --project-name aetheredge config'
)

docker_filesystem_preflight_line=$(grep -nFx 'reject_existing_docker_filesystem_footprint' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
docker_runtime_preflight_line=$(grep -nFx 'reject_existing_docker_runtime_footprint' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
docker_runtime_recheck_line=$(grep -nFx 'reject_existing_docker_runtime_footprint' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
docker_container_recheck_line=$(grep -nFx 'reject_existing_docker_container_footprint' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
docker_transaction_line=$(grep -nFx 'INSTALL_TRANSACTION_ACTIVE=true' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
docker_container_mutation_line=$(grep -nFx 'CONTAINER_STATE_MUTATED=true' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$docker_filesystem_preflight_line" -lt "$docker_transaction_line" \
    && "$docker_runtime_preflight_line" -lt "$docker_transaction_line" \
    && "$docker_runtime_recheck_line" -lt "$docker_container_mutation_line" \
    && "$docker_container_recheck_line" -lt "$docker_container_mutation_line" ]] \
    || fail "fresh Docker footprint checks must run before stop/mutation"

install_user=$(id -un)
[[ "$(resolve_install_user 0 "" "$install_user" root)" == "$install_user" ]] \
    || fail "sudo installer did not resolve the invoking non-root user"
if resolve_install_user 0 "" root root >/dev/null; then
    fail "direct root install silently selected UID 0 for public containers"
fi
[[ "$(resolve_install_user 0 "$install_user" root root)" == "$install_user" ]] \
    || fail "explicit non-root install identity was rejected"
unset AETHER_BASE_PATH

echo "Testing a new distribution template cannot retain stale unsafe files..."
packaged_template_dir="$TEST_ROOT/package/config.template"
staged_template_dir="$TEST_ROOT/install/config.template"
mkdir -p "$packaged_template_dir/io" "$staged_template_dir/automation/rules"
printf 'channels: []\n' > "$packaged_template_dir/io/io.yaml"
printf '{"enabled": true}\n' > "$staged_template_dir/automation/rules/stale-control.json"
stage_distribution_template "$packaged_template_dir" "$staged_template_dir"
cmp "$packaged_template_dir/io/io.yaml" "$staged_template_dir/io/io.yaml"
[[ ! -e "$staged_template_dir/automation/rules/stale-control.json" ]] \
    || fail "stale distribution control rule survived template staging"

echo "Testing atomic activation of the safe configuration..."
template_dir="$TEST_ROOT/config.template"
live_config_dir="$TEST_ROOT/data/config"
mkdir -p "$template_dir/io" "$template_dir/automation"
printf 'api: {}\n' > "$template_dir/global.yaml"
printf 'channels: []\n' > "$template_dir/io/io.yaml"
printf 'auto_load_instances: false\n' > "$template_dir/automation/automation.yaml"
activate_initial_config "$template_dir" "$live_config_dir"
cmp "$template_dir/global.yaml" "$live_config_dir/global.yaml"
cmp "$template_dir/io/io.yaml" "$live_config_dir/io/io.yaml"

echo "Testing an empty bind-mount directory is treated as first install..."
empty_live_config_dir="$TEST_ROOT/empty-live/config"
mkdir -p "$empty_live_config_dir"
activate_initial_config "$template_dir" "$empty_live_config_dir"
cmp "$template_dir/global.yaml" "$empty_live_config_dir/global.yaml"

echo "Testing fresh activation rejects an existing configuration..."
existing_live_config_dir="$TEST_ROOT/existing/config"
mkdir -p "$existing_live_config_dir"
printf 'operator-owned\n' > "$existing_live_config_dir/site-marker"
if activate_initial_config "$template_dir" "$existing_live_config_dir"; then
    fail "fresh activation accepted an existing site configuration"
fi
[[ "$(cat "$existing_live_config_dir/site-marker")" == operator-owned ]] \
    || fail "failed fresh preflight overwrote existing site configuration"
[[ ! -e "$existing_live_config_dir/global.yaml" ]] \
    || fail "failed fresh preflight merged distribution defaults"

echo "Testing install context is create-only..."
INSTALL_CONTEXT_FILE="$TEST_ROOT/etc/aether/install.yaml"
persist_install_context \
    docker-compose \
    "$live_config_dir" \
    "$TEST_ROOT/data" \
    /run/aether
assert_contains "$INSTALL_CONTEXT_FILE" 'channel: stable'
assert_contains "$INSTALL_CONTEXT_FILE" 'packs: []'

context_before=$(cat "$INSTALL_CONTEXT_FILE")
if persist_install_context \
    docker-compose \
    "$TEST_ROOT/replacement/config" \
    "$TEST_ROOT/replacement/data" \
    /run/aether; then
    fail "fresh installer accepted an existing install context"
fi
[[ "$(cat "$INSTALL_CONTEXT_FILE")" == "$context_before" ]] \
    || fail "rejected install context was modified"

echo "Testing relative installed paths fail closed..."
relative_context="$TEST_ROOT/relative/install.yaml"
INSTALL_CONTEXT_FILE="$relative_context"
if persist_install_context docker-compose relative/config relative/data /run/aether; then
    fail "relative installed paths were accepted"
fi
[[ ! -e "$relative_context" ]] \
    || fail "invalid install context was partially written"

echo "Testing fresh database initialization uses explicit installed paths..."
assert_not_contains "$DOCKER_INSTALLER" 'migrate_points_tables'
assert_not_contains "$DOCKER_INSTALLER" 'restore_migrated_data'
assert_not_contains "$DOCKER_INSTALLER" '_backup AS SELECT'
init_count=$(grep -Ec '^[[:space:]]*(/usr/local/bin/)?aether([[:space:]]|$).*init([[:space:]]|$)' "$DOCKER_INSTALLER" || true)
explicit_init_count=$(grep -Ec '^[[:space:]]*(/usr/local/bin/)?aether --config-path "\$LIVE_CONFIG_DIR" --db-path "\$DATA_DIR" init([[:space:]]|$)' "$DOCKER_INSTALLER" || true)
[[ "$init_count" -gt 0 && "$init_count" == "$explicit_init_count" ]] \
    || fail "all Docker installer aether init calls must use explicit live paths"

first_context_line=$(awk '/^persist_install_context[[:space:]]/ { print NR; exit }' "$DOCKER_INSTALLER")
first_init_line=$(grep -nE '^[[:space:]]*(/usr/local/bin/)?aether .* init([[:space:]]|$)' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
[[ -n "$first_context_line" && -n "$first_init_line" && "$first_context_line" -lt "$first_init_line" ]] \
    || fail "install context must be persisted before the first database migration"
fresh_runtime_guard_line=$(grep -nFx 'reject_existing_docker_runtime_footprint' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
container_mutation_line=$(grep -nFx 'CONTAINER_STATE_MUTATED=true' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
[[ -n "$fresh_runtime_guard_line" \
    && "$fresh_runtime_guard_line" -lt "$first_init_line" \
    && "$first_init_line" -lt "$container_mutation_line" ]] \
    || fail "fresh runtime guard/init/container ordering is unsafe"

echo "Testing bare-metal fresh-install rollback and guards..."
assert_not_contains "$BARE_METAL_INSTALLER" 'Existing install context'
assert_contains "$BARE_METAL_INSTALLER" 'restore_previous_binaries'
assert_contains "$BARE_METAL_INSTALLER" 'INSTALL_COMPLETED=true'
swap_guard_line=$(grep -nF 'BINARIES_SWAPPED=true' "$BARE_METAL_INSTALLER" | head -1 | cut -d: -f1)
binary_publish_line=$(grep -nF 'mv "$BINARY_STAGE" "$INSTALL_DIR/bin"' "$BARE_METAL_INSTALLER" | head -1 | cut -d: -f1)
[[ "$swap_guard_line" -lt "$binary_publish_line" ]] \
    || fail "binary rollback must be armed before publishing the staged directory"
assert_contains "$BARE_METAL_INSTALLER" 'ensure_env_setting "$CONFIG_DIR/aether.env" "AETHER_CONFIG_PATH" "$CONFIG_DIR/config"'
assert_contains "$BARE_METAL_INSTALLER" 'ensure_env_setting "$CONFIG_DIR/aether.env" "AETHER_DATA_PATH" "$DATA_DIR"'
assert_contains "$BARE_METAL_INSTALLER" 'AETHER_BARE_METAL_INSTALLER_FUNCTIONS_ONLY'
(
    # shellcheck disable=SC1090
    AETHER_BARE_METAL_INSTALLER_FUNCTIONS_ONLY=true source "$BARE_METAL_INSTALLER"
    validate_bare_metal_install_dir /opt/aether
    if validate_bare_metal_install_dir /srv/aether; then
        fail "bare-metal installer accepted a layout incompatible with packaged systemd units"
    fi

    INSTALL_DIR="$TEST_ROOT/bare-fresh/install"
    CONFIG_DIR="$TEST_ROOT/bare-fresh/config"
    DATA_DIR="$TEST_ROOT/bare-fresh/data"
    SYSTEMD_DIR="$TEST_ROOT/bare-fresh/systemd"
    DOCKER_INSTALL_ROOT="$TEST_ROOT/bare-fresh/docker-install"
    DOCKER_PROFILE_ENTRY="$TEST_ROOT/bare-fresh/docker-profile"
    SHM_PATH="$TEST_ROOT/bare-fresh/aether-rtdb.shm"
    mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$DATA_DIR" "$SYSTEMD_DIR"
    active_unit=""
    systemctl() {
        [[ "$1" == is-active && "$3" == "$active_unit" && -n "$active_unit" ]]
    }
    docker() { return 1; }
    reject_existing_bare_metal_footprint \
        || fail "bare-metal fresh preflight rejected empty provisioning roots"
    printf 'runtime state\n' > "$DATA_DIR/aether.db"
    if reject_existing_bare_metal_footprint; then
        fail "bare-metal fresh preflight accepted an existing database"
    fi
    rm "$DATA_DIR/aether.db"
    active_unit=aether-api.service
    if reject_existing_bare_metal_footprint; then
        fail "bare-metal fresh preflight accepted an active Aether service"
    fi
    active_unit=""
    touch "$SYSTEMD_DIR/aether.target"
    if reject_existing_bare_metal_footprint; then
        fail "bare-metal fresh preflight accepted an existing systemd unit"
    fi
    rm "$SYSTEMD_DIR/aether.target"

    generated_bootstrap=$(generate_bootstrap_admin_password)
    is_valid_bootstrap_admin_password "$generated_bootstrap" \
        || fail "bare-metal installer generated a weak bootstrap password"
    bare_bootstrap_env="$TEST_ROOT/bare-bootstrap/aether.env"
    mkdir -p "$(dirname "$bare_bootstrap_env")"
    printf 'JWT_SECRET_KEY=%064d\n' 0 > "$bare_bootstrap_env"
    bare_bootstrap_log="$TEST_ROOT/bare-bootstrap/install.log"
    ensure_bare_metal_bootstrap_admin \
        "$bare_bootstrap_env" \
        "$TEST_ROOT/bare-bootstrap/aether.db" > "$bare_bootstrap_log"
    bare_bootstrap_password=$(sed -n \
        's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' "$bare_bootstrap_env")
    is_valid_bootstrap_admin_password "$bare_bootstrap_password" \
        || fail "bare-metal first install did not persist a strong bootstrap password"
    [[ "$(file_mode "$bare_bootstrap_env")" == 600 ]] \
        || fail "bare-metal bootstrap credential file is not mode 0600"
    ! grep -Fq "$bare_bootstrap_password" "$bare_bootstrap_log" \
        || fail "bare-metal installer printed the bootstrap password"
    assert_contains "$bare_bootstrap_env" 'AETHER_ALLOW_PUBLIC_REGISTRATION=false'

    core_bundle="$TEST_ROOT/bare-core-bundle"
    mkdir -p "$core_bundle"
    [[ "$(detect_bundled_frontend "$core_bundle")" == false ]] \
        || fail "a core-only bare-metal bundle was treated as frontend-enabled"

    frontend_bundle="$TEST_ROOT/bare-frontend-bundle"
    mkdir -p "$frontend_bundle/bin" "$frontend_bundle/apps-dist" "$frontend_bundle/systemd"
    touch "$frontend_bundle/bin/nginx" \
        "$frontend_bundle/apps-dist/index.html" \
        "$frontend_bundle/nginx.conf" \
        "$frontend_bundle/systemd/aether-apps.service"
    chmod +x "$frontend_bundle/bin/nginx"
    [[ "$(detect_bundled_frontend "$frontend_bundle")" == true ]] \
        || fail "a complete bare-metal frontend bundle was not detected"

    partial_frontend_bundle="$TEST_ROOT/bare-partial-frontend-bundle"
    mkdir -p "$partial_frontend_bundle/bin"
    touch "$partial_frontend_bundle/bin/nginx"
    chmod +x "$partial_frontend_bundle/bin/nginx"
    if detect_bundled_frontend "$partial_frontend_bundle" >/dev/null; then
        fail "an incomplete bare-metal frontend bundle was accepted"
    fi

    empty_frontend_bundle="$TEST_ROOT/bare-empty-frontend-bundle"
    mkdir -p "$empty_frontend_bundle/apps-dist"
    if detect_bundled_frontend "$empty_frontend_bundle" >/dev/null; then
        fail "an empty apps-dist trace was treated as a core-only bundle"
    fi

    non_executable_frontend_bundle="$TEST_ROOT/bare-non-executable-frontend-bundle"
    mkdir -p "$non_executable_frontend_bundle/bin"
    touch "$non_executable_frontend_bundle/bin/nginx"
    if detect_bundled_frontend "$non_executable_frontend_bundle" >/dev/null; then
        fail "a non-executable bundled nginx was treated as a core-only bundle"
    fi

    redis_bundle="$TEST_ROOT/bare-redis-bundle"
    mkdir -p "$redis_bundle/bin" "$redis_bundle/systemd"
    touch "$redis_bundle/bin/redis-server" \
        "$redis_bundle/bin/redis-cli" \
        "$redis_bundle/systemd/aether-redis.service"
    chmod +x "$redis_bundle/bin/redis-server" "$redis_bundle/bin/redis-cli"
    [[ "$(detect_bundled_redis "$redis_bundle")" == true ]] \
        || fail "a complete bare-metal Redis extension bundle was not detected"

    partial_redis_bundle="$TEST_ROOT/bare-partial-redis-bundle"
    mkdir -p "$partial_redis_bundle/bin"
    touch "$partial_redis_bundle/bin/redis-server"
    chmod +x "$partial_redis_bundle/bin/redis-server"
    if detect_bundled_redis "$partial_redis_bundle" >/dev/null; then
        fail "an incomplete bare-metal Redis extension bundle was accepted"
    fi

    non_executable_redis_bundle="$TEST_ROOT/bare-non-executable-redis-bundle"
    mkdir -p "$non_executable_redis_bundle/bin"
    touch "$non_executable_redis_bundle/bin/redis-server"
    if detect_bundled_redis "$non_executable_redis_bundle" >/dev/null; then
        fail "a non-executable Redis artifact was treated as a core-only bundle"
    fi

    symlink_frontend_bundle="$TEST_ROOT/bare-symlink-frontend-bundle"
    mkdir -p "$symlink_frontend_bundle/bin" \
        "$symlink_frontend_bundle/apps-dist" \
        "$symlink_frontend_bundle/systemd" \
        "$symlink_frontend_bundle/external"
    touch "$symlink_frontend_bundle/external/nginx" \
        "$symlink_frontend_bundle/apps-dist/index.html" \
        "$symlink_frontend_bundle/nginx.conf" \
        "$symlink_frontend_bundle/systemd/aether-apps.service"
    chmod +x "$symlink_frontend_bundle/external/nginx"
    ln -s "$symlink_frontend_bundle/external/nginx" \
        "$symlink_frontend_bundle/bin/nginx"
    if detect_bundled_frontend "$symlink_frontend_bundle" >/dev/null; then
        fail "a symlinked frontend binary was accepted as self-contained"
    fi

    nested_frontend_assets="$TEST_ROOT/bare-nested-frontend-assets"
    mkdir -p "$nested_frontend_assets/assets"
    touch "$nested_frontend_assets/index.html"
    ln -s / "$nested_frontend_assets/assets/outside"
    if validate_tree_without_links_or_special_files \
        "$nested_frontend_assets" "frontend fixture"; then
        fail "a nested frontend asset symlink was accepted"
    fi

    web_root_fixture="$TEST_ROOT/bare-web-root"
    mkdir -p "$web_root_fixture/unowned"
    printf 'unrelated\n' > "$web_root_fixture/unowned/index.html"
    if validate_frontend_web_root \
        "$web_root_fixture/unowned" "$web_root_fixture/marker" false; then
        fail "bare-metal frontend accepted an unowned non-empty web root"
    fi
    touch "$web_root_fixture/marker"
    validate_frontend_web_root \
        "$web_root_fixture/unowned" "$web_root_fixture/marker" false
    ln -s / "$web_root_fixture/symlink"
    if validate_frontend_web_root \
        "$web_root_fixture/symlink" "$web_root_fixture/marker" true; then
        fail "bare-metal frontend accepted a symlinked destructive web root"
    fi

    (
        mock_active=true
        mock_stop_calls=0
        systemctl() {
            case "$1" in
                is-active)
                    [[ "$mock_active" == true ]]
                    ;;
                stop)
                    mock_stop_calls=$((mock_stop_calls + 1))
                    mock_active=false
                    ;;
                *)
                    return 0
                    ;;
            esac
        }
        quiesce_aether_services_for_rollback
        [[ "$mock_stop_calls" -ge 1 ]] \
            || fail "rollback quiesce did not stop a partially started service"
    )
    (
        systemctl() {
            case "$1" in
                is-active) return 0 ;;
                stop) return 1 ;;
                *) return 0 ;;
            esac
        }
        if quiesce_aether_services_for_rollback; then
            fail "rollback accepted services that could not be quiesced"
        fi
    )

    state_fixture="$TEST_ROOT/bare-state-snapshot"
    STATE_BACKUP_DIR="$state_fixture/backup"
    mkdir -p "$STATE_BACKUP_DIR" "$state_fixture/live"
    printf 'old state\n' > "$state_fixture/live/config"
    snapshot_bare_metal_path "$state_fixture/live/config" config-file
    printf 'new state\n' > "$state_fixture/live/config"
    restore_bare_metal_path "$state_fixture/live/config" config-file
    [[ "$(cat "$state_fixture/live/config")" == 'old state' ]] \
        || fail "bare-metal asset snapshot did not restore prior content"
    snapshot_bare_metal_path "$state_fixture/live/absent" absent-file
    printf 'new file\n' > "$state_fixture/live/absent"
    restore_bare_metal_path "$state_fixture/live/absent" absent-file
    [[ ! -e "$state_fixture/live/absent" ]] \
        || fail "bare-metal rollback retained an asset absent before install"
)
bare_root_guard_line=$(grep -nFx 'validate_bare_metal_install_dir "$INSTALL_DIR"' "$BARE_METAL_INSTALLER" | head -1 | cut -d: -f1)
bare_preflight_line=$(grep -nFx 'reject_existing_bare_metal_footprint' "$BARE_METAL_INSTALLER" | head -1 | cut -d: -f1)
bare_preflight_recheck_line=$(grep -nFx 'reject_existing_bare_metal_footprint' "$BARE_METAL_INSTALLER" | tail -1 | cut -d: -f1)
bare_mutation_line=$(grep -nFx 'normalize_root_owned_tree .' "$BARE_METAL_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$bare_root_guard_line" -lt "$bare_preflight_line" \
    && "$bare_preflight_line" -lt "$bare_mutation_line" ]] \
    || fail "bare-metal footprint preflight must run before host mutation"
frontend_detection_line=$(grep -nF 'FRONTEND_INCLUDED=$(detect_bundled_frontend .)' "$BARE_METAL_INSTALLER" | head -1 | cut -d: -f1)
[[ "$frontend_detection_line" -lt "$bare_preflight_line" ]] \
    || fail "bare-metal optional artifacts must be validated before footprint preflight"
redis_detection_line=$(grep -nF 'REDIS_INCLUDED=$(detect_bundled_redis .)' "$BARE_METAL_INSTALLER" | head -1 | cut -d: -f1)
[[ "$redis_detection_line" -lt "$bare_preflight_line" ]] \
    || fail "bare-metal Redis artifacts must be validated before footprint preflight"
assert_contains "$BARE_METAL_INSTALLER" 'validate_secure_regular_file_if_exists'
state_snapshot_line=$(grep -nFx 'snapshot_bare_metal_state' "$BARE_METAL_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$bare_preflight_recheck_line" -lt "$state_snapshot_line" \
    && "$state_snapshot_line" -lt "$binary_publish_line" ]] \
    || fail "bare-metal host-state snapshot is taken after binary publication"

echo "Testing optional static dependency builds require trusted provenance..."
if INCLUDE_REDIS=1 INCLUDE_NGINX=0 REDIS_VERSION=9.9.9 \
    REDIS_SHA256=untrusted bash "$STATIC_DEP_BUILDER" arm64 >/dev/null 2>&1; then
    fail "static Redis build accepted an invalid source digest"
fi
assert_contains "$STATIC_DEP_BUILDER" '.source-sha256'
assert_contains "$STATIC_DEP_BUILDER" 'validate_static_elf'

echo "Testing optional PostgreSQL storage stays opt-in during installation..."
timescale_guard_line=$(grep -nF 'if [[ -f "docker/aether-timescaledb.tar.gz" ]]' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
timescale_directory_line=$(grep -nF 'TIMESCALE_DATA_DIR=$(resolve_compose_timescale_data_directory)' "$DOCKER_INSTALLER" | head -1 | cut -d: -f1)
[[ -n "$timescale_guard_line" && -n "$timescale_directory_line" \
    && "$timescale_guard_line" -lt "$timescale_directory_line" ]] \
    || fail "TimescaleDB data directory creation must be guarded by the selected extension artifact"
assert_not_contains "$DOCKER_INSTALLER" '${AETHER_DATA_PATH:-/extp}/timescaledb/data'
assert_contains "$ROOT_DIR/docker-compose.yml" '${AETHER_TIMESCALE_DATA_PATH:-./data/timescaledb/data}'
assert_not_contains "$ROOT_DIR/docker-compose.yml" 'TIMESCALEDB_PASSWORD:-postgres'
assert_contains "$ROOT_DIR/docker-compose.yml" 'POSTGRES_PASSWORD=${TIMESCALEDB_PASSWORD:-}'
assert_contains "$ROOT_DIR/docker-compose.yml" 'listen_addresses=127.0.0.1'
assert_contains "$ROOT_DIR/docker-compose.yml" '"--bind", "127.0.0.1"'
[[ "$(grep -Fc 'AETHER_UPLINK_CONTROL_TOKEN=${AETHER_UPLINK_CONTROL_TOKEN:?' "$ROOT_DIR/docker-compose.yml")" == 2 ]] \
    || fail "uplink control credential must be injected only into automation and uplink"
assert_contains "$BARE_METAL_INSTALLER" 'AETHER_UPLINK_CONTROL_TOKEN=$UPLINK_CONTROL_TOKEN'

echo "Testing internal APIs keep their commissioned network and credential boundaries..."
assert_contains "$ROOT_DIR/docker-compose.yml" 'command: ["aether-io", "--bind-address", "127.0.0.1:6001"]'
io_compose_service=$(sed -n '/^  aether-io:/,/^  aether-automation:/p' "$ROOT_DIR/docker-compose.yml")
if [[ "$io_compose_service" != *'JWT_SECRET_KEY=${JWT_SECRET_KEY:?'* ]]; then
    fail "aether-io Compose service must receive the shared access-JWT verification secret"
fi
internal_loopback_count=$(grep -Fc -- '- API_HOST=127.0.0.1' "$ROOT_DIR/docker-compose.yml")
[[ "$internal_loopback_count" == 4 ]] \
    || fail "the four internal service APIs must bind to loopback"
assert_contains "$ROOT_DIR/docker-compose.yml" 'API_HOST=${AETHER_API_HOST:-0.0.0.0}'
processor_compose_section=$(awk '
    /^  aether-load-forecasting-processor:/ { in_processor = 1 }
    in_processor && /^  [a-zA-Z0-9_-]+:/ && !/^  aether-load-forecasting-processor:/ { exit }
    in_processor { print }
' "$ROOT_DIR/docker-compose.yml")
grep -Fq -- 'profiles: ["data-processing-dev"]' <<< "$processor_compose_section" \
    || fail "the mutable data processor image must remain development-only"
grep -Fq -- '127.0.0.1:${AETHER_LOAD_FORECASTING_PORT:-8989}:8989' \
    <<< "$processor_compose_section" \
    || fail "the bridged data processor must publish only on host loopback"
grep -Fq -- 'API_HOST=0.0.0.0' <<< "$processor_compose_section" \
    || fail "the bridged data processor must listen on its container interface"
grep -Fq -- 'data-processing-local' <<< "$processor_compose_section" \
    || fail "the optional data processor must use its dedicated internal network"
if grep -Fq -- 'network_mode: host' <<< "$processor_compose_section"; then
    fail "the optional data processor must not share the host network namespace"
fi
assert_contains "$ROOT_DIR/docker-compose.yml" '  data-processing-local:'
assert_contains "$ROOT_DIR/docker-compose.yml" '    internal: true'
assert_contains "$ROOT_DIR/integrations/load-forecasting/deploy/aether-load-forecasting-processor.service" \
    '--host 127.0.0.1 --port 8989'
assert_contains "$ROOT_DIR/scripts/systemd/aether-io.service" 'ExecStart=/opt/aether/bin/aether-io --bind-address 127.0.0.1:6001'
for unit in aether-automation aether-history aether-uplink aether-alarm; do
    assert_contains "$ROOT_DIR/scripts/systemd/${unit}.service" 'Environment=API_HOST=127.0.0.1'
done
apps_compose_section=$(awk '
    /^  apps:/ { in_apps = 1 }
    in_apps && /^volumes:/ { exit }
    in_apps { print }
' "$ROOT_DIR/docker-compose.yml")
grep -Fq -- 'profiles: ["frontend"]' <<< "$apps_compose_section" \
    || fail "optional frontend is still part of the default edge-kernel composition"
assert_not_contains "$ROOT_DIR/scripts/systemd/aether.target" 'aether-apps.service'
assert_contains "$INSTALLER_BUILDER" 'BUILD_IMAGES="aetherems:latest"'
assert_not_contains "$INSTALLER_BUILDER" 'BUILD_IMAGES="aetherems:latest,aether-apps:latest"'
if bash "$INSTALLER_BUILDER" v0-test amd64 --services=redis \
    >/dev/null 2>&1; then
    fail "Docker installer builder accepted an extension-only fresh package"
fi
assert_contains "$INSTALLER_BUILDER" '! csv_contains "$BUILD_IMAGES" "aetherems:latest"'
assert_contains "$INSTALLER_BUILDER" 'csv_contains "$BUILD_IMAGES" "redis:8-alpine"'
assert_contains "$INSTALLER_BUILDER" 'CARGO_FEATURES=""'
assert_contains "$INSTALLER_BUILDER" 'generate "$TARGET" "$RUNTIME_MANIFEST_DIR"'
assert_contains "$INSTALLER_BUILDER" '--bin aether-runtime-manifest -- print-default-features'
assert_contains "$INSTALLER_BUILDER" 'CARGO_FEATURES=$(add_csv_item "$CARGO_FEATURES" "$feature")'
assert_contains "$INSTALLER_BUILDER" 'cp "$RUNTIME_MANIFEST_PATH" "$BM_PKG_DIR/config.template/runtime-manifest.json"'
assert_contains "$INSTALLER_BUILDER" 'cp "$RUNTIME_MANIFEST_PATH" "$BUILD_DIR/config.template/runtime-manifest.json"'
assert_contains "$ROOT_DIR/Dockerfile" 'COPY ${RUNTIME_MANIFEST_PATH} /app/config/runtime-manifest.json'
assert_contains "$ROOT_DIR/.dockerignore" '!build/installer/runtime/runtime-manifest.json'
assert_contains "$DOCKER_INSTALLER" './tools/aether --json runtime-manifest'
assert_contains "$BARE_METAL_INSTALLER" 'bin/aether --json runtime-manifest'
timescale_feature_guard_line=$(grep -nF 'if csv_contains "$BUILD_IMAGES" "timescale/timescaledb:2.25.2-pg17"; then' "$INSTALLER_BUILDER" | head -1 | cut -d: -f1)
postgres_feature_line=$(grep -nF '"$CARGO_FEATURES" "aether-history/postgres-storage")' "$INSTALLER_BUILDER" | cut -d: -f1)
[[ -n "$timescale_feature_guard_line" && -n "$postgres_feature_line" \
    && "$timescale_feature_guard_line" -lt "$postgres_feature_line" \
    && "$(grep -Fc 'aether-history/postgres-storage' "$INSTALLER_BUILDER")" == 1 ]] \
    || fail "PostgreSQL history feature is not exclusively guarded by Timescale selection"
assert_contains "$INSTALLER_BUILDER" 'INCLUDE_FRONTEND_STATIC=0'
assert_contains "$INSTALLER_BUILDER" 'INCLUDE_NGINX="$INCLUDE_FRONTEND_STATIC"'
assert_contains "$INSTALLER_BUILDER" 'rm -f "$BM_PKG_DIR/systemd/aether-apps.service"'
assert_contains "$STATIC_DEP_BUILDER" 'INCLUDE_NGINX="${INCLUDE_NGINX:-0}"'
assert_contains "$BARE_METAL_INSTALLER" 'FRONTEND_INCLUDED=$(detect_bundled_frontend .)'
assert_contains "$BARE_METAL_INSTALLER" 'systemctl enable aether-apps.service'
assert_contains "$BARE_METAL_INSTALLER" 'REDIS_INCLUDED=$(detect_bundled_redis .)'
assert_contains "$BARE_METAL_INSTALLER" 'systemctl enable aether-redis.service'
rollback_body=$(awk '
    /^finish_install\(\)/ { in_finish = 1 }
    in_finish && /^if \[\[ "\$\{AETHER_BARE_METAL_INSTALLER_FUNCTIONS_ONLY/ { exit }
    in_finish { print }
' "$BARE_METAL_INSTALLER")
if grep -Fq '_INCLUDED' <<< "$rollback_body"; then
    fail "bare-metal rollback derives previous extension state from the incoming bundle"
fi
grep -Fq 'for unit in aether-apps.service aether-redis.service aether.target; do' \
    <<< "$rollback_body" \
    || fail "bare-metal rollback does not remove newly enabled service state"
rollback_quiesce_line=$(grep -nF 'quiesce_aether_services_for_rollback' <<< "$rollback_body" | head -1 | cut -d: -f1)
rollback_restore_line=$(grep -nF 'restore_previous_binaries' <<< "$rollback_body" | head -1 | cut -d: -f1)
[[ -n "$rollback_quiesce_line" && -n "$rollback_restore_line" \
    && "$rollback_quiesce_line" -lt "$rollback_restore_line" ]] \
    || fail "bare-metal rollback restores binaries before quiescing new processes"
grep -Fq 'restore_bare_metal_state' <<< "$rollback_body" \
    || fail "bare-metal rollback does not restore units/configuration/runtime data"
bare_health_line=$(grep -nF 'wait_for_bare_metal_services 30' "$BARE_METAL_INSTALLER" | tail -1 | cut -d: -f1)
bare_commit_line=$(grep -nF 'INSTALL_COMPLETED=true' "$BARE_METAL_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$bare_health_line" -lt "$bare_commit_line" ]] \
    || fail "bare-metal install commits before every expected service is healthy"

echo "Testing the API container receives its mounted configuration path..."
api_compose_section=$(awk '
    /^  aether-api:/ { in_api = 1 }
    in_api && /^  aether-uplink:/ { exit }
    in_api { print }
' "$ROOT_DIR/docker-compose.yml")
grep -Fq -- 'AETHER_CONFIG_PATH=/app/data/config' <<< "$api_compose_section" \
    || fail "aether-api is missing AETHER_CONFIG_PATH for the mounted safe configuration"
grep -Fq -- 'AETHER_BOOTSTRAP_ADMIN_PASSWORD=${AETHER_BOOTSTRAP_ADMIN_PASSWORD:-}' <<< "$api_compose_section" \
    || fail "aether-api bootstrap password must remain optional for databases that already have users"
grep -Fq -- 'AETHER_ALLOW_PUBLIC_REGISTRATION=${AETHER_ALLOW_PUBLIC_REGISTRATION:-false}' <<< "$api_compose_section" \
    || fail "aether-api public registration is not explicitly deny-by-default"
grep -Fq -- 'user: "${HOST_UID:-1000}:${HOST_GID:-1000}"' <<< "$api_compose_section" \
    || fail "aether-api is not running with the ordinary host user/group"
grep -Fq -- '/etc/systemd/network:/etc/systemd/network:ro' <<< "$api_compose_section" \
    || fail "aether-api network configuration mount is not read-only"
if grep -Eq '/var/run/docker\.sock|/opt/AetherEdge:/opt/AetherEdge' <<< "$api_compose_section"; then
    fail "aether-api still has a privileged Docker or installation-root mount"
fi
assert_not_contains "$DOCKER_INSTALLER" 'chgrp docker /etc/systemd/network'
assert_not_contains "$DOCKER_INSTALLER" 'chmod g+w /etc/systemd/network'
assert_not_contains "$DOCKER_INSTALLER" 'ACTUAL_GID=$DOCKER_GID'
assert_contains "$DOCKER_INSTALLER" 'run_docker_compose up -d --force-recreate'
assert_contains "$DOCKER_INSTALLER" 'docker compose --project-name "$AETHER_COMPOSE_PROJECT"'
assert_contains "$DOCKER_INSTALLER" 'docker-compose --project-name "$AETHER_COMPOSE_PROJECT"'
assert_contains "$DOCKER_INSTALLER" 'trap docker_install_exit EXIT'
assert_contains "$DOCKER_INSTALLER" 'publish_compose_atomically "docker-compose.yml"'
assert_contains "$DOCKER_INSTALLER" 'wait_for_installed_stack "$INSTALL_CONTAINER_LIST" 30'
assert_contains "$DOCKER_INSTALLER" 'INSTALL_TRANSACTION_COMMITTED=true'
assert_not_contains "$DOCKER_INSTALLER" 'docker tag "$image" "$backup_tag"'
assert_not_contains "$DOCKER_INSTALLER" 'migrate_points_tables'
assert_not_contains "$DOCKER_INSTALLER" 'restore_migrated_data'
assert_not_contains "$DOCKER_INSTALLER" 'CREATE TABLE ${table}_backup AS'
assert_not_contains "$DOCKER_INSTALLER" 'if OUTPUT=$(gunzip -c "$tarball" | docker load 2>&1)'
assert_contains "$DOCKER_INSTALLER" 'Refusing $basename: no rollback image mapping'
assert_not_contains "$DOCKER_INSTALLER" 'alpine.tar.gz'
assert_not_contains "$DOCKER_INSTALLER" 'docker image prune -f'
assert_not_contains "$DOCKER_INSTALLER" 'docker images -f "dangling=true" -q'
assert_contains "$DOCKER_INSTALLER" 'docker rm -f "$container"'
assert_contains "$DOCKER_INSTALLER" 'remove_fresh_redis_volumes'
assert_contains "$DOCKER_INSTALLER" 'snapshot_runtime_directory "$INSTALL_DIR" install-root'
assert_contains "$DOCKER_INSTALLER" 'snapshot_runtime_directory "$DATA_DIR" data-root'
snapshot_line=$(grep -nFx 'snapshot_runtime_data_for_rollback' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
layout_recheck_line=$(grep -nFx 'require_empty_or_absent_directory "$DATA_DIR" "Aether data root"' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
secret_mutation_line=$(grep -nFx 'ensure_compose_jwt_secret' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$layout_recheck_line" -lt "$snapshot_line" ]] \
    || fail "data-root assets are not revalidated after writers are stopped"
[[ "$snapshot_line" -lt "$docker_mutation_line" ]] \
    || fail "the installed CLI is replaced before its rollback snapshot"
[[ "$snapshot_line" -lt "$secret_mutation_line" ]] \
    || fail "persistent Compose secrets are changed before rollback state is captured"
compose_publish_line=$(grep -nF 'publish_compose_atomically "docker-compose.yml"' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
stack_commit_line=$(grep -nF 'run_docker_compose up -d --force-recreate' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$compose_publish_line" -lt "$stack_commit_line" ]] \
    || fail "new containers can start before the new Compose file is published"
health_gate_line=$(grep -nF 'wait_for_installed_stack "$INSTALL_CONTAINER_LIST" 30' "$DOCKER_INSTALLER" | cut -d: -f1)
transaction_commit_line=$(grep -nF 'INSTALL_TRANSACTION_COMMITTED=true' "$DOCKER_INSTALLER" | tail -1 | cut -d: -f1)
[[ "$health_gate_line" -lt "$transaction_commit_line" ]] \
    || fail "Docker install commits before the health window passes"

assert_contains "$BARE_METAL_INSTALLER" 'ensure_bare_metal_bootstrap_admin'
assert_contains "$BARE_METAL_INSTALLER" 'AETHER_ALLOW_PUBLIC_REGISTRATION'
assert_contains "$BARE_METAL_INSTALLER" 'Change the bootstrap administrator password immediately'

echo "Testing user-facing Docker paths match the installed layout..."
assert_not_contains "$ROOT_DIR/.env.example" 'AETHER_CONFIG_PATH=/opt/AetherEdge/config'
assert_contains "$ROOT_DIR/docs/AETHER_CLI_GUIDE.md" '/etc/aether/install.yaml'
assert_contains "$ROOT_DIR/docs/AETHER_CLI_GUIDE.md" '/opt/AetherEdge/data/config'
assert_contains "$ROOT_DIR/docs/guides/deployment.md" '`AETHER_INSTALL_DIR` overrides'

echo "Installer layout tests passed."
