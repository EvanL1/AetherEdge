#!/usr/bin/env bash
# AetherEMS Installation Script

set -euo pipefail

# Target architecture (injected by build-installer.sh, do not edit manually)
INSTALLER_ARCH_LABEL="ARM64"    # Display name: ARM64 or AMD64
INSTALLER_ARCH_UNAME="aarch64"  # uname -m value: aarch64 or x86_64
INSTALLER_ARCH_SHORT="arm64"    # Short name: arm64 or amd64

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Installation directories (configurable via environment variables)
# Default: /opt/AetherEdge for production, can be overridden
INSTALL_DIR="${AETHER_INSTALL_DIR:-${INSTALL_DIR:-/opt/AetherEdge}}"
INSTALL_CONTEXT_FILE="${AETHER_INSTALL_CONTEXT_PATH:-/etc/aether/install.yaml}"
PROFILE_ENTRY="/etc/profile.d/aetheredge.sh"
SHM_PATH="/dev/shm/aether-rtdb.shm"
AETHER_COMPOSE_PROJECT="aetheredge"
DATA_DIR=""
LIVE_CONFIG_DIR=""
# Allow logs to be stored on external storage if available
# Accept both AETHER_LOG_PATH (matches docker-compose.yml) and AETHER_LOG_DIR (legacy)
LOG_DIR="${AETHER_LOG_PATH:-${AETHER_LOG_DIR:-${LOG_DIR:-$INSTALL_DIR/logs}}}"

# Save the directory where installation was launched (for cleanup later)
LAUNCH_DIR="${LAUNCH_DIR:-$(pwd)}"

# =============================================================================
# Command Line Arguments
# =============================================================================
# Default: auto mode when stdin is not a TTY (e.g. makeself, pipe, ssh)
if [ -t 0 ]; then
    AUTO_MODE=false
else
    AUTO_MODE=true
fi
SHOW_HELP=false
BOOTSTRAP_ADMIN_REQUIRED=false
TIMESCALE_DATA_DIR=""
REDIS_EXTENSION_SELECTED=false

while [[ $# -gt 0 ]]; do
    case $1 in
        -a|--auto)
            AUTO_MODE=true
            shift
            ;;
        --help|-h)
            SHOW_HELP=true
            shift
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            echo "Usage: $0 [-a|--auto] [--help|-h]"
            exit 1
            ;;
    esac
done

if [[ "$SHOW_HELP" == true ]]; then
    echo "AetherEdge ${INSTALLER_ARCH_LABEL} Installation Script"
    echo ""
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  -a, --auto          Auto mode: no prompts, start services automatically"
    echo "  --help, -h          Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0                  Interactive installation (default)"
    echo "  $0 -a               Automatic installation with auto-start"
    exit 0
fi

if [[ "$AUTO_MODE" == false ]]; then
    echo -e "${BLUE}Running in INTERACTIVE mode - will prompt for confirmations${NC}"
fi

# Docker Compose V1/V2 compatibility functions
detect_docker_compose_cmd() {
    if docker compose version &>/dev/null 2>&1; then
        echo "docker compose"
    elif command -v docker-compose &>/dev/null; then
        echo "docker-compose"
    else
        echo -e "${RED}ERROR: Neither 'docker compose' (V2) nor 'docker-compose' (V1) found${NC}" >&2
        echo -e "${YELLOW}Please install Docker Compose: https://docs.docker.com/compose/install/${NC}" >&2
        return 1
    fi
}

run_docker_compose() {
    local compose_cmd

    # This wrapper must be side-effect free. In particular, rollback invokes it
    # after restoring the previous .env; mutating secrets here would make the
    # restored Compose file start with a different runtime identity.
    compose_cmd=$(detect_docker_compose_cmd) || return 1

    if [[ "$compose_cmd" == "docker compose" ]]; then
        docker compose --project-name "$AETHER_COMPOSE_PROJECT" "$@"
    else
        docker-compose --project-name "$AETHER_COMPOSE_PROJECT" "$@"
    fi
}

generate_jwt_secret() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
    else
        # `od` and `/dev/urandom` are available on the supported Linux targets.
        od -An -N32 -tx1 /dev/urandom | tr -d '[:space:]'
    fi
}

generate_bootstrap_admin_password() {
    # 32 random bytes encoded as 64 hexadecimal characters: strong, printable,
    # and safe to persist in an unquoted Docker Compose environment file.
    generate_jwt_secret
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

persist_compose_env_value() {
    local env_file=$1
    local key=$2
    local value=$3
    local env_directory temp_file

    env_directory=$(dirname "$env_file")
    if [[ -L "$env_file" || ( -e "$env_file" && ! -f "$env_file" ) ]]; then
        echo "Refusing unsafe Compose environment target: $env_file" >&2
        return 1
    fi
    $SUDO mkdir -p "$env_directory"
    temp_file=$($SUDO mktemp "$env_directory/.env.tmp.XXXXXX")
    if [[ -f "$env_file" ]]; then
        # shellcheck disable=SC2016 # awk evaluates key; this is not shell expansion.
        $SUDO awk -v key="$key" 'index($0, key "=") != 1' "$env_file" \
            | $SUDO tee "$temp_file" >/dev/null
    fi
    printf '%s=%s\n' "$key" "$value" | $SUDO tee -a "$temp_file" >/dev/null
    $SUDO chown "${ACTUAL_UID}:${ACTUAL_GID}" "$temp_file"
    $SUDO chmod 600 "$temp_file"
    $SUDO mv -f "$temp_file" "$env_file"
}

ensure_compose_env_default() {
    local env_file=$1
    local key=$2
    local default_value=$3

    if [[ -f "$env_file" ]] && $SUDO grep -q "^${key}=" "$env_file"; then
        return 0
    fi
    persist_compose_env_value "$env_file" "$key" "$default_value"
}

database_requires_bootstrap_admin() {
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
    # Unknown state is treated like an empty users table. Supplying an unused
    # bootstrap secret to an existing installation is harmless because the API
    # reads it only after proving the table is empty.
    return 0
}

ensure_compose_bootstrap_admin() {
    local env_file=$1
    local database_file=$2
    local current_password=""
    local selected_password=""

    ensure_compose_env_default "$env_file" "AETHER_ALLOW_PUBLIC_REGISTRATION" "false"
    if ! database_requires_bootstrap_admin "$database_file"; then
        $SUDO chmod 600 "$env_file"
        return 0
    fi

    BOOTSTRAP_ADMIN_REQUIRED=true
    if [[ -f "$env_file" ]]; then
        current_password=$($SUDO sed -n \
            's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' "$env_file" \
            | tail -n 1 | tr -d '\r')
    fi
    if [[ -n "$current_password" ]]; then
        if ! is_valid_bootstrap_admin_password "$current_password"; then
            echo "ERROR: existing AETHER_BOOTSTRAP_ADMIN_PASSWORD is not safe for first startup" >&2
            return 1
        fi
        $SUDO chmod 600 "$env_file"
        echo "  Preserving the existing bootstrap administrator credential."
        return 0
    fi

    selected_password=$(generate_bootstrap_admin_password)
    is_valid_bootstrap_admin_password "$selected_password" || {
        echo "ERROR: failed to generate a strong bootstrap administrator password" >&2
        return 1
    }
    persist_compose_env_value \
        "$env_file" \
        "AETHER_BOOTSTRAP_ADMIN_PASSWORD" \
        "$selected_password"
    echo "  Generated a private bootstrap administrator credential (value not printed)."
}

ensure_compose_timescaledb_password() {
    local env_file=$1
    local current_password=""

    if [[ -f "$env_file" ]]; then
        current_password=$($SUDO sed -n 's/^TIMESCALEDB_PASSWORD=//p' "$env_file" \
            | tail -n 1 | tr -d '\r')
    fi
    if [[ -n "$current_password" ]]; then
        $SUDO chmod 600 "$env_file"
        return 0
    fi

    persist_compose_env_value "$env_file" "TIMESCALEDB_PASSWORD" "$(generate_jwt_secret)"
    echo "  Generated a private TimescaleDB extension password (value not printed)."
}

print_bootstrap_admin_instructions() {
    local env_file=$1

    [[ "$BOOTSTRAP_ADMIN_REQUIRED" == true ]] || return 0
    echo ""
    echo -e "${YELLOW}Bootstrap administrator action required:${NC}"
    echo "  Username: admin"
    echo "  Retrieve the generated password locally (it is never printed by the installer):"
    echo "    sudo sed -n 's/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=//p' '$env_file'"
    echo "  Change the bootstrap administrator password immediately after the first login."
    echo "  Then remove AETHER_BOOTSTRAP_ADMIN_PASSWORD from $env_file."
}

yaml_escape_double_quoted_string() {
    local value=$1
    case "$value" in
        *$'\n'*|*$'\r'*)
            echo "Install paths cannot contain newline characters" >&2
            return 1
            ;;
    esac
    printf '%s' "$value" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

require_absolute_directory() {
    local label=$1
    local directory=$2

    case "$directory" in
        /*) ;;
        *)
            echo "$label must be an absolute path: $directory" >&2
            return 1
            ;;
    esac
}

normalize_absolute_directory() {
    local directory=$1

    require_absolute_directory "Aether data directory" "$directory" || return 1
    while [[ "$directory" == *//* ]]; do
        directory=${directory//\/\//\/}
    done
    while [[ "$directory" != / && "$directory" == */ ]]; do
        directory=${directory%/}
    done
    [[ -n "$directory" ]] || directory=/
    printf '%s\n' "$directory"
}

