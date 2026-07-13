#!/usr/bin/env bash
# Bare-metal AetherIot installer. Packaged by build-installer.sh --bare-metal;
# this script is what makeself runs after extracting the .run archive.
set -Eeuo pipefail
umask 022
cd "$(dirname "${BASH_SOURCE[0]}")"

INSTALL_DIR="${AETHER_INSTALL_DIR:-/opt/aether}"
CONFIG_DIR="/etc/aether"
DATA_DIR="/var/lib/aether"
INSTALL_CONTEXT_FILE="$CONFIG_DIR/install.yaml"
SYSTEMD_DIR="/etc/systemd/system"
DOCKER_INSTALL_ROOT="/opt/AetherEdge"
DOCKER_PROFILE_ENTRY="/etc/profile.d/aetheredge.sh"
SHM_PATH="/dev/shm/aether-rtdb.shm"
INSTALL_COMPLETED=false
BINARIES_SWAPPED=false
BINARY_STAGE=""
BINARY_BACKUP=""
BOOTSTRAP_ADMIN_REQUIRED=false
REDIS_INCLUDED=false
SERVICE_STATE_CAPTURED=false
STATE_BACKUP_DIR=""
STATE_SNAPSHOT_CAPTURED=false

validate_no_symlink_components() {
    local path=$1
    local label=$2
    local cursor=$path

    while [[ "$cursor" != / ]]; do
        if [[ -L "$cursor" ]]; then
            echo "ERROR: $label has a symlinked path component: $cursor" >&2
            return 1
        fi
        cursor=${cursor%/*}
        [[ -n "$cursor" ]] || cursor=/
    done
}

validate_root_owned_not_writable() {
    local path=$1
    local label=$2
    local owner mode

    owner=$(stat -c '%u' -- "$path") || return 1
    mode=$(stat -c '%a' -- "$path") || return 1
    if [[ "$owner" != 0 ]]; then
        echo "ERROR: $label must be owned by root: $path" >&2
        return 1
    fi
    if (( (8#$mode & 0022) != 0 )); then
        echo "ERROR: $label must not be group/other writable: $path" >&2
        return 1
    fi
}

validate_nearest_existing_parent() {
    local path=$1
    local label=$2
    local cursor

    cursor=${path%/*}
    [[ -n "$cursor" ]] || cursor=/
    while [[ ! -e "$cursor" && ! -L "$cursor" ]]; do
        cursor=${cursor%/*}
        [[ -n "$cursor" ]] || cursor=/
    done
    validate_no_symlink_components "$cursor" "$label"
    if [[ ! -d "$cursor" ]]; then
        echo "ERROR: nearest existing parent for $label is not a directory: $cursor" >&2
        return 1
    fi
    validate_root_owned_not_writable "$cursor" "$label parent"
}

validate_secure_directory_if_exists() {
    local path=$1
    local label=$2

    validate_no_symlink_components "$path" "$label"
    if [[ -e "$path" ]]; then
        if [[ ! -d "$path" ]]; then
            echo "ERROR: $label is not a directory: $path" >&2
            return 1
        fi
        validate_root_owned_not_writable "$path" "$label"
    else
        validate_nearest_existing_parent "$path" "$label"
    fi
}

validate_secure_regular_file_if_exists() {
    local path=$1
    local label=$2
    local link_count

    validate_no_symlink_components "$path" "$label"
    if [[ -e "$path" ]]; then
        if [[ ! -f "$path" ]]; then
            echo "ERROR: $label is not a regular file: $path" >&2
            return 1
        fi
        link_count=$(stat -c '%h' -- "$path") || return 1
        if [[ "$link_count" != 1 ]]; then
            echo "ERROR: $label must not be hard-linked to another path: $path" >&2
            return 1
        fi
        validate_root_owned_not_writable "$path" "$label"
    else
        validate_nearest_existing_parent "$path" "$label"
    fi
}

validate_regular_file_if_exists() {
    local path=$1
    local label=$2
    local link_count

    validate_no_symlink_components "$path" "$label"
    if [[ -e "$path" && ! -f "$path" ]]; then
        echo "ERROR: $label is not a regular file: $path" >&2
        return 1
    fi
    if [[ -f "$path" ]]; then
        link_count=$(stat -c '%h' -- "$path") || return 1
        if [[ "$link_count" != 1 ]]; then
            echo "ERROR: $label must not be hard-linked to another path: $path" >&2
            return 1
        fi
    fi
}

validate_tree_without_links_or_special_files() {
    local root=$1
    local label=$2
    local invalid_path

    [[ -d "$root" && ! -L "$root" ]] || {
        echo "ERROR: $label is not a non-symlink directory: $root" >&2
        return 1
    }
    invalid_path=$(find "$root" -type l -print -quit)
    if [[ -n "$invalid_path" ]]; then
        echo "ERROR: $label contains a symlink: $invalid_path" >&2
        return 1
    fi
    invalid_path=$(find "$root" ! -type d ! -type f -print -quit)
    if [[ -n "$invalid_path" ]]; then
        echo "ERROR: $label contains a non-regular entry: $invalid_path" >&2
        return 1
    fi
    invalid_path=$(find "$root" -type f -links +1 -print -quit)
    if [[ -n "$invalid_path" ]]; then
        echo "ERROR: $label contains a multiply linked file: $invalid_path" >&2
        return 1
    fi
}

validate_secure_installed_tree() {
    local root=$1
    local label=$2
    local invalid_path

    validate_tree_without_links_or_special_files "$root" "$label"
    invalid_path=$(find "$root" ! -user root -print -quit)
    if [[ -n "$invalid_path" ]]; then
        echo "ERROR: $label contains an entry not owned by root: $invalid_path" >&2
        return 1
    fi
    invalid_path=$(find "$root" -perm /022 -print -quit)
    if [[ -n "$invalid_path" ]]; then
        echo "ERROR: $label contains a group/other-writable entry: $invalid_path" >&2
        return 1
    fi
}

normalize_root_owned_tree() {
    local root=$1

    chown -R 0:0 -- "$root"
    chmod -R go-w -- "$root"
}

validate_bare_metal_bundle() {
    local bundle_root=$1
    local redis_included=$2
    local binary unit

    for directory in bin systemd config.template script-host; do
        validate_tree_without_links_or_special_files \
            "$bundle_root/$directory" "bundled $directory tree"
    done
    for binary in \
        aether aether-io aether-automation aether-history aether-api \
        aether-uplink aether-alarm; do
        if [[ ! -f "$bundle_root/bin/$binary" \
            || ! -x "$bundle_root/bin/$binary" \
            || -L "$bundle_root/bin/$binary" ]]; then
            echo "ERROR: required bundled binary is missing, non-executable, or unsafe: $binary" >&2
            return 1
        fi
    done
    for unit in \
        aether.target aether-io.service aether-automation.service \
        aether-history.service aether-api.service aether-uplink.service \
        aether-alarm.service; do
        if [[ ! -f "$bundle_root/systemd/$unit" \
            || -L "$bundle_root/systemd/$unit" ]]; then
            echo "ERROR: required bundled systemd unit is missing or unsafe: $unit" >&2
            return 1
        fi
    done
    if [[ ! -f "$bundle_root/script-host/main.py" \
        || -L "$bundle_root/script-host/main.py" ]]; then
        echo "ERROR: required bundled script host is missing or unsafe" >&2
        return 1
    fi
    if [[ "$redis_included" == true ]]; then
        for binary in redis-server redis-cli; do
            if [[ ! -f "$bundle_root/bin/$binary" \
                || ! -x "$bundle_root/bin/$binary" \
                || -L "$bundle_root/bin/$binary" ]]; then
                echo "ERROR: required bundled Redis binary is unsafe: $binary" >&2
                return 1
            fi
        done
    fi
}

validate_bare_metal_install_dir() {
    local install_directory=$1

    if [[ "$install_directory" != "/opt/aether" ]]; then
        echo "AETHER_INSTALL_DIR overrides are not supported: packaged systemd units require /opt/aether" >&2
        return 1
    fi
}

require_empty_or_absent_bare_metal_root() {
    local path=$1
    local label=$2
    local entry

    validate_no_symlink_components "$path" "$label"
    if [[ -e "$path" || -L "$path" ]]; then
        if [[ ! -d "$path" || -L "$path" ]]; then
            echo "ERROR: fresh installation refused; $label is not a safe directory: $path" >&2
            return 1
        fi
        entry=$(find "$path" -mindepth 1 -print -quit) || return 1
        if [[ -n "$entry" ]]; then
            echo "ERROR: fresh installation refused; $label is not empty: $path" >&2
            return 1
        fi
    fi
}

reject_existing_bare_metal_footprint() {
    local container path unit

    for path in "$INSTALL_DIR" "$CONFIG_DIR" "$DATA_DIR" "$DOCKER_INSTALL_ROOT"; do
        require_empty_or_absent_bare_metal_root \
            "$path" "Aether runtime root" || return 1
    done
    # A standalone /usr/local/bin/aether CLI is deliberately allowed. It is
    # snapshotted before replacement and restored if this fresh install fails.
    for path in \
        /etc/profile.d/aether.sh \
        "$DOCKER_PROFILE_ENTRY" \
        "$SHM_PATH" \
        "$SYSTEMD_DIR/aether.target.wants" \
        "$SYSTEMD_DIR/multi-user.target.wants/aether.target"; do
        if [[ -e "$path" || -L "$path" ]]; then
            echo "ERROR: fresh installation refused; Aether footprint exists: $path" >&2
            return 1
        fi
    done
    for unit in \
        aether.target aether-io.service aether-automation.service \
        aether-history.service aether-api.service aether-uplink.service \
        aether-alarm.service aether-redis.service; do
        if [[ -e "$SYSTEMD_DIR/$unit" || -L "$SYSTEMD_DIR/$unit" ]]; then
            echo "ERROR: fresh installation refused; systemd unit exists: $unit" >&2
            return 1
        fi
        if systemctl is-active --quiet "$unit"; then
            echo "ERROR: fresh installation refused; Aether service is active: $unit" >&2
            return 1
        fi
    done
    if command -v docker >/dev/null 2>&1; then
        for container in \
            aether-io aether-automation aether-history aether-api \
            aether-uplink aether-alarm aether-redis aether-timescaledb; do
            if docker inspect "$container" >/dev/null 2>&1; then
                echo "ERROR: fresh installation refused; Docker container exists: $container" >&2
                return 1
            fi
        done
    fi
}

detect_bundled_redis() {
    local bundle_root=$1
    local trace_count=0
    local valid_count=0

    [[ -e "$bundle_root/bin/redis-server" || -L "$bundle_root/bin/redis-server" ]] \
        && trace_count=$((trace_count + 1))
    [[ -e "$bundle_root/bin/redis-cli" || -L "$bundle_root/bin/redis-cli" ]] \
        && trace_count=$((trace_count + 1))
    [[ -e "$bundle_root/systemd/aether-redis.service" \
        || -L "$bundle_root/systemd/aether-redis.service" ]] \
        && trace_count=$((trace_count + 1))

    [[ -f "$bundle_root/bin/redis-server" \
        && -x "$bundle_root/bin/redis-server" \
        && ! -L "$bundle_root/bin/redis-server" ]] \
        && valid_count=$((valid_count + 1))
    [[ -f "$bundle_root/bin/redis-cli" && -x "$bundle_root/bin/redis-cli" \
        && ! -L "$bundle_root/bin/redis-cli" ]] \
        && valid_count=$((valid_count + 1))
    [[ -f "$bundle_root/systemd/aether-redis.service" \
        && ! -L "$bundle_root/systemd/aether-redis.service" ]] \
        && valid_count=$((valid_count + 1))

    case "$trace_count:$valid_count" in
        0:0)
            printf '%s\n' false
            ;;
        3:3)
            printf '%s\n' true
            ;;
        *)
            echo "ERROR: incomplete optional Redis bundle; expected redis-server, redis-cli, and aether-redis.service" >&2
            return 1
            ;;
    esac
}

validate_expected_symlink_if_exists() {
    local path=$1
    local expected_target=$2
    local label=$3
    local parent resolved_target resolved_expected

    parent=${path%/*}
    [[ -n "$parent" ]] || parent=/
    validate_no_symlink_components "$parent" "$label parent"
    validate_secure_directory_if_exists "$parent" "$label parent"
    if [[ ! -e "$path" && ! -L "$path" ]]; then
        return 0
    fi
    if [[ ! -L "$path" ]]; then
        echo "ERROR: $label must be the expected symlink or absent: $path" >&2
        return 1
    fi
    resolved_target=$(readlink -f -- "$path") || {
        echo "ERROR: $label is a broken or unresolvable symlink: $path" >&2
        return 1
    }
    resolved_expected=$(readlink -f -- "$expected_target") || {
        echo "ERROR: expected target for $label cannot be resolved: $expected_target" >&2
        return 1
    }
    if [[ "$resolved_target" != "$resolved_expected" ]]; then
        echo "ERROR: $label points outside the Aether installation: $path -> $resolved_target" >&2
        return 1
    fi
}

validate_bare_metal_host_layout() {
    local unit runtime_asset path

    if [[ $EUID -ne 0 ]]; then
        echo "ERROR: the bare-metal installer must run as root" >&2
        return 1
    fi

    validate_secure_directory_if_exists "$INSTALL_DIR" "installation root"
    validate_secure_directory_if_exists "$CONFIG_DIR" "configuration root"
    validate_secure_directory_if_exists "$DATA_DIR" "runtime data root"
    validate_secure_directory_if_exists "$SYSTEMD_DIR" "systemd unit root"
    validate_secure_directory_if_exists /usr/local/bin "CLI link parent"
    validate_secure_directory_if_exists /etc/profile.d "profile entry parent"

    if [[ -e "$INSTALL_DIR/bin" ]]; then
        validate_secure_installed_tree "$INSTALL_DIR/bin" "installed binary tree"
    fi
    for path in \
        "$CONFIG_DIR/config" "$CONFIG_DIR/config.template" \
        "$CONFIG_DIR/script-host"; do
        if [[ -e "$path" ]]; then
            validate_secure_installed_tree "$path" "installed configuration tree"
        else
            validate_no_symlink_components "$path" "configuration path"
        fi
    done
    for path in \
        "$CONFIG_DIR/aether.env" "$CONFIG_DIR/install.yaml" \
        "$CONFIG_DIR/script-host/main.py" \
        "$INSTALL_DIR/uninstall.sh" \
        /etc/profile.d/aether.sh; do
        validate_secure_regular_file_if_exists "$path" "installer write target"
    done

    for unit in \
        aether.target aether-io.service aether-automation.service \
        aether-history.service aether-api.service aether-uplink.service \
        aether-alarm.service aether-redis.service; do
        validate_secure_regular_file_if_exists \
            "$SYSTEMD_DIR/$unit" "installed systemd unit"
    done
    validate_secure_directory_if_exists \
        "$SYSTEMD_DIR/aether.target.wants" "Aether target wants directory"
    validate_secure_directory_if_exists \
        "$SYSTEMD_DIR/multi-user.target.wants" "multi-user target wants directory"
    validate_expected_symlink_if_exists \
        "$SYSTEMD_DIR/multi-user.target.wants/aether.target" \
        "$SYSTEMD_DIR/aether.target" "Aether target enablement link"
    # A CLI-only installation is a supported precursor to commissioning. It
    # may be a regular binary rather than the runtime symlink created below.
    validate_regular_file_if_exists /usr/local/bin/aether "standalone Aether CLI"
    if [[ -e /usr/local/bin/aether && ! -x /usr/local/bin/aether ]]; then
        echo "ERROR: standalone Aether CLI is not executable: /usr/local/bin/aether" >&2
        return 1
    fi

    for path in "$DATA_DIR/logs" "$DATA_DIR/redis"; do
        if [[ -e "$path" ]]; then
            validate_secure_installed_tree "$path" "runtime data directory"
        else
            validate_no_symlink_components "$path" "runtime data path"
        fi
    done
    for runtime_asset in \
        aether.db aether.db-wal aether.db-shm aether-history.db \
        aether-history.db-wal aether-history.db-shm uplink.outbox; do
        validate_regular_file_if_exists \
            "$DATA_DIR/$runtime_asset" "runtime state file"
    done
}

snapshot_bare_metal_path() {
    local source_path=$1
    local key=$2

    if [[ -e "$source_path" || -L "$source_path" ]]; then
        touch "$STATE_BACKUP_DIR/$key.present"
        cp -a "$source_path" "$STATE_BACKUP_DIR/$key.payload"
    else
        touch "$STATE_BACKUP_DIR/$key.absent"
    fi
}

restore_bare_metal_path() {
    local target_path=$1
    local key=$2

    rm -rf -- "$target_path" || return 1
    if [[ -f "$STATE_BACKUP_DIR/$key.present" ]]; then
        cp -a "$STATE_BACKUP_DIR/$key.payload" "$target_path" || return 1
    fi
}

snapshot_bare_metal_state() {
    local unit runtime_asset

    STATE_BACKUP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aether-baremetal-state.XXXXXX")
    chmod 700 "$STATE_BACKUP_DIR"
    snapshot_bare_metal_path "$INSTALL_DIR" install-root
    snapshot_bare_metal_path "$CONFIG_DIR" config-root
    snapshot_bare_metal_path "$DATA_DIR" data-root
    snapshot_bare_metal_path "$SYSTEMD_DIR/aether.target" systemd-target
    snapshot_bare_metal_path "$SYSTEMD_DIR/aether.target.wants" systemd-wants
    snapshot_bare_metal_path \
        "$SYSTEMD_DIR/multi-user.target.wants/aether.target" \
        systemd-multi-user-aether-target
    for unit in \
        aether-io aether-automation aether-history aether-api aether-uplink \
        aether-alarm aether-redis; do
        snapshot_bare_metal_path \
            "$SYSTEMD_DIR/$unit.service" "systemd-$unit"
    done
    snapshot_bare_metal_path /usr/local/bin/aether cli-symlink
    snapshot_bare_metal_path /etc/profile.d/aether.sh profile-entry
    snapshot_bare_metal_path "$INSTALL_DIR/uninstall.sh" uninstall-script
    for runtime_asset in \
        aether.db aether.db-wal aether.db-shm aether-history.db \
        aether-history.db-wal aether-history.db-shm uplink.outbox; do
        snapshot_bare_metal_path \
            "$DATA_DIR/$runtime_asset" "runtime-$runtime_asset"
    done
    STATE_SNAPSHOT_CAPTURED=true
}

restore_bare_metal_state() {
    local unit runtime_asset

    [[ "$STATE_SNAPSHOT_CAPTURED" == true \
        && -n "$STATE_BACKUP_DIR" && -d "$STATE_BACKUP_DIR" ]] || return 0
    restore_bare_metal_path "$CONFIG_DIR" config-root || return 1
    restore_bare_metal_path "$SYSTEMD_DIR/aether.target" systemd-target || return 1
    restore_bare_metal_path "$SYSTEMD_DIR/aether.target.wants" systemd-wants || return 1
    restore_bare_metal_path \
        "$SYSTEMD_DIR/multi-user.target.wants/aether.target" \
        systemd-multi-user-aether-target || return 1
    for unit in \
        aether-io aether-automation aether-history aether-api aether-uplink \
        aether-alarm aether-redis; do
        restore_bare_metal_path \
            "$SYSTEMD_DIR/$unit.service" "systemd-$unit" || return 1
    done
    restore_bare_metal_path /usr/local/bin/aether cli-symlink || return 1
    restore_bare_metal_path /etc/profile.d/aether.sh profile-entry || return 1
    restore_bare_metal_path "$INSTALL_DIR/uninstall.sh" uninstall-script || return 1
    for runtime_asset in \
        aether.db aether.db-wal aether.db-shm aether-history.db \
        aether-history.db-wal aether-history.db-shm uplink.outbox; do
        restore_bare_metal_path \
            "$DATA_DIR/$runtime_asset" "runtime-$runtime_asset" || return 1
    done
    # Restore full fresh-install roots last so auxiliary log/cache directories
    # and ownership changes are also removed on failure.
    restore_bare_metal_path "$DATA_DIR" data-root || return 1
    restore_bare_metal_path "$INSTALL_DIR" install-root || return 1
}

generate_bootstrap_admin_password() {
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'
}

is_valid_bootstrap_admin_password() {
    local candidate=${1:-}
    local normalized

    [[ ${#candidate} -ge 16 ]] || return 1
    [[ "$candidate" != [[:space:]]* && "$candidate" != *[[:space:]] ]] || return 1
    if printf '%s' "$candidate" | LC_ALL=C grep -q '[[:cntrl:]]'; then
        return 1
    fi
    normalized=$(printf '%s' "$candidate" | tr '[:upper:]' '[:lower:]')
    case "$normalized" in
        admin|admin123|password|changeme|change-me-in-production|0192023a7bbd73250516f069df18b500)
            return 1
            ;;
    esac
}

replace_env_setting() {
    local env_file=$1
    local key=$2
    local value=$3
    local env_directory temp_file

    env_directory=$(dirname "$env_file")
    temp_file=$(mktemp "$env_directory/aether.env.tmp.XXXXXX")
    awk -v key="$key" 'index($0, key "=") != 1' "$env_file" > "$temp_file"
    printf '%s=%s\n' "$key" "$value" >> "$temp_file"
    chmod 600 "$temp_file"
    mv -f "$temp_file" "$env_file"
}

bare_metal_database_requires_bootstrap_admin() {
    local database_file=$1
    local has_users_table user_count

    [[ -s "$database_file" ]] || return 0
    if command -v sqlite3 >/dev/null 2>&1; then
        has_users_table=$(sqlite3 "$database_file" \
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users';" \
            2>/dev/null) || return 0
        [[ "$has_users_table" == 1 ]] || return 0
        user_count=$(sqlite3 "$database_file" "SELECT COUNT(*) FROM users;" 2>/dev/null) \
            || return 0
        [[ "$user_count" =~ ^[0-9]+$ && "$user_count" -gt 0 ]] && return 1
    fi
    return 0
}

ensure_bare_metal_bootstrap_admin() {
    local env_file=$1
    local database_file=$2
    local current_password=""
    local generated_password

    ensure_env_setting "$env_file" "AETHER_ALLOW_PUBLIC_REGISTRATION" "false"
    if ! bare_metal_database_requires_bootstrap_admin "$database_file"; then
        chmod 600 "$env_file"
        return 0
    fi

    BOOTSTRAP_ADMIN_REQUIRED=true
    current_password=$(sed -n 's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' "$env_file" \
        | tail -n 1 | tr -d '\r')
    if [[ -n "$current_password" ]]; then
        is_valid_bootstrap_admin_password "$current_password" || {
            echo "ERROR: existing AETHER_BOOTSTRAP_ADMIN_PASSWORD is not safe for first startup" >&2
            return 1
        }
        chmod 600 "$env_file"
        echo "  Preserving the existing bootstrap administrator credential."
        return 0
    fi

    generated_password=$(generate_bootstrap_admin_password)
    is_valid_bootstrap_admin_password "$generated_password" || {
        echo "ERROR: failed to generate a strong bootstrap administrator password" >&2
        return 1
    }
    replace_env_setting "$env_file" "AETHER_BOOTSTRAP_ADMIN_PASSWORD" "$generated_password"
    echo "  Generated a private bootstrap administrator credential (value not printed)."
}

ensure_env_setting() {
    local env_file=$1
    local key=$2
    local default_value=$3

    if grep -q "^${key}=" "$env_file"; then
        echo "  Preserving existing $key from $env_file"
        return 0
    fi
    printf '%s=%s\n' "$key" "$default_value" >> "$env_file"
}

restore_previous_binaries() {
    local failed_binary_directory=""

    [[ "$BINARIES_SWAPPED" == true ]] || return 0

    if [[ -e "$INSTALL_DIR/bin" ]]; then
        failed_binary_directory=$(mktemp -d "$INSTALL_DIR/.bin.failed.XXXXXX")
        rmdir "$failed_binary_directory"
        mv "$INSTALL_DIR/bin" "$failed_binary_directory"
    fi
    if [[ -n "$BINARY_BACKUP" && -e "$BINARY_BACKUP" ]]; then
        mv "$BINARY_BACKUP" "$INSTALL_DIR/bin"
        BINARY_BACKUP=""
        echo "  Previous service binaries restored."
    else
        echo "  No previous binary set existed; removing the incomplete first install."
    fi
    if [[ -n "$failed_binary_directory" && -e "$failed_binary_directory" ]]; then
        rm -rf "$failed_binary_directory"
    fi
    BINARIES_SWAPPED=false
}

quiesce_aether_services_for_rollback() {
    local unit
    local stop_failed=false
    local units=(
        aether.target
        aether-io.service
        aether-automation.service
        aether-history.service
        aether-api.service
        aether-uplink.service
        aether-alarm.service
        aether-redis.service
    )

    for unit in "${units[@]}"; do
        if systemctl is-active --quiet "$unit"; then
            if ! systemctl stop "$unit"; then
                stop_failed=true
            fi
        fi
    done

    # A target stop may have failed because one child was still transitioning.
    # Retry once after addressing every concrete unit, then verify all states.
    if systemctl is-active --quiet aether.target; then
        if ! systemctl stop aether.target; then
            stop_failed=true
        fi
    fi
    for unit in "${units[@]}"; do
        if systemctl is-active --quiet "$unit"; then
            echo "ERROR: rollback cannot quiesce $unit" >&2
            stop_failed=true
        fi
    done

    [[ "$stop_failed" == false ]]
}

wait_for_bare_metal_services() {
    local attempts=${1:-30}
    local attempt unit
    local stable=0
    local units=(
        aether-io.service
        aether-automation.service
        aether-history.service
        aether-api.service
        aether-uplink.service
        aether-alarm.service
    )

    if [[ "$REDIS_INCLUDED" == true ]]; then
        units+=(aether-redis.service)
    fi

    for ((attempt = 1; attempt <= attempts; attempt++)); do
        local all_active=true
        for unit in "${units[@]}"; do
            if ! systemctl is-active --quiet "$unit"; then
                all_active=false
                break
            fi
        done
        if [[ "$all_active" == true ]] \
            && "$INSTALL_DIR/bin/aether" \
                --config-path "$CONFIG_DIR/config" \
                --db-path "$DATA_DIR" doctor --json >/dev/null 2>&1; then
            stable=$((stable + 1))
            if [[ "$stable" -ge 3 ]]; then
                return 0
            fi
        else
            stable=0
        fi
        sleep 2
    done
    echo "ERROR: installed systemd services did not pass the commit health window" >&2
    return 1
}

finish_install() {
    local status=$?
    local binary_restore_safe=true
    local preserve_binary_backup=false
    local preserve_state_backup=false
    local rollback_complete=true

    trap - EXIT
    if [[ "$INSTALL_COMPLETED" != true ]]; then
        echo "ERROR: installation did not complete; starting rollback." >&2
        if [[ "$SERVICE_STATE_CAPTURED" == true ]] \
            && ! quiesce_aether_services_for_rollback; then
            echo "ERROR: running services could not be stopped; binaries were left in place for manual recovery." >&2
            binary_restore_safe=false
            preserve_binary_backup=true
            preserve_state_backup=true
            rollback_complete=false
        fi
        if [[ "$binary_restore_safe" == true ]] \
            && ! restore_previous_binaries; then
            echo "ERROR: failed to restore the previous binaries; manual recovery is required." >&2
            binary_restore_safe=false
            preserve_binary_backup=true
            preserve_state_backup=true
            rollback_complete=false
        fi
        if [[ "$binary_restore_safe" == true ]] \
            && ! restore_bare_metal_state; then
            echo "ERROR: failed to restore the previous configuration/unit/data snapshot." >&2
            binary_restore_safe=false
            preserve_state_backup=true
            rollback_complete=false
        fi
        if [[ "$binary_restore_safe" == true \
            && "$SERVICE_STATE_CAPTURED" == true ]]; then
            if ! systemctl daemon-reload; then
                rollback_complete=false
            fi
            for unit in aether-redis.service aether.target; do
                systemctl disable "$unit" >/dev/null 2>&1 || true
                if systemctl is-enabled --quiet "$unit"; then
                    rollback_complete=false
                fi
            done
        fi
        if [[ "$rollback_complete" == true ]]; then
            echo "NOTICE: failed fresh-install binaries, configuration, units, runtime data, and enablement were removed." >&2
        else
            echo "ERROR: rollback is incomplete; preserved backups require manual recovery. Do not assume the installed host state is consistent." >&2
        fi
    fi

    if [[ -n "$BINARY_STAGE" && -e "$BINARY_STAGE" ]]; then
        rm -rf "$BINARY_STAGE"
    fi
    if [[ "$preserve_binary_backup" != true \
        && -n "$BINARY_BACKUP" && -e "$BINARY_BACKUP" ]]; then
        rm -rf "$BINARY_BACKUP"
    elif [[ -n "$BINARY_BACKUP" && -e "$BINARY_BACKUP" ]]; then
        echo "NOTICE: binary rollback evidence preserved for manual recovery at $BINARY_BACKUP" >&2
    fi
    if [[ "$preserve_state_backup" != true \
        && -n "$STATE_BACKUP_DIR" && -d "$STATE_BACKUP_DIR" ]]; then
        rm -rf "$STATE_BACKUP_DIR"
    elif [[ -n "$STATE_BACKUP_DIR" && -d "$STATE_BACKUP_DIR" ]]; then
        echo "NOTICE: host-state rollback evidence preserved for manual recovery at $STATE_BACKUP_DIR" >&2
    fi
    exit "$status"
}

if [[ "${AETHER_BARE_METAL_INSTALLER_FUNCTIONS_ONLY:-false}" == true ]]; then
    return 0 2>/dev/null || exit 0
fi

REDIS_INCLUDED=$(detect_bundled_redis .)

echo "=== AetherIot bare-metal installer ==="

validate_bare_metal_install_dir "$INSTALL_DIR"

if ! command -v systemctl >/dev/null 2>&1; then
    echo "ERROR: systemctl not found. This installer requires a systemd-based" >&2
    echo "Linux distribution. See docs/guides/deployment.md for Docker Compose" >&2
    echo "as an alternative on non-systemd systems." >&2
    exit 1
fi
validate_bare_metal_bundle . "$REDIS_INCLUDED"
if [[ ! -f config.template/runtime-manifest.json \
    || -L config.template/runtime-manifest.json ]]; then
    echo "ERROR: bare-metal package is missing a regular runtime manifest" >&2
    exit 1
fi
if ! bin/aether --json runtime-manifest \
    --path config.template/runtime-manifest.json >/dev/null; then
    echo "ERROR: bare-metal runtime manifest failed verification" >&2
    exit 1
fi
reject_existing_bare_metal_footprint
validate_bare_metal_host_layout
# makeself preserves archive ownership when it is executed as root. Take
# ownership of the validated extraction tree before any packaged artifact is
# copied into a root-executed location.
normalize_root_owned_tree .
trap finish_install EXIT

echo "[1/7] Capturing rollback state for this fresh installation..."
# Close the preflight-to-snapshot race. A fresh installer never stops or
# adopts an existing runtime; any footprint appearing here aborts untouched.
reject_existing_bare_metal_footprint
snapshot_bare_metal_state
SERVICE_STATE_CAPTURED=true

echo "[2/7] Installing binaries to $INSTALL_DIR/bin ..."
mkdir -p "$INSTALL_DIR"
chown 0:0 "$INSTALL_DIR"
chmod go-w "$INSTALL_DIR"
BINARY_STAGE=$(mktemp -d "$INSTALL_DIR/.bin.stage.XXXXXX")
cp -a bin/. "$BINARY_STAGE/"
normalize_root_owned_tree "$BINARY_STAGE"
find "$BINARY_STAGE" -type d -exec chmod 0755 {} +
find "$BINARY_STAGE" -type f -exec chmod 0755 {} +
if [[ -e "$INSTALL_DIR/bin" ]]; then
    BINARY_BACKUP=$(mktemp -d "$INSTALL_DIR/.bin.backup.XXXXXX")
    rmdir "$BINARY_BACKUP"
    mv "$INSTALL_DIR/bin" "$BINARY_BACKUP"
fi
BINARIES_SWAPPED=true
mv "$BINARY_STAGE" "$INSTALL_DIR/bin"
BINARY_STAGE=""
# The CLI and optional bundled tools are meant to be run interactively; make
# the package bin directory reachable on PATH.
ln -sfn "$INSTALL_DIR/bin/aether" /usr/local/bin/aether
echo "export PATH=\"$INSTALL_DIR/bin:\$PATH\"" > /etc/profile.d/aether.sh
chown 0:0 /etc/profile.d/aether.sh
chmod 0644 /etc/profile.d/aether.sh

echo "[3/7] Installing the headless kernel package (no bundled Web UI)."

echo "[4/7] Preparing runtime data directories under $DATA_DIR ..."
mkdir -p "$DATA_DIR/logs"
chown 0:0 "$DATA_DIR" "$DATA_DIR/logs"
chmod go-w "$DATA_DIR" "$DATA_DIR/logs"
if [[ -x "$INSTALL_DIR/bin/redis-server" ]]; then
    mkdir -p "$DATA_DIR/redis"
    chown 0:0 "$DATA_DIR/redis"
    chmod go-w "$DATA_DIR/redis"
fi

echo "[5/7] Installing configuration to $CONFIG_DIR ..."
mkdir -p "$CONFIG_DIR"
mkdir -p "$CONFIG_DIR/script-host"
chown 0:0 "$CONFIG_DIR" "$CONFIG_DIR/script-host"
chmod go-w "$CONFIG_DIR" "$CONFIG_DIR/script-host"
install -o 0 -g 0 -m 0644 \
    script-host/main.py "$CONFIG_DIR/script-host/main.py"

if [[ ! -e "$CONFIG_DIR/config" ]]; then
    echo "  First install detected: activating config.template/ -> $CONFIG_DIR/config"
    CONFIG_STAGE=$(mktemp -d "$CONFIG_DIR/.config.tmp.XXXXXX")
    cp -a config.template/. "$CONFIG_STAGE/"
    normalize_root_owned_tree "$CONFIG_STAGE"
    find "$CONFIG_STAGE" -type d -exec chmod 0755 {} +
    find "$CONFIG_STAGE" -type f -exec chmod 0644 {} +
    mv "$CONFIG_STAGE" "$CONFIG_DIR/config"
else
    echo "ERROR: fresh installation refused; configuration appeared after preflight: $CONFIG_DIR/config" >&2
    exit 1
fi
normalize_root_owned_tree "$CONFIG_DIR/config"

if [[ -e "$CONFIG_DIR/aether.env" || -L "$CONFIG_DIR/aether.env" ]]; then
    echo "ERROR: fresh installation refused; environment file appeared after preflight: $CONFIG_DIR/aether.env" >&2
    exit 1
fi
JWT_SECRET=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
UPLINK_CONTROL_TOKEN=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
while [[ "$UPLINK_CONTROL_TOKEN" == "$JWT_SECRET" ]]; do
    UPLINK_CONTROL_TOKEN=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
done
AETHER_ENV_TEMP=$(mktemp "$CONFIG_DIR/aether.env.tmp.XXXXXX")
chmod 600 "$AETHER_ENV_TEMP"
cat > "$AETHER_ENV_TEMP" <<EOF
AETHER_DB_PATH=$DATA_DIR/aether.db
AETHER_HISTORY_DB_PATH=$DATA_DIR/aether-history.db
AETHER_LOG_DIR=$DATA_DIR/logs
RUST_LOG=info
JWT_SECRET_KEY=$JWT_SECRET
AETHER_UPLINK_CONTROL_TOKEN=$UPLINK_CONTROL_TOKEN
EOF
chown 0:0 "$AETHER_ENV_TEMP"
mv "$AETHER_ENV_TEMP" "$CONFIG_DIR/aether.env"
echo "  Wrote default $CONFIG_DIR/aether.env (generated JWT and uplink control credentials)"
# Lock the credential file before reading or appending any bootstrap secret.
chmod 600 "$CONFIG_DIR/aether.env"
chown 0:0 "$CONFIG_DIR/aether.env"
ensure_env_setting "$CONFIG_DIR/aether.env" "AETHER_CONFIG_PATH" "$CONFIG_DIR/config"
ensure_env_setting "$CONFIG_DIR/aether.env" "AETHER_DATA_PATH" "$DATA_DIR"
ensure_bare_metal_bootstrap_admin "$CONFIG_DIR/aether.env" "$DATA_DIR/aether.db"
# aether.env contains JWT_SECRET_KEY in plaintext — tighten permissions every
# run, not just on first write, in case an older installer left it world-readable.
chown 0:0 "$CONFIG_DIR/aether.env"
chmod 600 "$CONFIG_DIR/aether.env"

# Persist the host-side layout so every subsequent CLI invocation resolves the
# same configuration and data directories without requiring shell-specific
# environment variables.
if [[ -e "$INSTALL_CONTEXT_FILE" || -L "$INSTALL_CONTEXT_FILE" ]]; then
    echo "ERROR: fresh installation refused; install context appeared after preflight: $INSTALL_CONTEXT_FILE" >&2
    exit 1
fi
INSTALL_CONTEXT_TEMP=$(mktemp "$CONFIG_DIR/install.yaml.tmp.XXXXXX")
cat > "$INSTALL_CONTEXT_TEMP" <<EOF
mode: systemd
config_dir: $CONFIG_DIR/config
data_dir: $DATA_DIR
runtime_dir: /run/aether
channel: stable
packs: []
EOF
chmod 644 "$INSTALL_CONTEXT_TEMP"
chown 0:0 "$INSTALL_CONTEXT_TEMP"
if ! ln "$INSTALL_CONTEXT_TEMP" "$INSTALL_CONTEXT_FILE" 2>/dev/null; then
    rm -f "$INSTALL_CONTEXT_TEMP"
    echo "ERROR: failed to create fresh install context $INSTALL_CONTEXT_FILE" >&2
    exit 1
fi
rm -f "$INSTALL_CONTEXT_TEMP"
echo "  Wrote install context $INSTALL_CONTEXT_FILE"
chown 0:0 "$INSTALL_CONTEXT_FILE"
chmod 0644 "$INSTALL_CONTEXT_FILE"

echo "[6/7] Installing systemd units ..."
for unit_file in systemd/*.service systemd/*.target; do
    install -o 0 -g 0 -m 0644 "$unit_file" "$SYSTEMD_DIR/"
done
systemctl daemon-reload

cat > "$INSTALL_DIR/uninstall.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
echo "Stopping AetherIot..."
systemctl stop aether.target || true
systemctl disable aether.target || true
rm -f $SYSTEMD_DIR/aether-*.service $SYSTEMD_DIR/aether.target
systemctl daemon-reload
rm -f /usr/local/bin/aether /etc/profile.d/aether.sh
rm -rf "$INSTALL_DIR"
echo "AetherIot removed. Configuration and data preserved at $CONFIG_DIR and $DATA_DIR."
echo "Delete those manually if you want a full wipe."
EOF
chown 0:0 "$INSTALL_DIR/uninstall.sh"
chmod 0755 "$INSTALL_DIR/uninstall.sh"

echo "[7/7] Initializing database and starting services ..."
for runtime_asset in \
    aether.db aether.db-wal aether.db-shm aether-history.db \
    aether-history.db-wal aether-history.db-shm uplink.outbox; do
    if [[ -e "$DATA_DIR/$runtime_asset" || -L "$DATA_DIR/$runtime_asset" ]]; then
        echo "ERROR: fresh installation refused; runtime state appeared after preflight: $DATA_DIR/$runtime_asset" >&2
        exit 1
    fi
done
"$INSTALL_DIR/bin/aether" --config-path "$CONFIG_DIR/config" --db-path "$DATA_DIR" init
"$INSTALL_DIR/bin/aether" --config-path "$CONFIG_DIR/config" --db-path "$DATA_DIR" sync

if [[ "$REDIS_INCLUDED" == true ]]; then
    systemctl enable aether-redis.service
fi
systemctl enable --now aether.target
wait_for_bare_metal_services 30
INSTALL_COMPLETED=true

if [[ "$BOOTSTRAP_ADMIN_REQUIRED" == true ]]; then
    echo ""
    echo "Bootstrap administrator action required:"
    echo "  Username: admin"
    echo "  Retrieve the generated password locally (it is never printed by the installer):"
    echo "    sed -n 's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' '$CONFIG_DIR/aether.env'"
    echo "  Change the bootstrap administrator password immediately after the first login."
    echo "  Then remove AETHER_BOOTSTRAP_ADMIN_PASSWORD from $CONFIG_DIR/aether.env."
fi

echo ""
echo "=== Install complete ==="
systemctl --no-pager status aether.target || true
echo ""
echo "Check full health with: aether doctor"