validate_compose_data_directory() {
    local directory
    local cursor
    local existing_cursor
    local resolved_cursor
    local unresolved_suffix=""

    directory=$(normalize_absolute_directory "$1") || return 1
    case "$directory" in
        *$'\n'*|*$'\r'*)
            echo "Aether data directory cannot contain newline characters" >&2
            return 1
            ;;
    esac
    if [[ ! "$directory" =~ ^[A-Za-z0-9._/+-]+$ ]]; then
        echo "Aether data directory contains characters unsafe for Docker Compose .env: $directory" >&2
        return 1
    fi
    case "$directory/" in
        *'/../'*|*'/./'*)
            echo "Aether data directory must not contain . or .. path components: $directory" >&2
            return 1
            ;;
    esac
    case "$directory" in
        /|/bin|/boot|/data|/dev|/etc|/extp|/home|/lib|/lib64|/media|/mnt|/opt|/proc|/root|/run|/sbin|/srv|/sys|/tmp|/usr|/var|/Applications|/Library|/System|/Users|/Volumes)
            echo "Refusing unsafe Aether data root: $directory (use a dedicated child directory)" >&2
            return 1
            ;;
    esac
    if [[ "$directory" == "$INSTALL_DIR" ]]; then
        echo "Aether data directory must not equal the installation root: $directory" >&2
        return 1
    fi

    # Installation paths are privileged recursive-write roots. Reject every
    # symlink component, even when it currently resolves to an apparently safe
    # child directory: the link can otherwise redirect a later chown/remove
    # outside Aether's selected root.
    cursor=$directory
    while [[ "$cursor" != / ]]; do
        if [[ -L "$cursor" ]]; then
            echo "Refusing Aether data path with symlink component: $cursor" >&2
            return 1
        fi
        cursor=${cursor%/*}
        [[ -n "$cursor" ]] || cursor=/
    done

    # Prove that physical resolution of the nearest existing ancestor keeps
    # the requested path exactly inside the operator-selected root. This also
    # catches platform aliases that are not exposed by a simple final-component
    # test and protects not-yet-created descendants.
    existing_cursor=$directory
    while [[ ! -e "$existing_cursor" ]]; do
        unresolved_suffix="/${existing_cursor##*/}${unresolved_suffix}"
        existing_cursor=${existing_cursor%/*}
        [[ -n "$existing_cursor" ]] || existing_cursor=/
    done
    if [[ ! -d "$existing_cursor" ]]; then
        echo "Nearest existing Aether path ancestor is not a directory: $existing_cursor" >&2
        return 1
    fi
    resolved_cursor=$(cd "$existing_cursor" 2>/dev/null && pwd -P) || {
        echo "Unable to resolve Aether path ancestor safely: $existing_cursor" >&2
        return 1
    }
    if [[ "${resolved_cursor%/}${unresolved_suffix}" != "$directory" ]]; then
        echo "Refusing Aether path whose physical resolution escapes the selected root: $directory" >&2
        return 1
    fi
}

is_safe_external_storage_root() {
    local root=$1
    local resolved_root

    [[ -d "$root" && ! -L "$root" ]] || return 1
    resolved_root=$(realpath "$root" 2>/dev/null) || return 1
    [[ "$resolved_root" == "$root" ]]
}

validate_docker_install_dir() {
    local install_directory=$1

    if [[ "$install_directory" != "/opt/AetherEdge" ]]; then
        echo "AETHER_INSTALL_DIR overrides are not supported: packaged Compose paths require /opt/AetherEdge" >&2
        return 1
    fi
}

reject_existing_path() {
    local path=$1
    local label=$2

    if $SUDO test -e "$path" || $SUDO test -L "$path"; then
        echo "Fresh installation refused: $label already exists at $path" >&2
        return 1
    fi
}

require_empty_or_absent_directory() {
    local path=$1
    local label=$2
    local entry

    validate_compose_data_directory "$path" || return 1
    if $SUDO test -e "$path" || $SUDO test -L "$path"; then
        if ! $SUDO test -d "$path" || $SUDO test -L "$path"; then
            echo "Fresh installation refused: $label is not a safe directory: $path" >&2
            return 1
        fi
        entry=$($SUDO find "$path" -mindepth 1 -print -quit) || {
            echo "Unable to inspect fresh $label: $path" >&2
            return 1
        }
        if [[ -n "$entry" ]]; then
            echo "Fresh installation refused: $label is not empty: $path" >&2
            return 1
        fi
    fi
}

require_empty_or_absent_install_root() {
    local path=$1
    local entry
    local cursor=$path

    while [[ "$cursor" != / ]]; do
        if [[ -L "$cursor" ]]; then
            echo "Fresh installation refused: installation root has a symlink component: $cursor" >&2
            return 1
        fi
        cursor=${cursor%/*}
        [[ -n "$cursor" ]] || cursor=/
    done
    if $SUDO test -e "$path" || $SUDO test -L "$path"; then
        if ! $SUDO test -d "$path" || $SUDO test -L "$path"; then
            echo "Fresh installation refused: installation root is not a safe directory: $path" >&2
            return 1
        fi
        entry=$($SUDO find "$path" -mindepth 1 -print -quit) || return 1
        if [[ -n "$entry" ]]; then
            echo "Fresh installation refused: Aether installation root is not empty: $path" >&2
            return 1
        fi
    fi
}

validate_standalone_cli_for_fresh_install() {
    local cli_path=$1

    if $SUDO test -e "$cli_path" || $SUDO test -L "$cli_path"; then
        if ! $SUDO test -f "$cli_path" \
            || $SUDO test -L "$cli_path" \
            || ! $SUDO test -x "$cli_path"; then
            echo "Fresh installation refused: standalone Aether CLI is unsafe: $cli_path" >&2
            return 1
        fi
    fi
}

reject_existing_docker_filesystem_footprint() {
    local path unit

    # An independently installed CLI is intentionally allowed. It is
    # snapshotted and replaced transactionally later, then restored on failure.
    for path in "$INSTALL_DIR" /opt/aether /etc/aether /var/lib/aether; do
        require_empty_or_absent_install_root "$path" || return 1
    done
    for path in \
        /etc/profile.d/aether.sh \
        "$PROFILE_ENTRY" \
        "$SHM_PATH"; do
        reject_existing_path "$path" "Aether filesystem footprint" || return 1
    done
    validate_standalone_cli_for_fresh_install /usr/local/bin/aether || return 1
    for unit in \
        aether.target aether-io.service aether-automation.service \
        aether-history.service aether-api.service aether-uplink.service \
        aether-alarm.service aether-apps.service aether-redis.service; do
        reject_existing_path "/etc/systemd/system/$unit" "Aether systemd footprint" \
            || return 1
        if command -v systemctl >/dev/null 2>&1 \
            && systemctl is-active --quiet "$unit"; then
            echo "Fresh installation refused: Aether systemd service is active: $unit" >&2
            return 1
        fi
    done
}

reject_existing_docker_container_footprint() {
    local container

    while IFS= read -r container; do
        if docker inspect "$container" >/dev/null 2>&1; then
            echo "Fresh installation refused: Docker container already exists: $container" >&2
            return 1
        fi
    done < <(known_aether_containers)
}

reject_existing_docker_runtime_footprint() {
    local image

    reject_existing_docker_container_footprint || return 1
    for image in aetherems:latest aether-apps:latest; do
        if docker image inspect "$image" >/dev/null 2>&1; then
            echo "Fresh installation refused: Docker image already exists: $image" >&2
            return 1
        fi
    done
}

known_aether_redis_volumes() {
    printf '%s\n' \
        "${AETHER_COMPOSE_PROJECT}_redis-data" \
        "${AETHER_COMPOSE_PROJECT}_redis-socket"
}

reject_existing_redis_volume_footprint() {
    local volume

    while IFS= read -r volume; do
        if docker volume inspect "$volume" >/dev/null 2>&1; then
            echo "Fresh installation refused: Redis volume already exists: $volume" >&2
            return 1
        fi
    done < <(known_aether_redis_volumes)
}

remove_fresh_redis_volumes() {
    local volume

    [[ "$REDIS_EXTENSION_SELECTED" == true ]] || return 0
    while IFS= read -r volume; do
        if docker volume inspect "$volume" >/dev/null 2>&1 \
            && ! docker volume rm "$volume" >/dev/null; then
            return 1
        fi
    done < <(known_aether_redis_volumes)
}

resolve_compose_data_directory() {
    local configured_directory="${AETHER_BASE_PATH:-}"

    if [[ -z "$configured_directory" ]]; then
        configured_directory="$INSTALL_DIR/data"
    elif [[ "$configured_directory" != /* ]]; then
        configured_directory="$INSTALL_DIR/${configured_directory#./}"
    fi

    configured_directory=$(normalize_absolute_directory "$configured_directory") || return 1
    validate_compose_data_directory "$configured_directory" || return 1
    printf '%s\n' "$configured_directory"
}

resolve_compose_timescale_data_directory() {
    local configured_directory="${AETHER_TIMESCALE_DATA_PATH:-}"

    if [[ -z "$configured_directory" ]]; then
        configured_directory="$DATA_DIR/timescaledb/data"
    elif [[ "$configured_directory" != /* ]]; then
        configured_directory="$INSTALL_DIR/${configured_directory#./}"
    fi

    configured_directory=$(normalize_absolute_directory "$configured_directory") \
        || return 1
    validate_compose_data_directory "$configured_directory" || return 1
    printf '%s\n' "$configured_directory"
}

resolve_compose_log_directory() {
    local configured_directory="${AETHER_LOG_PATH:-${AETHER_LOG_DIR:-}}"

    if [[ -z "$configured_directory" ]]; then
        configured_directory="$INSTALL_DIR/logs"
    elif [[ "$configured_directory" != /* ]]; then
        configured_directory="$INSTALL_DIR/${configured_directory#./}"
    fi
    configured_directory=$(normalize_absolute_directory "$configured_directory") \
        || return 1
    validate_compose_data_directory "$configured_directory" || return 1
    printf '%s\n' "$configured_directory"
}

# Refresh the non-authoritative distribution template as a complete snapshot.
# Replacing the directory prevents removed rules or device definitions from an
# older release surviving into a later first-time activation.
stage_distribution_template() {
    local source_directory=$1
    local target_directory=$2
    local target_parent staging_directory previous_directory="" symlink_entry

    [[ -d "$source_directory" && ! -L "$source_directory" ]] || {
        echo "Configuration template not found: $source_directory" >&2
        return 1
    }
    symlink_entry=$($SUDO find "$source_directory" -type l -print -quit) || {
        echo "Unable to inspect configuration template: $source_directory" >&2
        return 1
    }
    if [[ -n "$symlink_entry" ]]; then
        echo "Configuration template contains symbolic links: $source_directory" >&2
        return 1
    fi
    require_absolute_directory "Staged configuration template" "$target_directory" \
        || return 1
    if $SUDO test -L "$target_directory"; then
        echo "Staged configuration path is a symlink: $target_directory" >&2
        return 1
    fi
    if $SUDO test -e "$target_directory" && ! $SUDO test -d "$target_directory"; then
        echo "Staged configuration path is not a directory: $target_directory" >&2
        return 1
    fi

    target_parent=$(dirname "$target_directory")
    $SUDO mkdir -p "$target_parent"
    staging_directory=$($SUDO mktemp -d "$target_parent/.config-template.tmp.XXXXXX")
    if ! $SUDO cp -R "$source_directory/." "$staging_directory/"; then
        $SUDO rm -rf "$staging_directory"
        return 1
    fi

    if $SUDO test -d "$target_directory"; then
        previous_directory=$($SUDO mktemp -d "$target_parent/.config-template.previous.XXXXXX")
        $SUDO rmdir "$previous_directory"
        $SUDO mv "$target_directory" "$previous_directory"
    fi
    if ! $SUDO mv "$staging_directory" "$target_directory"; then
        if [[ -n "$previous_directory" && -e "$previous_directory" ]]; then
            $SUDO mv "$previous_directory" "$target_directory" || true
        fi
        $SUDO rm -rf "$staging_directory"
        return 1
    fi
    if [[ -n "$previous_directory" && -e "$previous_directory" ]]; then
        $SUDO rm -rf "$previous_directory"
    fi
}

# Activate distribution defaults for a fresh site. Staging beside the
# destination keeps the final rename on one filesystem, so services can never
# observe a partially copied tree.
activate_initial_config() {
    local template_directory=$1
    local live_config_directory=$2
    local live_parent staging_directory

    [[ -d "$template_directory" ]] || {
        echo "Configuration template not found: $template_directory" >&2
        return 1
    }
    require_absolute_directory "Live configuration directory" "$live_config_directory" \
        || return 1

    if $SUDO test -e "$live_config_directory"; then
        $SUDO test -d "$live_config_directory" || {
            echo "Live configuration path is not a directory: $live_config_directory" >&2
            return 1
        }
        if [[ -n "$($SUDO find "$live_config_directory" -mindepth 1 -print -quit)" ]]; then
            echo "Fresh installation refused: live configuration already exists at $live_config_directory" >&2
            return 1
        fi
    fi

    live_parent=$(dirname "$live_config_directory")
    $SUDO mkdir -p "$live_parent"
    staging_directory=$($SUDO mktemp -d "$live_parent/.aether-config.tmp.XXXXXX")
    if ! $SUDO cp -R "$template_directory/." "$staging_directory/"; then
        $SUDO rm -rf "$staging_directory"
        return 1
    fi

    # An explicitly pre-created empty provisioning directory is safe to replace.
    if $SUDO test -d "$live_config_directory" \
        && ! $SUDO rmdir "$live_config_directory"; then
        echo "Live configuration became non-empty during activation" >&2
        $SUDO rm -rf "$staging_directory"
        return 1
    fi
    if ! $SUDO mv "$staging_directory" "$live_config_directory"; then
        $SUDO rm -rf "$staging_directory"
        return 1
    fi
    echo -e "${GREEN}✓ Safe configuration activated at $live_config_directory${NC}"
}

persist_install_context() {
    local mode=$1
    local config_directory=$2
    local data_directory=$3
    local runtime_directory=$4
    local context_directory context_temp
    local escaped_mode escaped_config escaped_data escaped_runtime

    require_absolute_directory "Install context config_dir" "$config_directory" || return 1
    require_absolute_directory "Install context data_dir" "$data_directory" || return 1
    require_absolute_directory "Install context runtime_dir" "$runtime_directory" || return 1

    escaped_mode=$(yaml_escape_double_quoted_string "$mode")
    escaped_config=$(yaml_escape_double_quoted_string "$config_directory")
    escaped_data=$(yaml_escape_double_quoted_string "$data_directory")
    escaped_runtime=$(yaml_escape_double_quoted_string "$runtime_directory")
    context_directory=$(dirname "$INSTALL_CONTEXT_FILE")

    $SUDO mkdir -p "$context_directory"
    context_temp=$($SUDO mktemp "$context_directory/install.yaml.tmp.XXXXXX")

    if $SUDO test -e "$INSTALL_CONTEXT_FILE" || $SUDO test -L "$INSTALL_CONTEXT_FILE"; then
        $SUDO rm -f "$context_temp"
        echo "Fresh installation refused: install context already exists: $INSTALL_CONTEXT_FILE" >&2
        return 1
    fi

    {
        printf 'mode: "%s"\n' "$escaped_mode"
        printf 'config_dir: "%s"\n' "$escaped_config"
        printf 'data_dir: "%s"\n' "$escaped_data"
        printf 'runtime_dir: "%s"\n' "$escaped_runtime"
        printf 'channel: stable\n'
        printf 'packs: []\n'
    } | $SUDO tee "$context_temp" >/dev/null
    $SUDO chmod 644 "$context_temp"
    # A hard link provides an atomic create-without-replace operation.
    if ! $SUDO ln "$context_temp" "$INSTALL_CONTEXT_FILE" 2>/dev/null; then
        $SUDO rm -f "$context_temp"
        echo "Failed to create fresh install context: $INSTALL_CONTEXT_FILE" >&2
        return 1
    fi
    $SUDO rm -f "$context_temp"
    echo -e "${GREEN}✓ Install context saved to $INSTALL_CONTEXT_FILE${NC}"
}

is_valid_jwt_secret() {
    local candidate=$1
    local normalized

    # Restrict persisted values to a Compose-safe alphabet. Generated secrets
    # are 64 hexadecimal characters; operators may also supply base64-like keys.
    [[ "$candidate" =~ ^[A-Za-z0-9._~+/=-]{32,}$ ]] || return 1
    normalized=$(printf '%s' "$candidate" | tr '[:upper:]' '[:lower:]')
    case "$normalized" in
        change-me*|changeme*|replace-me*|your-secret*|secret|0123456789abcdef0123456789abcdef)
            return 1
            ;;
    esac
}

# Compose intentionally has no insecure JWT fallback. Export a valid secret
# before any smart-update path can invoke Compose, and persist it for restarts.
ensure_compose_jwt_secret() {
    local env_file="$INSTALL_DIR/.env"
    local current=""
    local selected=""
    local temp_file=""

    if [[ -f "$env_file" ]]; then
        current=$($SUDO sed -n 's/^JWT_SECRET_KEY=//p' "$env_file" | tail -n 1 | tr -d '\r')
    fi

    if is_valid_jwt_secret "$current"; then
        export JWT_SECRET_KEY="$current"
        $SUDO chown "${ACTUAL_UID}:${ACTUAL_GID}" "$env_file"
        $SUDO chmod 600 "$env_file"
        return
    fi

    if is_valid_jwt_secret "${JWT_SECRET_KEY:-}"; then
        selected=$JWT_SECRET_KEY
    else
        selected=$(generate_jwt_secret)
    fi
    if ! is_valid_jwt_secret "$selected"; then
        echo -e "${RED}ERROR: failed to generate a secure JWT secret${NC}" >&2
        exit 1
    fi

    $SUDO mkdir -p "$INSTALL_DIR"
    temp_file=$($SUDO mktemp "$INSTALL_DIR/.env.tmp.XXXXXX")
    $SUDO chmod 600 "$temp_file"
    if [[ -f "$env_file" ]]; then
        $SUDO awk '
            !/^JWT_SECRET_KEY=/ &&
            !/^# JWT signing secret managed by install\.sh$/
        ' "$env_file" | $SUDO tee "$temp_file" >/dev/null
    fi
    printf '\n# JWT signing secret managed by install.sh\nJWT_SECRET_KEY=%s\n' \
        "$selected" | $SUDO tee -a "$temp_file" >/dev/null
    $SUDO chown "${ACTUAL_UID}:${ACTUAL_GID}" "$temp_file"
    $SUDO chmod 600 "$temp_file"
    $SUDO mv -f "$temp_file" "$env_file"
    $SUDO chmod 600 "$env_file"
    export JWT_SECRET_KEY="$selected"
    echo -e "${GREEN}✓ Stored a private JWT signing secret${NC}"
}

# Keep uplink device-control authority separate from user JWT signing. The
# credential is shared only by aether-uplink and aether-automation.
ensure_compose_uplink_control_token() {
    local env_file="$INSTALL_DIR/.env"
    local current=""
    local selected=""

    if [[ -f "$env_file" ]]; then
        current=$($SUDO sed -n 's/^AETHER_UPLINK_CONTROL_TOKEN=//p' "$env_file" | tail -n 1 | tr -d '\r')
    fi
    if is_valid_jwt_secret "$current" && [[ "$current" != "${JWT_SECRET_KEY:-}" ]]; then
        export AETHER_UPLINK_CONTROL_TOKEN="$current"
        return
    fi

    if is_valid_jwt_secret "${AETHER_UPLINK_CONTROL_TOKEN:-}" \
        && [[ "${AETHER_UPLINK_CONTROL_TOKEN:-}" != "${JWT_SECRET_KEY:-}" ]]; then
        selected=${AETHER_UPLINK_CONTROL_TOKEN:-}
    else
        selected=$(generate_jwt_secret)
    fi
    if ! is_valid_jwt_secret "$selected" || [[ "$selected" == "${JWT_SECRET_KEY:-}" ]]; then
        echo -e "${RED}ERROR: failed to generate a distinct uplink control credential${NC}" >&2
        exit 1
    fi

    persist_compose_env_value "$env_file" AETHER_UPLINK_CONTROL_TOKEN "$selected"
    export AETHER_UPLINK_CONTROL_TOKEN="$selected"
    echo -e "${GREEN}✓ Stored a private uplink control credential${NC}"
}

# =============================================================================
# Smart Update Helper Functions
# =============================================================================

# Tarball filename to image name mapping
declare -A TARBALL_TO_IMAGE=(
    ["aetherems.tar.gz"]="aetherems:latest"
    ["aether-redis.tar.gz"]="redis:8-alpine"
    ["aether-timescaledb.tar.gz"]="timescale/timescaledb:2.25.2-pg17"
    ["apps.tar.gz"]="aether-apps:latest"
)

# Per-run rollback state. Pre-existing dependency image IDs are tagged before a
# package tarball can replace their public tag and retained until this fresh
# installation is healthy.
declare -A IMAGE_BACKUP_TAGS=()
declare -A IMAGE_PREVIOUS_IDS=()
declare -A FRESH_IMAGE_TAGS=()
INSTALL_TRANSACTION_ACTIVE=false
INSTALL_TRANSACTION_COMMITTED=false
CONTAINER_STATE_MUTATED=false
COMPOSE_BACKUP_PATH=""
COMPOSE_WAS_PRESENT=false
COMPOSE_PUBLISHED=false
DATABASE_BACKUP_DIR=""
RUNTIME_SNAPSHOT_COMPLETE=false
CLI_SNAPSHOT_CAPTURED=false
CLI_WAS_PRESENT=false

# Generate backup tag for an image
# Example: redis:8-alpine -> redis:backup-8-alpine-1703260800
generate_backup_tag() {
    local image=$1
    local timestamp=${BACKUP_TIMESTAMP:-$(date +%s)}
    local name="${image%:*}"      # redis, aetherems
    local tag="${image##*:}"      # 8-alpine, latest
    echo "${name}:backup-${tag}-${timestamp}"
}

# =============================================================================
# Smart Image Loading - Skip unchanged images
# =============================================================================

# Extract image ID from tar.gz without loading
# Docker save format: manifest.json contains Config field with image sha256
get_tarball_image_id() {
    local tarball=$1

    # Extract manifest.json and get the Config (image ID)
    # Support both docker save and skopeo formats
    # Use timeout to prevent hanging (30 seconds max)
    local raw_config
    raw_config=$(timeout 30 sh -c "zcat '$tarball' 2>/dev/null | tar -xOf - manifest.json 2>/dev/null | \
        sed -n 's/.*\"Config\":\"\([^\"]*\)\".*/\1/p' | head -1" 2>/dev/null) || return

    if [[ -z "$raw_config" ]]; then
        return
    fi

    # Clean up the path (skopeo uses blobs/sha256/HASH, docker uses HASH.json or sha256:HASH)
    local clean_hash
    clean_hash=$(basename "$raw_config" | sed 's/\.json$//' | sed 's/sha256://')

    # Return first 12 chars
    echo "${clean_hash:0:12}"
}

# Get local image ID by image name
get_local_image_id() {
    local image=$1
    local result
    result=$(docker images "$image" --format '{{.ID}}' 2>/dev/null | head -1)
    echo "$result"
}

get_full_local_image_id() {
    local image=$1
    docker image inspect "$image" --format '{{.Id}}' 2>/dev/null | head -1
}

backup_image_before_load() {
    local image=$1
    local old_id

    [[ -z "${IMAGE_BACKUP_TAGS[$image]:-}" ]] || return 0
    old_id=$(get_full_local_image_id "$image")
    [[ -n "$old_id" ]] || return 0

    preserve_image_id_for_rollback "$image" "$old_id"
}

preserve_image_id_for_rollback() {
    local image=$1
    local old_id=$2
    local backup_tag

    if [[ -n "${IMAGE_PREVIOUS_IDS[$image]:-}" ]]; then
        if [[ "${IMAGE_PREVIOUS_IDS[$image]}" != "$old_id" ]]; then
            echo "Image rollback identity changed during installation: $image" >&2
            return 1
        fi
        return 0
    fi

    backup_tag=$(generate_backup_tag "$image")
    echo "  Preserving $image ($old_id) as $backup_tag"
    docker tag "$old_id" "$backup_tag" || {
        echo "Unable to preserve the existing image before load: $image" >&2
        return 1
    }
    IMAGE_BACKUP_TAGS["$image"]=$backup_tag
    IMAGE_PREVIOUS_IDS["$image"]=$old_id
}

restore_image_tag_from_backup() {
    local image=$1
    local backup_tag=${IMAGE_BACKUP_TAGS[$image]:-}
    local expected_id=${IMAGE_PREVIOUS_IDS[$image]:-}
    local restored_id

    [[ -n "$backup_tag" && -n "$expected_id" ]] || return 0
    docker tag "$backup_tag" "$image" || return 1
    restored_id=$(get_full_local_image_id "$image")
    [[ "$restored_id" == "$expected_id" ]]
}

# Smart load: only load if image has changed
# Special handling for multi-arch images (timescaledb, redis)
# Returns: 0 if loaded, 1 if skipped (unchanged), 2 if error
smart_load_image() {
    local tarball=$1
    local basename
    basename=$(basename "$tarball")
    local image="${TARBALL_TO_IMAGE[$basename]:-}"

    # Refuse untracked package images. Without an expected public tag we cannot
    # preserve and verify the pre-install image for rollback.
    if [[ -z "$image" ]]; then
        echo -e "  ${RED}Refusing $basename: no rollback image mapping${NC}" >&2
        return 2
    fi

    # Get tarball image ID (with timeout protection). A missing ID disables the
    # skip optimization, never the pre-load backup.
    local tarball_id
    tarball_id=$(get_tarball_image_id "$tarball")

    # Get local image ID
    local local_id
    local_id=$(get_local_image_id "$image")

    # Aether-owned image tags are part of the fresh-runtime footprint. A tag
    # appearing after preflight is a TOCTOU conflict, never an upgrade target.
    case "$image" in
        aetherems:latest|aether-apps:latest)
            if [[ -n "$local_id" ]]; then
                echo -e "  ${RED}Refusing $basename: Aether image tag appeared after fresh preflight${NC}" >&2
                return 2
            fi
            ;;
    esac

    # Compare IDs
    if [[ -n "$tarball_id" && -n "$local_id" && "$tarball_id" == "$local_id" ]]; then
        echo -e "  ${GREEN}✓${NC} $basename: ${GREEN}unchanged${NC} (skipped)"
        return 1
    fi

    # IDs differ (or the tarball could not be inspected). Preserve the exact
    # old image ID before removing its public tag.
    if [[ -z "$local_id" ]]; then
        echo -n "  Loading $basename (new)... "
    else
        echo -n "  Loading $basename (${local_id:0:12} → ${tarball_id:-unknown})... "
        backup_image_before_load "$image" || return 2
        if ! docker rmi "$image" >/dev/null 2>&1; then
            echo -e "${RED}failed to release old tag${NC}"
            return 2
        fi
    fi

    # Use timeout to prevent hanging (5 minutes max for docker load)
    if timeout 300 bash -c 'gunzip -c "$1" | docker load' _ "$tarball" >/dev/null 2>&1; then
        local loaded_id
        loaded_id=$(get_local_image_id "$image")
        if [[ -z "$loaded_id" \
            || ( -n "$tarball_id" && "$loaded_id" != "$tarball_id" ) ]]; then
            echo -e "${RED}loaded tag does not match package${NC}"
            restore_image_tag_from_backup "$image" || true
            return 2
        fi
        if [[ -z "$local_id" ]]; then
            FRESH_IMAGE_TAGS["$image"]=true
        fi
        echo -e "${GREEN}done${NC}"
        return 0
    else
        echo -e "${RED}failed (timeout or error)${NC}"
        restore_image_tag_from_backup "$image" || true
        return 2
    fi
}

# =============================================================================
# End of Smart Update Helper Functions
# =============================================================================

resolve_install_user() {
    local effective_uid=$1
    local explicit_user=$2
    local sudo_user=$3
    local current_user=$4
    local candidate=""
    local candidate_uid

    if [[ -n "$explicit_user" ]]; then
        candidate=$explicit_user
    elif [[ "$effective_uid" == 0 ]]; then
        candidate=$sudo_user
    else
        candidate=$current_user
    fi

    if [[ -z "$candidate" ]] || ! candidate_uid=$(id -u "$candidate" 2>/dev/null) \
        || [[ "$candidate_uid" == 0 ]]; then
        echo "A non-root host identity is required for Aether containers. Use sudo from an ordinary account or set AETHER_INSTALL_USER." >&2
        return 1
    fi
    printf '%s\n' "$candidate"
}

# Determine which non-root host user should own installed files and run the
# externally reachable API container. `sudo ./installer.run` preserves the
# invoking account in SUDO_USER; direct root automation must choose explicitly.
determine_install_user() {
    local current_user
    current_user=$(id -un)
    ACTUAL_USER=$(resolve_install_user \
        "$EUID" "${AETHER_INSTALL_USER:-}" "${SUDO_USER:-}" "$current_user")
    ACTUAL_UID=$(id -u "$ACTUAL_USER")
    ACTUAL_GID=$(id -g "$ACTUAL_USER")
}

known_aether_containers() {
    printf '%s\n' \
        aether-io \
        aether-automation \
        aether-history \
        aether-api \
        aether-uplink \
        aether-alarm \
        aether-redis \
        aether-timescaledb \
        aether-apps
}

docker_container_state() {
    local container=$1
    local state

    state=$(docker inspect "$container" --format '{{.State.Status}}' 2>/dev/null) || return 1
    case "$state" in
        created|running|paused|restarting|removing|exited|dead)
            printf '%s\n' "$state"
            ;;
        *)
            # Compatibility with older/mock Docker clients that do not honor
            # the state format. Never treat absence from `docker ps` as quiet.
            if docker ps --filter "name=^${container}$" --filter status=running -q \
                | grep -q .; then
                printf 'running\n'
            else
                return 1
            fi
            ;;
    esac
}

quiesce_docker_services() {
    local container state
    local failed=false

    while IFS= read -r container; do
        state=$(docker_container_state "$container") || continue
        case "$state" in
            paused)
                if ! docker unpause "$container" >/dev/null \
                    || ! docker stop "$container" >/dev/null; then
                    failed=true
                fi
                ;;
            running|restarting)
                if ! docker stop "$container" >/dev/null; then
                    failed=true
                fi
                ;;
            created|exited|dead) ;;
            *)
                echo "Refusing to mutate $container in Docker state '$state'" >&2
                failed=true
                ;;
        esac
    done < <(known_aether_containers)
    while IFS= read -r container; do
        state=$(docker_container_state "$container") || continue
        case "$state" in
            created|exited|dead) ;;
            *)
                echo "Unable to quiesce $container safely (state=$state)" >&2
                failed=true
                ;;
        esac
    done < <(known_aether_containers)
    [[ "$failed" == false ]]
}

snapshot_runtime_data_for_rollback() {
    local asset
    local assets=(
        aether.db
        aether.db-wal
        aether.db-shm
        aether.db-journal
        aether-history.db
        aether-history.db-wal
        aether-history.db-shm
        aether-history.db-journal
        uplink.outbox
    )

    RUNTIME_SNAPSHOT_COMPLETE=false
    DATABASE_BACKUP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aether-install-data.XXXXXX")
    chmod 700 "$DATABASE_BACKUP_DIR"
    snapshot_runtime_directory "$INSTALL_DIR" install-root || return 1
    snapshot_runtime_directory "$DATA_DIR" data-root || return 1
    snapshot_runtime_directory "$LOG_DIR" log-root || return 1
    for asset in "${assets[@]}"; do
        snapshot_runtime_regular_file "$DATA_DIR/$asset" "data-$asset" || return 1
    done
    snapshot_runtime_directory "$DATA_DIR/config" data-config || return 1
    snapshot_runtime_directory "$DATA_DIR/cert" data-cert || return 1
    snapshot_runtime_regular_file "$INSTALL_DIR/.env" install-env || return 1
    snapshot_runtime_regular_file \
        "$INSTALL_CONTEXT_FILE" install-context || return 1
    snapshot_runtime_regular_file \
        "$PROFILE_ENTRY" profile-entry || return 1
    snapshot_runtime_regular_file \
        "$SHM_PATH" shared-memory-authority || return 1
    snapshot_runtime_directory \
        "$INSTALL_DIR/config.template" install-config-template || return 1
    if [[ -n "$TIMESCALE_DATA_DIR" ]]; then
        snapshot_runtime_directory \
            "$TIMESCALE_DATA_DIR" timescale-data || return 1
    fi
    RUNTIME_SNAPSHOT_COMPLETE=true
}

restore_runtime_data_from_backup() {
    local asset
    local assets=(
        aether.db
        aether.db-wal
        aether.db-shm
        aether.db-journal
        aether-history.db
        aether-history.db-wal
        aether-history.db-shm
        aether-history.db-journal
        uplink.outbox
    )

    [[ "$RUNTIME_SNAPSHOT_COMPLETE" == true ]] || return 0
    [[ -n "$DATABASE_BACKUP_DIR" && -d "$DATABASE_BACKUP_DIR" ]] || return 1
    for asset in "${assets[@]}"; do
        restore_runtime_path "$DATA_DIR/$asset" "data-$asset" || return 1
    done
    restore_runtime_path "$DATA_DIR/config" data-config || return 1
    restore_runtime_path "$DATA_DIR/cert" data-cert || return 1
    restore_runtime_path "$INSTALL_DIR/.env" install-env || return 1
    restore_runtime_path "$INSTALL_CONTEXT_FILE" install-context || return 1
    restore_runtime_path "$PROFILE_ENTRY" profile-entry || return 1
    restore_runtime_path \
        "$SHM_PATH" shared-memory-authority || return 1
    restore_runtime_path \
        "$INSTALL_DIR/config.template" install-config-template || return 1
    if [[ -n "$TIMESCALE_DATA_DIR" ]]; then
        restore_runtime_path "$TIMESCALE_DATA_DIR" timescale-data || return 1
    fi
    # Root snapshots are restored last so every directory, symlink, permission,
    # and auxiliary file created by this failed fresh install is removed while
    # pre-existing empty provisioning roots are recreated exactly.
    restore_runtime_path "$LOG_DIR" log-root || return 1
    restore_runtime_path "$DATA_DIR" data-root || return 1
    restore_runtime_path "$INSTALL_DIR" install-root || return 1
}

snapshot_installed_cli_for_rollback() {
    local cli_path=/usr/local/bin/aether

    CLI_SNAPSHOT_CAPTURED=false
    CLI_WAS_PRESENT=false
    if [[ -L "$cli_path" || ( -e "$cli_path" && ! -f "$cli_path" ) ]]; then
        echo "Refusing unsafe installed CLI target: $cli_path" >&2
        return 1
    fi
    if [[ -f "$cli_path" ]]; then
        if ! $SUDO cp -a "$cli_path" "$DATABASE_BACKUP_DIR/installed-cli.payload"; then
            return 1
        fi
        CLI_WAS_PRESENT=true
    fi
    CLI_SNAPSHOT_CAPTURED=true
}

restore_installed_cli_from_backup() {
    local cli_path=/usr/local/bin/aether
    local staged_path

    [[ "$CLI_SNAPSHOT_CAPTURED" == true ]] || return 0
    if [[ -L "$cli_path" || ( -e "$cli_path" && ! -f "$cli_path" ) ]]; then
        echo "Refusing unsafe installed CLI rollback target: $cli_path" >&2
        return 1
    fi
    if [[ "$CLI_WAS_PRESENT" == true ]]; then
        [[ -f "$DATABASE_BACKUP_DIR/installed-cli.payload" ]] || return 1
        staged_path=$($SUDO mktemp /usr/local/bin/.aether.restore.XXXXXX) || return 1
        if ! $SUDO cp -a "$DATABASE_BACKUP_DIR/installed-cli.payload" "$staged_path" \
            || ! $SUDO chmod 755 "$staged_path" \
            || ! $SUDO mv -f "$staged_path" "$cli_path"; then
            $SUDO rm -f "$staged_path" || true
            return 1
        fi
    else
        $SUDO rm -f "$cli_path" || return 1
    fi
}

snapshot_runtime_path() {
    local source_path=$1
    local key=$2

    if $SUDO test -L "$source_path"; then
        echo "Refusing to snapshot a symlinked persistent asset: $source_path" >&2
        return 1
    fi
    if $SUDO test -e "$source_path"; then
        touch "$DATABASE_BACKUP_DIR/$key.present"
        if ! $SUDO cp -a "$source_path" "$DATABASE_BACKUP_DIR/$key.payload"; then
            echo "Failed to snapshot persistent asset: $source_path" >&2
            return 1
        fi
    else
        touch "$DATABASE_BACKUP_DIR/$key.absent"
    fi
}

snapshot_runtime_regular_file() {
    local source_path=$1
    local key=$2

    touch "$DATABASE_BACKUP_DIR/$key.expected-file"
    if { $SUDO test -e "$source_path" || $SUDO test -L "$source_path"; } \
        && { ! $SUDO test -f "$source_path" || $SUDO test -L "$source_path"; }; then
        echo "Expected a regular persistent file: $source_path" >&2
        return 1
    fi
    snapshot_runtime_path "$source_path" "$key"
}

snapshot_runtime_directory() {
    local source_path=$1
    local key=$2

    touch "$DATABASE_BACKUP_DIR/$key.expected-directory"
    if { $SUDO test -e "$source_path" || $SUDO test -L "$source_path"; } \
        && { ! $SUDO test -d "$source_path" || $SUDO test -L "$source_path"; }; then
        echo "Expected a persistent directory: $source_path" >&2
        return 1
    fi
    snapshot_runtime_path "$source_path" "$key"
}

restore_runtime_path() {
    local target_path=$1
    local key=$2
    local parent_path staged_path

    if $SUDO test -L "$target_path"; then
        echo "Refusing to restore over a symlinked persistent asset: $target_path" >&2
        return 1
    fi
    if $SUDO test -e "$target_path" \
        && [[ -f "$DATABASE_BACKUP_DIR/$key.expected-file" ]] \
        && ! $SUDO test -f "$target_path"; then
        echo "Refusing to restore a file over a non-file path: $target_path" >&2
        return 1
    fi
    if $SUDO test -e "$target_path" \
        && [[ -f "$DATABASE_BACKUP_DIR/$key.expected-directory" ]] \
        && ! $SUDO test -d "$target_path"; then
        echo "Refusing to restore a directory over a non-directory path: $target_path" >&2
        return 1
    fi

    if [[ -f "$DATABASE_BACKUP_DIR/$key.present" ]]; then
        parent_path=$(dirname "$target_path")
        $SUDO mkdir -p "$parent_path" || return 1
        staged_path=$($SUDO mktemp -d "$parent_path/.aether-restore.XXXXXX") || return 1
        $SUDO rmdir "$staged_path" || return 1
        if ! $SUDO cp -a "$DATABASE_BACKUP_DIR/$key.payload" "$staged_path"; then
            $SUDO rm -rf -- "$staged_path" || true
            echo "Failed to stage restored persistent asset: $target_path" >&2
            return 1
        fi
        if [[ -f "$DATABASE_BACKUP_DIR/$key.expected-directory" ]]; then
            $SUDO rm -rf -- "$target_path" || return 1
        else
            $SUDO rm -f -- "$target_path" || return 1
        fi
        if ! $SUDO mv "$staged_path" "$target_path"; then
            echo "Failed to publish restored persistent asset: $target_path" >&2
            return 1
        fi
    elif [[ -f "$DATABASE_BACKUP_DIR/$key.expected-directory" ]]; then
        $SUDO rm -rf -- "$target_path" || return 1
    else
        $SUDO rm -f -- "$target_path" || return 1
    fi
}

publish_compose_atomically() {
    local source_file=$1
    local target_file="$INSTALL_DIR/docker-compose.yml"
    local staged_file

    [[ -f "$source_file" && ! -L "$source_file" ]] || {
        echo "Packaged docker-compose.yml is missing, non-regular, or symlinked" >&2
        return 1
    }
    if [[ ( -e "$target_file" || -L "$target_file" ) \
        && ( ! -f "$target_file" || -L "$target_file" ) ]]; then
        echo "Refusing unsafe Compose publication target: $target_file" >&2
        return 1
    fi
    $SUDO mkdir -p "$INSTALL_DIR"
    staged_file=$($SUDO mktemp "$INSTALL_DIR/.docker-compose.new.XXXXXX")
    $SUDO cp "$source_file" "$staged_file"
    $SUDO chmod 644 "$staged_file"
    if ! (cd "$INSTALL_DIR" && run_docker_compose -f "$staged_file" config >/dev/null); then
        $SUDO rm -f "$staged_file"
        echo "Packaged docker-compose.yml failed validation" >&2
        return 1
    fi

    if [[ -e "$target_file" || -L "$target_file" ]]; then
        if [[ ! -f "$target_file" || -L "$target_file" ]]; then
            $SUDO rm -f "$staged_file" || true
            echo "Compose target changed to an unsafe file during publication" >&2
            return 1
        fi
        COMPOSE_WAS_PRESENT=true
        COMPOSE_BACKUP_PATH=$($SUDO mktemp "$INSTALL_DIR/.docker-compose.backup.XXXXXX")
        $SUDO cp -a "$target_file" "$COMPOSE_BACKUP_PATH" || return 1
    fi
    $SUDO mv -f "$staged_file" "$target_file"
    COMPOSE_PUBLISHED=true
    $SUDO chown "$ACTUAL_USER:docker" "$target_file" 2>/dev/null || true
}

restore_compose_from_backup() {
    local target_file="$INSTALL_DIR/docker-compose.yml"
    local staged_file

    if [[ ( -e "$target_file" || -L "$target_file" ) \
        && ( ! -f "$target_file" || -L "$target_file" ) ]]; then
        echo "Refusing unsafe Compose rollback target: $target_file" >&2
        return 1
    fi

    if [[ "$COMPOSE_WAS_PRESENT" == true ]]; then
        [[ -n "$COMPOSE_BACKUP_PATH" && -f "$COMPOSE_BACKUP_PATH" ]] || return 1
        staged_file=$($SUDO mktemp "$INSTALL_DIR/.docker-compose.restore.XXXXXX") \
            || return 1
        if ! $SUDO cp -a "$COMPOSE_BACKUP_PATH" "$staged_file" \
            || ! $SUDO cmp -s "$COMPOSE_BACKUP_PATH" "$staged_file" \
            || ! $SUDO mv -f "$staged_file" "$target_file"; then
            $SUDO rm -f "$staged_file" || true
            return 1
        fi
    else
        $SUDO rm -f "$target_file" || return 1
    fi
}

cleanup_install_transaction_backups() {
    local image backup_tag

    for image in "${!IMAGE_BACKUP_TAGS[@]}"; do
        backup_tag=${IMAGE_BACKUP_TAGS[$image]}
        docker rmi "$backup_tag" >/dev/null 2>&1 || true
    done
    if [[ -n "$COMPOSE_BACKUP_PATH" ]]; then
        $SUDO rm -f "$COMPOSE_BACKUP_PATH"
    fi
    if [[ -n "$DATABASE_BACKUP_DIR" ]]; then
        $SUDO rm -rf "$DATABASE_BACKUP_DIR"
    fi
}

rollback_docker_install() {
    local rollback_ok=true
    local image

    echo "Fresh installation failed; removing its partial Docker runtime..." >&2
    if [[ "$CONTAINER_STATE_MUTATED" == true ]] && ! quiesce_docker_services; then
        rollback_ok=false
    fi
    if [[ "$rollback_ok" == true && "$CONTAINER_STATE_MUTATED" == true ]]; then
        while IFS= read -r container; do
            if docker inspect "$container" >/dev/null 2>&1 \
                && ! docker rm -f "$container" >/dev/null; then
                rollback_ok=false
            fi
        done < <(known_aether_containers)
    fi
    if [[ "$rollback_ok" == true && "$CONTAINER_STATE_MUTATED" == true ]] \
        && ! remove_fresh_redis_volumes; then
        rollback_ok=false
    fi
    if [[ "$rollback_ok" == true ]] && ! restore_runtime_data_from_backup; then
        rollback_ok=false
    fi
    if [[ "$rollback_ok" == true ]] && ! restore_installed_cli_from_backup; then
        rollback_ok=false
    fi
    if [[ "$rollback_ok" == true && "$COMPOSE_PUBLISHED" == true ]] \
        && ! restore_compose_from_backup; then
        rollback_ok=false
    fi
    if [[ "$rollback_ok" == true ]]; then
        for image in "${!IMAGE_BACKUP_TAGS[@]}"; do
            if ! restore_image_tag_from_backup "$image"; then
                rollback_ok=false
            fi
        done
    fi
    if [[ "$rollback_ok" == true ]]; then
        for image in "${!FRESH_IMAGE_TAGS[@]}"; do
            if ! docker rmi "$image" >/dev/null 2>&1; then
                rollback_ok=false
            fi
        done
    fi
    if [[ "$rollback_ok" == true ]]; then
        cleanup_install_transaction_backups
        echo "Partial containers, images, Compose state, and runtime data were removed." >&2
    else
        echo "Automatic rollback is incomplete. Backups were preserved; manual recovery is required." >&2
    fi
    [[ "$rollback_ok" == true ]]
}

docker_install_exit() {
    local status=$?

    trap - EXIT
    if [[ "$status" -ne 0 && "$INSTALL_TRANSACTION_ACTIVE" == true \
        && "$INSTALL_TRANSACTION_COMMITTED" != true ]]; then
        rollback_docker_install || true
    elif [[ "$INSTALL_TRANSACTION_COMMITTED" == true ]]; then
        cleanup_install_transaction_backups
    fi
    exit "$status"
}

all_containers_ready() {
    local containers=$1
    local container state health

    for container in $containers; do
        state=$(docker_container_state "$container") || return 1
        [[ "$state" == running ]] || return 1
        health=$(docker inspect "$container" \
            --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
            2>/dev/null) || return 1
        case "$health" in
            none|healthy) ;;
            *) return 1 ;;
        esac
    done
}

wait_for_installed_stack() {
    local containers=$1
    local attempts=${2:-30}
    local stable=0
    local attempt

    if [[ ! -x /usr/local/bin/aether ]]; then
        echo "The required installed Aether CLI is unavailable for the health gate" >&2
        return 1
    fi

    for ((attempt = 1; attempt <= attempts; attempt++)); do
        if all_containers_ready "$containers"; then
            if ! /usr/local/bin/aether --config-path "$LIVE_CONFIG_DIR" \
                --db-path "$DATA_DIR" doctor --json >/dev/null 2>&1; then
                stable=0
            else
                stable=$((stable + 1))
                if [[ "$stable" -ge 3 ]]; then
                    return 0
                fi
            fi
        else
            stable=0
        fi
        sleep 2
    done
    echo "The installed stack did not remain healthy during the commit window" >&2
    return 1
}

if [[ "${AETHER_INSTALLER_FUNCTIONS_ONLY:-false}" == true ]]; then
    return 0 2>/dev/null || exit 0
fi

validate_docker_install_dir "$INSTALL_DIR"

echo -e "${BLUE}================================${NC}"
echo -e "${BLUE}  AetherEMS ${INSTALLER_ARCH_LABEL} Installer   ${NC}"
echo -e "${BLUE}================================${NC}"
echo ""

# Check architecture
ARCH=$(uname -m)
if [[ "$ARCH" != "$INSTALLER_ARCH_UNAME" && "$ARCH" != "$INSTALLER_ARCH_SHORT" ]]; then
    echo -e "${YELLOW}Warning: This installer is for ${INSTALLER_ARCH_LABEL}. Current arch: $ARCH${NC}"
    # Verify aether binary can actually execute on this architecture
    if [[ -f "tools/aether" ]] && ! ./tools/aether --version &>/dev/null; then
        echo -e "${RED}Error: aether binary cannot execute on $ARCH (built for ${INSTALLER_ARCH_LABEL})${NC}"
        echo -e "${RED}Use the correct architecture installer for this machine.${NC}"
        exit 1
    fi
    if [[ "$AUTO_MODE" == true ]]; then
        echo -e "${YELLOW}Auto mode: continuing despite architecture mismatch${NC}"
    else
        read -p "Continue anyway? (y/N): " -n 1 -r
        echo
        [[ ! $REPLY =~ ^[Yy]$ ]] && exit 1
    fi
fi

# Resolve installation user details once up front so later steps can safely
# reference ACTUAL_USER/UID/GID without triggering `set -u` exits.
determine_install_user

# Check if we have sudo access (will be needed for some operations)
SUDO=""
if [[ $EUID -ne 0 ]]; then
    echo -e "${YELLOW}Note: Some operations will require sudo privileges${NC}"
    SUDO="sudo"
    # Test sudo access
    if ! sudo -n true 2>/dev/null; then
        echo "Please enter your password for sudo access:"
        sudo true || {
            echo -e "${RED}Error: sudo access required for installation${NC}"
            exit 1
        }
    fi
fi

require_absolute_directory "AETHER_INSTALL_DIR" "$INSTALL_DIR"
reject_existing_docker_filesystem_footprint
DATA_DIR=$(resolve_compose_data_directory)
LIVE_CONFIG_DIR="$DATA_DIR/config"
LOG_DIR_EXPLICIT=false
if [[ -n "${AETHER_LOG_PATH:-${AETHER_LOG_DIR:-}}" ]]; then
    LOG_DIR_EXPLICIT=true
fi
LOG_DIR=$(resolve_compose_log_directory)
# Resolve the automatic external log root during preflight so every privileged
# write root is known and proven empty before images, containers, or host state
# can be changed.
if [[ "$LOG_DIR_EXPLICIT" != true ]] \
    && is_safe_external_storage_root "/extp" \
    && validate_compose_data_directory "/extp/logs" \
    && [[ -w "/extp" || -n "$SUDO" ]]; then
    LOG_DIR="/extp/logs"
fi
require_empty_or_absent_directory "$DATA_DIR" "Aether data root"
require_empty_or_absent_directory "$LOG_DIR" "Aether log root"

# A fresh runtime package is self-contained: CLI, Compose, safe template, and
# the core image are all mandatory. Extension-only packages are unsupported.
if [[ ! -f "tools/aether" || -L "tools/aether" || ! -x "tools/aether" ]]; then
    echo -e "${RED}Error: package is missing the required executable tools/aether${NC}" >&2
    exit 1
fi
if ! ./tools/aether --version >/dev/null 2>&1; then
    echo -e "${RED}Error: packaged Aether CLI cannot execute on this host${NC}" >&2
    exit 1
fi
if [[ ! -f config.template/runtime-manifest.json \
    || -L config.template/runtime-manifest.json ]]; then
    echo -e "${RED}Error: package is missing a regular runtime manifest${NC}" >&2
    exit 1
fi
if ! ./tools/aether --json runtime-manifest \
    --path config.template/runtime-manifest.json >/dev/null; then
    echo -e "${RED}Error: packaged runtime manifest failed verification${NC}" >&2
    exit 1
fi
if [[ ! -f docker-compose.yml || -L docker-compose.yml ]]; then
    echo -e "${RED}Error: package is missing a regular docker-compose.yml${NC}" >&2
    exit 1
fi
if [[ ! -d config.template || -L config.template ]]; then
    echo -e "${RED}Error: packaged config.template is missing or contains symlinks${NC}" >&2
    exit 1
fi
PACKAGED_TEMPLATE_SYMLINK=$(find config.template -type l -print -quit) || {
    echo -e "${RED}Error: packaged config.template cannot be inspected safely${NC}" >&2
    exit 1
}
if [[ -n "$PACKAGED_TEMPLATE_SYMLINK" ]]; then
    echo -e "${RED}Error: packaged config.template contains symlinks${NC}" >&2
    exit 1
fi
for package_tarball in \
    docker/aetherems.tar.gz \
    docker/aether-redis.tar.gz \
    docker/aether-timescaledb.tar.gz \
    docker/apps.tar.gz; do
    if [[ -L "$package_tarball" \
        || ( -e "$package_tarball" && ! -f "$package_tarball" ) ]]; then
        echo "Refusing non-regular or symlinked image tarball: $package_tarball" >&2
        exit 1
    fi
done
if ! command -v docker >/dev/null 2>&1; then
    echo -e "${RED}Docker not installed. Please install Docker first.${NC}" >&2
    exit 1
fi
detect_docker_compose_cmd >/dev/null
if [[ -f "docker/aether-redis.tar.gz" ]]; then
    REDIS_EXTENSION_SELECTED=true
fi
reject_existing_docker_runtime_footprint
reject_existing_redis_volume_footprint

if [[ ! -f "docker/aetherems.tar.gz" ]]; then
    echo "Fresh installation requires the packaged core image docker/aetherems.tar.gz" >&2
    exit 1
fi

TIMESCALE_DATA_DIR=""
if [[ -f "docker/aether-timescaledb.tar.gz" ]]; then
    TIMESCALE_DATA_DIR=$(resolve_compose_timescale_data_directory)
    require_empty_or_absent_directory \
        "$TIMESCALE_DATA_DIR" "TimescaleDB extension data root"
fi

# Arm rollback before capturing data or changing persistent secrets, the
# installed CLI, image tags, or configuration.
BACKUP_TIMESTAMP="$(date +%s)-$$"
INSTALL_TRANSACTION_ACTIVE=true
trap docker_install_exit EXIT
reject_existing_docker_runtime_footprint
reject_existing_redis_volume_footprint
require_empty_or_absent_directory "$DATA_DIR" "Aether data root"
require_empty_or_absent_directory "$LOG_DIR" "Aether log root"
if [[ -n "$TIMESCALE_DATA_DIR" ]]; then
    require_empty_or_absent_directory \
        "$TIMESCALE_DATA_DIR" "TimescaleDB extension data root"
fi
snapshot_runtime_data_for_rollback
snapshot_installed_cli_for_rollback

# Secrets are part of the fresh-install snapshot and are removed on failure.
ensure_compose_jwt_secret
ensure_compose_uplink_control_token
ensure_compose_bootstrap_admin "$INSTALL_DIR/.env" "$DATA_DIR/aether.db"
if [[ -f "docker/aether-timescaledb.tar.gz" ]]; then
    ensure_compose_timescaledb_password "$INSTALL_DIR/.env"
fi

# Step 1: atomically publish the required CLI inside the same transaction.
echo -e "${YELLOW}[1/3] Installing CLI tools...${NC}"
$SUDO mkdir -p /usr/local/bin
CLI_STAGE=$($SUDO mktemp /usr/local/bin/.aether.new.XXXXXX)
if ! $SUDO cp "tools/aether" "$CLI_STAGE" \
    || ! $SUDO chmod 755 "$CLI_STAGE" \
    || ! $SUDO "$CLI_STAGE" --version >/dev/null 2>&1 \
    || ! $SUDO mv -f "$CLI_STAGE" /usr/local/bin/aether; then
    $SUDO rm -f "$CLI_STAGE" || true
    echo "Failed to atomically publish the packaged Aether CLI" >&2
    exit 1
fi
echo -e "${GREEN}✓ Aether CLI installed${NC}"

# Step 2: pure image staging. A tarball's presence is the explicit extension
# selection; there is no second prompt whose answer could diverge from commit.
echo -e "${YELLOW}[2/3] Staging Docker images...${NC}"
STAGED_IMAGE_COUNT=0
SKIPPED_IMAGE_COUNT=0
tarball_list=(
    docker/aetherems.tar.gz
    docker/aether-redis.tar.gz
    docker/aether-timescaledb.tar.gz
    docker/apps.tar.gz
)
for tarball in "${tarball_list[@]}"; do
    if [[ -L "$tarball" || ( -e "$tarball" && ! -f "$tarball" ) ]]; then
        echo "Refusing non-regular or symlinked image tarball: $tarball" >&2
        exit 1
    fi
    [[ -f "$tarball" ]] || continue
    if smart_load_image "$tarball"; then
        STAGED_IMAGE_COUNT=$((STAGED_IMAGE_COUNT + 1))
    else
        load_status=$?
        case "$load_status" in
            1) SKIPPED_IMAGE_COUNT=$((SKIPPED_IMAGE_COUNT + 1)) ;;
            *)
                echo -e "${RED}Failed to stage $(basename "$tarball")${NC}" >&2
                exit 1
                ;;
        esac
    fi
done
if ! docker image inspect aetherems:latest >/dev/null 2>&1; then
    echo "The package omitted docker/aetherems.tar.gz and no local core image exists" >&2
    exit 1
fi
echo -e "${GREEN}[DONE] Image staging: $STAGED_IMAGE_COUNT loaded, $SKIPPED_IMAGE_COUNT unchanged${NC}"

# Redis is an explicitly optional, non-authoritative mirror. TimescaleDB is
# snapshotted above before its selected image can start. No selected extension
# container is started until schema/configuration publication has succeeded.

# Step 3: Setup directories and configuration
echo -e "${YELLOW}[3/3] Setting up configuration...${NC}"

# Preserve an operator-owned/persisted log root. Automatic /extp selection is
# only a first-install default and never migrates existing logs implicitly.
if [[ "$LOG_DIR_EXPLICIT" == true ]]; then
    echo "Preserving configured log directory: $LOG_DIR"
elif is_safe_external_storage_root "/extp" \
    && validate_compose_data_directory "/extp/logs" \
    && [[ -w "/extp" || -n "$SUDO" ]]; then
    echo "External storage detected at /extp"
    LOG_DIR="/extp/logs"
    echo "Logs will be stored at: $LOG_DIR"

else
    if [[ -e "/extp" || -L "/extp" ]] && ! is_safe_external_storage_root "/extp"; then
        echo -e "${YELLOW}Unsafe /extp entry ignored (must be a resolvable non-symlink directory).${NC}"
    fi
    echo "No external storage found, using default location"
    LOG_DIR="$INSTALL_DIR/logs"
fi

# Create all necessary directories
echo "Creating installation directories..."
$SUDO mkdir -p "$DATA_DIR"

# Create log directories (permissions will be set after user detection)
echo "Creating log directories..."
$SUDO mkdir -p "$LOG_DIR"
# Create log directories for all services
for service in aether-io aether-automation aether-history aether-api aether-uplink aether-alarm; do
    $SUDO mkdir -p "$LOG_DIR/$service"
done

# Install scripts directory (utility scripts)
if [[ -d "scripts" ]] && [[ "$INSTALL_DIR" != "$(pwd)" ]]; then
    echo "Installing utility scripts..."
    $SUDO mkdir -p "$INSTALL_DIR/scripts"

    # Copy update-env-permissions.sh if it exists
    if [[ -f "scripts/update-env-permissions.sh" ]]; then
        $SUDO cp "scripts/update-env-permissions.sh" "$INSTALL_DIR/scripts/"
        $SUDO chmod +x "$INSTALL_DIR/scripts/update-env-permissions.sh"
        echo -e "${GREEN}✓ Utility scripts installed${NC}"
    fi
fi

# Install config.template directory (only config files)
if [[ -d "config.template" ]]; then
    echo "Installing configuration templates..."
    stage_distribution_template "config.template" "$INSTALL_DIR/config.template"

    echo -e "${GREEN}✓ Configuration templates installed${NC}"
else
    echo -e "${RED}Error: config.template not found${NC}" >&2
    exit 1
fi

activate_initial_config "$INSTALL_DIR/config.template" "$LIVE_CONFIG_DIR"
persist_install_context \
    "docker-compose" \
    "$LIVE_CONFIG_DIR" \
    "$DATA_DIR" \
    "/run/aether"

# Create certificate directory for uplink TLS (mounted to /app/config/cert in container)
echo "Creating certificate directory for uplink..."
$SUDO mkdir -p "$DATA_DIR/cert"
echo -e "${GREEN}✓ Certificate directory ready: $DATA_DIR/cert${NC}"

# PostgreSQL history is an extension. Do not create or chown its storage path
# unless the selected installer bundle actually contains that profile image.
if [[ -f "docker/aether-timescaledb.tar.gz" ]]; then
    [[ -n "$TIMESCALE_DATA_DIR" ]] || {
        echo "TimescaleDB storage path was not resolved during preflight" >&2
        exit 1
    }
    echo "Creating TimescaleDB data directory for the selected postgres-storage extension..."
    $SUDO mkdir -p "$TIMESCALE_DATA_DIR"
    validate_compose_data_directory "$TIMESCALE_DATA_DIR"
    $SUDO chown -R 70:70 "$TIMESCALE_DATA_DIR" 2>/dev/null || true
    echo -e "${GREEN}✓ TimescaleDB data directory ready: $TIMESCALE_DATA_DIR${NC}"
else
    echo -e "${BLUE}ℹ PostgreSQL history extension not selected; skipping TimescaleDB storage${NC}"
fi

# Create a symlink if logs are external
if [[ "$LOG_DIR" != "$INSTALL_DIR/logs" ]]; then
    echo "Creating symlink for logs..."
    if [[ -L "$INSTALL_DIR/logs" ]]; then
        if [[ "$(realpath "$INSTALL_DIR/logs")" != "$(realpath "$LOG_DIR")" ]]; then
            echo "Refusing to repoint an existing Aether log symlink" >&2
            exit 1
        fi
    elif [[ -d "$INSTALL_DIR/logs" ]]; then
        EXISTING_LOG_ENTRY=$($SUDO find "$INSTALL_DIR/logs" -mindepth 1 -print -quit) || {
            echo "Unable to inspect existing Aether log directory" >&2
            exit 1
        }
        if [[ -n "$EXISTING_LOG_ENTRY" ]]; then
            echo "Fresh installation refused: local log root became non-empty" >&2
            exit 1
        else
            $SUDO rmdir "$INSTALL_DIR/logs"
        fi
    elif [[ -e "$INSTALL_DIR/logs" ]]; then
        echo "Refusing non-directory Aether log path: $INSTALL_DIR/logs" >&2
        exit 1
    fi
    if [[ "$LOG_DIR" != "$INSTALL_DIR/logs" && ! -L "$INSTALL_DIR/logs" ]]; then
        $SUDO ln -s "$LOG_DIR" "$INSTALL_DIR/logs"
        echo "Linked $INSTALL_DIR/logs -> $LOG_DIR"
    fi
fi

# A fresh installer never migrates or preserves an existing database. The
# preflight already rejected a populated root; this second check closes a race
# before the schema is created.
echo "Creating fresh database..."
DB_FILE="$DATA_DIR/aether.db"
if [[ -e "$DB_FILE" || -L "$DB_FILE" ]]; then
    echo "Fresh installation refused: database appeared after preflight: $DB_FILE" >&2
    exit 1
fi
$SUDO touch "$DB_FILE"
$SUDO chown "$ACTUAL_USER:docker" "$DB_FILE" 2>/dev/null || true
/usr/local/bin/aether --config-path "$LIVE_CONFIG_DIR" --db-path "$DATA_DIR" init

# Set permissions using docker group for secure access
echo "Setting up permissions..."

if [[ -z "${ACTUAL_USER:-}" ]]; then
    echo -e "${RED}Error: Failed to determine installation user. Aborting.${NC}"
    exit 1
fi

# Check if docker group exists and get its GID
# Use getent if available, fall back to grep /etc/group
_get_group_info() {
    local name="$1"
    if command -v getent &>/dev/null; then
        getent group "$name" 2>/dev/null
    elif [[ -f /etc/group ]]; then
        grep "^${name}:" /etc/group 2>/dev/null
    fi
}

DOCKER_GROUP=$(_get_group_info docker)
if [[ -n "$DOCKER_GROUP" ]]; then
    DOCKER_GID=$(echo "$DOCKER_GROUP" | cut -d: -f3)
    echo "Docker group found (GID=$DOCKER_GID)"
else
    echo "Warning: docker group not found, creating it..."
    $SUDO groupadd docker 2>/dev/null || true
    DOCKER_GID=$(_get_group_info docker | cut -d: -f3)
fi

# Get numeric UID and GID
ACTUAL_UID=$(id -u "$ACTUAL_USER")
ACTUAL_GID=$(id -g "$ACTUAL_USER")
ACTUAL_GROUP=$(id -gn "$ACTUAL_USER")

echo "Setting permissions (UID=$ACTUAL_UID, GID=$ACTUAL_GID)..."

# Set ownership for all directories
$SUDO chown -R ${ACTUAL_UID}:${ACTUAL_GID} "$INSTALL_DIR" 2>/dev/null || true
$SUDO chown -R "${ACTUAL_UID}:${ACTUAL_GID}" "$DATA_DIR" 2>/dev/null || true

# Fix /extp if using external storage
if [[ "$LOG_DIR" == "/extp/logs" ]] && [[ -d "/extp" ]]; then
    validate_compose_data_directory "$LOG_DIR"
    # Recursive chown for external log dir (not covered by INSTALL_DIR chown above);
    # without this, /extp/logs/<service>/ subdirs stay root-owned and the container's
    # UID=1000 api/io/... fail to init logging until they self-mkdir.
    $SUDO chown -R ${ACTUAL_UID}:${ACTUAL_GID} "$LOG_DIR" 2>/dev/null || true
elif [[ "$LOG_DIR" != "$INSTALL_DIR/logs" ]]; then
    validate_compose_data_directory "$LOG_DIR"
    $SUDO chown -R ${ACTUAL_UID}:${ACTUAL_GID} "$LOG_DIR" 2>/dev/null || true
fi

# Set permissions: 755 for dirs, 777 for logs (container write access)
$SUDO chmod 755 "$INSTALL_DIR" 2>/dev/null || true
$SUDO chmod -R 775 "$DATA_DIR" 2>/dev/null || true
$SUDO chmod -R 775 "$INSTALL_DIR/config" 2>/dev/null || true
$SUDO chmod -R 775 "$INSTALL_DIR/config.template" 2>/dev/null || true
$SUDO chmod -R 775 "$LOG_DIR" 2>/dev/null || true

# Fix symlink ownership if exists
[[ -L "$INSTALL_DIR/logs" ]] && $SUDO chown -h ${ACTUAL_UID}:${ACTUAL_GID} "$INSTALL_DIR/logs" 2>/dev/null || true

echo -e "${GREEN}✓ Permissions configured${NC}"

# Create system-wide environment variables for Docker Compose
echo "Creating system environment variables..."

# Read device serial number from device tree if available
DEVICE_SN=""
if [[ -f /proc/device-tree/serial-number ]]; then
    DEVICE_SN=$(cat /proc/device-tree/serial-number 2>/dev/null | tr -d '\0' | tr -d '\n')
    if [[ -n "$DEVICE_SN" ]]; then
        echo "Detected device serial number: $DEVICE_SN"
    fi
fi

# Update the Compose environment through one-key atomic rewrites. The complete
# pre-install file is already snapshotted, so any later failure restores it.
ENV_FILE="$INSTALL_DIR/.env"
persist_compose_env_value "$ENV_FILE" HOST_UID "$ACTUAL_UID"
persist_compose_env_value "$ENV_FILE" HOST_GID "$ACTUAL_GID"
persist_compose_env_value "$ENV_FILE" DEVICE_SN "$DEVICE_SN"
persist_compose_env_value "$ENV_FILE" AETHER_LOG_PATH "$LOG_DIR"
persist_compose_env_value "$ENV_FILE" AETHER_BASE_PATH "$DATA_DIR"
if [[ -n "$TIMESCALE_DATA_DIR" ]]; then
    persist_compose_env_value \
        "$ENV_FILE" AETHER_TIMESCALE_DATA_PATH "$TIMESCALE_DATA_DIR"
else
    # Remove a stale extension-only setting without touching other secrets.
    ENV_STAGE=$($SUDO mktemp "$INSTALL_DIR/.env.new.XXXXXX")
    if ! $SUDO awk '!/^AETHER_TIMESCALE_DATA_PATH=/' "$ENV_FILE" \
        | $SUDO tee "$ENV_STAGE" >/dev/null \
        || ! $SUDO chown "${ACTUAL_UID}:${ACTUAL_GID}" "$ENV_STAGE" \
        || ! $SUDO chmod 600 "$ENV_STAGE" \
        || ! $SUDO mv -f "$ENV_STAGE" "$ENV_FILE"; then
        $SUDO rm -f "$ENV_STAGE" || true
        exit 1
    fi
fi
$SUDO chmod 600 "$ENV_FILE"
echo -e "${GREEN}✓ Environment variables saved to $ENV_FILE${NC}"

# Also create system-wide environment file for convenience (if profile.d exists)
if [[ -d "$(dirname "$PROFILE_ENTRY")" ]]; then
    $SUDO tee "$PROFILE_ENTRY" > /dev/null << EOF
# AetherEMS Docker environment variables
# Generated by install.sh on $(date)
# User: $ACTUAL_USER (UID=$ACTUAL_UID, GID=$ACTUAL_GID)
export HOST_UID=$ACTUAL_UID
export HOST_GID=$ACTUAL_GID
export DEVICE_SN=$DEVICE_SN
EOF
    $SUDO chmod 644 "$PROFILE_ENTRY"
    echo -e "${GREEN}✓ Environment variables exported to $PROFILE_ENTRY${NC}"
else
    echo -e "${YELLOW}⚠ /etc/profile.d not found, skipping system-wide env export${NC}"
    echo -e "${YELLOW}  Variables are available in $ENV_FILE${NC}"
fi

echo "Permissions configured:"
echo "  User: $ACTUAL_USER (UID=$ACTUAL_UID)"
echo "  Primary group: $ACTUAL_GROUP (GID=$ACTUAL_GID)"
echo "  Mode: 775 (directories), 664 (files)"

# Add user to docker group if not already
if ! groups $ACTUAL_USER 2>/dev/null | grep -q docker; then
    echo "Adding $ACTUAL_USER to docker group..."
    $SUDO usermod -aG docker $ACTUAL_USER
    echo "IMPORTANT: User must logout and login for group changes to take effect!"
fi

# Additional config samples are not needed - already handled above

# Install docker-compose.yml
if [[ -f docker-compose.yml ]]; then
    echo "Validating and atomically publishing docker-compose.yml..."
    publish_compose_atomically "docker-compose.yml"
    echo -e "${GREEN}docker-compose.yml published${NC}"
else
    echo -e "${RED}Packaged docker-compose.yml not found${NC}" >&2
    exit 1
fi

echo -e "${GREEN}[DONE] Configuration installed${NC}"

echo ""
echo -e "${GREEN}================================${NC}"
echo -e "${GREEN}  Configuration Staged          ${NC}"
echo -e "${GREEN}================================${NC}"
echo ""
echo "  Installation directory: $INSTALL_DIR"

if [[ "$AUTO_MODE" != true ]]; then
    echo "Installed components:"
    echo "  • CLI Tool: aether (unified management)"
    echo "  • Docker Image: aetherems (all Rust services)"
    echo "    - Optional profiles: aether-redis mirror, aether-timescaledb history"
    if [[ "$LOG_DIR" != "$INSTALL_DIR/logs" ]]; then
        echo "  • Log directory: $LOG_DIR (symlinked from $INSTALL_DIR/logs)"
    else
        echo "  • Log directory: $LOG_DIR"
    fi
    echo ""

    # Display actual permissions for verification
    echo -e "${BLUE}Directory Permissions:${NC}"
    echo "--------------------------------------------"
    ls -ld "$INSTALL_DIR" 2>/dev/null | awk '{printf "%-20s %s %s:%s\n", $9":", $1, $3, $4}'

    if [[ -d "$INSTALL_DIR/data" ]]; then
        ls -ld "$INSTALL_DIR/data" 2>/dev/null | awk '{printf "%-20s %s %s:%s\n", "├── data:", $1, $3, $4}'
    fi

    if [[ -L "$INSTALL_DIR/logs" ]]; then
        LINK_INFO=$(ls -ld "$INSTALL_DIR/logs" 2>/dev/null)
        TARGET=$(readlink "$INSTALL_DIR/logs" 2>/dev/null)
        echo "$LINK_INFO" | awk -v target="$TARGET" '{printf "%-20s %s %s:%s -> %s\n", "├── logs:", $1, $3, $4, target}'
    elif [[ -d "$INSTALL_DIR/logs" ]]; then
        ls -ld "$INSTALL_DIR/logs" 2>/dev/null | awk '{printf "%-20s %s %s:%s\n", "├── logs:", $1, $3, $4}'
    fi

    if [[ -d "$INSTALL_DIR/config.template" ]]; then
        ls -ld "$INSTALL_DIR/config.template" 2>/dev/null | awk '{printf "%-20s %s %s:%s\n", "├── config.template:", $1, $3, $4}'
    fi

    if [[ -f "$INSTALL_DIR/docker-compose.yml" ]]; then
        ls -l "$INSTALL_DIR/docker-compose.yml" 2>/dev/null | awk '{printf "%-20s %s %s:%s\n", "└── docker-compose:", $1, $3, $4}'
    fi
    echo "--------------------------------------------"

    # If using external log directory, show its permissions too
    if [[ "$LOG_DIR" != "$INSTALL_DIR/logs" ]] && [[ -d "$LOG_DIR" ]]; then
        echo ""
        echo -e "${BLUE}External Log Directory Permissions:${NC}"
        echo "--------------------------------------------"
        ls -ld "$LOG_DIR" 2>/dev/null | awk '{printf "%-25s %s %s:%s\n", $9":", $1, $3, $4}'

        # Show service subdirectories
        for service in aether-io aether-automation aether-history aether-api aether-uplink aether-alarm; do
            if [[ -d "$LOG_DIR/$service" ]]; then
                ls -ld "$LOG_DIR/$service" 2>/dev/null | awk -v svc="├── $service:" '{printf "%-25s %s %s:%s\n", svc, $1, $3, $4}'
            fi
        done
        echo "--------------------------------------------"
    fi

    echo ""

    # Check if permissions might need attention
    MAIN_OWNER=$(stat -c "%U" "$INSTALL_DIR" 2>/dev/null || stat -f "%Su" "$INSTALL_DIR" 2>/dev/null || echo "unknown")
    if [[ "$MAIN_OWNER" == "root" ]]; then
        echo -e "${YELLOW}⚠ Note: Directory is owned by root${NC}"
        echo -e "${YELLOW}  To change owner: sudo chown -R <user>:docker $INSTALL_DIR${NC}"
        echo ""
    fi

    echo "Permission Configuration:"
    echo "  • Directories owned by: $ACTUAL_USER:$ACTUAL_GROUP"
    echo "  • Ensure your user is in docker group:"
    echo -e "    ${YELLOW}sudo usermod -aG docker \$USER${NC}"
    echo "    (logout and login for changes to take effect)"
    echo ""
    echo "Network Configuration:"
    echo -e "${YELLOW}  • Using host network mode for optimal performance${NC}"
    echo "  • Services available on localhost:"
    echo "    - Redis: 6379          (optional mirror profile)"
    echo "    - TimescaleDB: 5432    (optional PostgreSQL history profile)"
    echo "    - aether-io: 6001         (communication - Rust)"
    echo "    - aether-automation: 6002 (model + rules - Rust)"
    echo "    - aether-history: 6004    (history - Rust, storage via /hisApi/storage)"
    echo "    - aether-api: 6005     (gateway - Rust)"
    echo "    - aether-uplink: 6006    (network/MQTT - Rust)"
    echo "    - aether-alarm: 6007     (alarm - Rust)"
    echo "    - Frontend: 8080       (Vue.js + nginx)"
    echo ""
    echo -e "${YELLOW}Important: Configuration Setup Required${NC}"
    echo "  A fail-safe configuration with no commissioned device was activated at:"
    echo "     $LIVE_CONFIG_DIR"
    echo "  To commission this host:"
    echo "  1. Add site channels, instances, and rules under that directory"
    echo "  2. Validate and sync configurations to the embedded database:"
    echo "     aether sync"
    echo "  3. (Optional) Configure history storage backend via API after startup:"
    echo "     PUT http://127.0.0.1:6004/hisApi/storage"
    echo "     POST http://127.0.0.1:6004/hisApi/storage/reconnect"
    echo "  4. (Optional) Upload MQTT TLS certificates via API:"
    echo "     POST http://127.0.0.1:6006/netApi/certificate/upload"
    echo "     (Files saved to $DATA_DIR/cert/)"
    echo ""
    echo -e "${BLUE}Configuration activation:${NC}"
    echo "  aether sync          - Validate and atomically activate configuration"
    echo ""
    echo "Quick Start:"
    echo -e "  ${YELLOW}source /etc/profile.d/aetheredge.sh${NC}  - Load environment variables (or re-login)"
    echo "  aether services start - Start all services"
    echo "  aether services stop  - Stop all services"
    echo "  aether services status - Check service status"
    echo "  aether services logs <service> - View service logs"
    echo ""
    echo "CLI Management (via aether):"
    echo ""
    echo "  Runtime and configuration:"
    echo "    aether setup                 - Show a reproducible commissioning plan"
    echo "    aether sync                  - Validate and activate configuration"
    echo "    aether status                - Show configuration status"
    echo "    aether doctor                - Run full runtime diagnostics"
    echo ""
    echo "  Read-only inventory:"
    echo "    aether channels list         - List all channels"
    echo "    aether models products list  - List products"
    echo "    aether models instances list - List instances"
    echo "    aether rules list            - List all rules"
    echo ""
    echo "  Services:"
    echo "    aether services start        - Start all services"
    echo "    aether services stop         - Stop all services"
    echo "    aether services logs io  - View service logs"
    echo ""
    echo -e "${YELLOW}Note: Using host network mode - ensure ports are not in use${NC}"
fi
echo ""

# =============================================================================
# Start Services
# =============================================================================
echo -e "${BLUE}================================${NC}"
echo -e "${BLUE}  Start Services                ${NC}"
echo -e "${BLUE}================================${NC}"
echo ""

# Commit the transaction with one recreation against the newly validated
# Compose file. This applies security/mount changes even when image IDs did not
# change and prevents any service from observing a half-migrated database.
INSTALL_SERVICES=(
    aether-io
    aether-automation
    aether-history
    aether-api
    aether-uplink
    aether-alarm
)
INSTALL_CONTAINERS=(
    aether-io
    aether-automation
    aether-history
    aether-api
    aether-uplink
    aether-alarm
)

add_install_service() {
    local service=$1
    local container=$2
    local existing

    for existing in "${INSTALL_SERVICES[@]}"; do
        [[ "$existing" == "$service" ]] && return 0
    done
    INSTALL_SERVICES+=("$service")
    INSTALL_CONTAINERS+=("$container")
}

[[ -f "docker/aether-redis.tar.gz" ]] \
    && add_install_service aether-redis aether-redis
[[ -f "docker/aether-timescaledb.tar.gz" ]] \
    && add_install_service timescaledb aether-timescaledb
[[ -f "docker/apps.tar.gz" ]] \
    && add_install_service apps aether-apps

echo -e "${GREEN}Starting and validating the complete installed stack...${NC}"
reject_existing_docker_container_footprint
CONTAINER_STATE_MUTATED=true
(cd "$INSTALL_DIR" && run_docker_compose up -d --force-recreate \
    "${INSTALL_SERVICES[@]}")
INSTALL_CONTAINER_LIST="${INSTALL_CONTAINERS[*]}"
wait_for_installed_stack "$INSTALL_CONTAINER_LIST" 30
INSTALL_TRANSACTION_COMMITTED=true
echo -e "${GREEN}✓ Installed stack passed the commit health window${NC}"
docker ps --format "table {{.Names}}\t{{.Status}}"
echo ""

# =============================================================================
# Cleanup
# =============================================================================
echo -e "${BLUE}================================${NC}"
echo -e "${BLUE}  Cleanup                       ${NC}"
echo -e "${BLUE}================================${NC}"
echo ""

# Try to detect installer package location
INSTALLER_NAME=""
POSSIBLE_LOCATIONS=(
    "$LAUNCH_DIR/AetherEdge-*-*.run"
    "/tmp/AetherEdge-*-*.run"
    "$HOME/AetherEdge-*-*.run"
    "$HOME/Downloads/AetherEdge-*-*.run"
)

# Search for installer in common locations
for pattern in "${POSSIBLE_LOCATIONS[@]}"; do
    # Use nullglob to handle no matches gracefully
    shopt -s nullglob
    for file in $pattern; do
        if [[ -f "$file" ]]; then
            INSTALLER_NAME="$file"
            break 2
        fi
    done
    shopt -u nullglob
done

if [[ -n "$INSTALLER_NAME" ]]; then
    echo -e "${YELLOW}Installer package detected:${NC}"
    echo "  Location: $INSTALLER_NAME"
    echo "  Size: $(du -h "$INSTALLER_NAME" 2>/dev/null | cut -f1)"
    echo ""
    echo -e "${GREEN}Cleaning up installer package...${NC}"
    if rm -f "$INSTALLER_NAME" 2>/dev/null; then
        echo -e "${GREEN}✓ Installer package deleted${NC}"
    else
        echo -e "${YELLOW}Warning: Failed to delete installer (may need sudo)${NC}"
        echo "  You can manually delete it with:"
        echo "  $SUDO rm -f '$INSTALLER_NAME'"
    fi
else
    echo -e "${BLUE}No installer package found in common locations.${NC}"
fi

# ── Install optional dpkg packages ──────────────────────────────────────────
if [[ -f "./dpkg/install-awsiot-deb.sh" ]]; then
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}  Installing bundled dpkg packages${NC}"
    echo -e "${BLUE}================================================${NC}"
    chmod +x ./dpkg/install-awsiot-deb.sh
    if $SUDO ./dpkg/install-awsiot-deb.sh; then
        echo -e "${GREEN}✓ dpkg packages installed successfully${NC}"
    else
        echo -e "${YELLOW}Warning: dpkg install returned non-zero exit code${NC}"
        echo -e "${YELLOW}You can retry manually: sudo ./dpkg/install-awsiot-deb.sh${NC}"
    fi
fi

echo ""
print_bootstrap_admin_instructions "$INSTALL_DIR/.env"
echo ""
echo -e "${GREEN}Installation complete! Thank you for using AetherEMS.${NC}"
echo ""
