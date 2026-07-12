#!/usr/bin/env bash
# Build multi-architecture installer package for AetherEMS
# Usage: build-installer.sh [VERSION] [ARCH] [TARGET] [--services=...] [--enable-swagger]
#   VERSION: Version string (default: YYYYMMDD)
#   ARCH: arm64 | amd64 (default: arm64)
#   TARGET: Rust target triple (default based on ARCH)
#   --services: Comma-separated list of services to include (optional, default: Rust core)
#   --enable-swagger: Enable Swagger UI for feature-gated Rust services
#
# Service names: aether-io, aether-automation, aether-history, aether-api,
# aether-uplink, aether-alarm, apps, redis, timescaledb (canonical
# aether-apps/aether-redis/aether-timescaledb names are accepted as aliases)
# Service groups: rust (all Rust services), py (the four former Python services)
#
# Examples:
#   ./build-installer.sh                                    # Build the Rust core (ARM64, today's date)
#   ./build-installer.sh v1.2.0 arm64                       # Build the Rust core (ARM64, v1.2.0)
#   ./build-installer.sh v1.2.0 arm64 -s rust               # All Rust services
#   ./build-installer.sh v1.2.0 arm64 -s rust --enable-swagger
#   ./build-installer.sh v1.2.0 arm64 -s aether-uplink,aether-history

set -euo pipefail

# Disable macOS resource fork files
export COPYFILE_DISABLE=1
export COPY_EXTENDED_ATTRIBUTES_DISABLE=1

# Docker Desktop on macOS ships credential helpers outside the default PATH.
if [[ "$OSTYPE" == "darwin"* ]] \
    && ! command -v docker-credential-desktop &> /dev/null \
    && [[ -x "/Applications/Docker.app/Contents/Resources/bin/docker-credential-desktop" ]]; then
    export PATH="/Applications/Docker.app/Contents/Resources/bin:$PATH"
fi

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Paths
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="$ROOT_DIR/build/installer"
OUTPUT_DIR="$ROOT_DIR/release"
LICENSE_FILE="$ROOT_DIR/LICENSE"
NOTICE_FILE="$ROOT_DIR/NOTICE"

if [[ ! -f "$LICENSE_FILE" || ! -f "$NOTICE_FILE" ]]; then
    echo -e "${RED}Error: root LICENSE or NOTICE file is missing${NC}"
    exit 1
fi

verify_installer_license() {
    local installer_path="$1"
    if ! "$installer_path" --list | grep -Eq '(^|/)LICENSE$'; then
        echo -e "${RED}Error: LICENSE missing from installer $installer_path${NC}"
        return 1
    fi
    if ! "$installer_path" --list | grep -Eq '(^|/)NOTICE$'; then
        echo -e "${RED}Error: NOTICE missing from installer $installer_path${NC}"
        return 1
    fi
}

# Parse arguments
VERSION=""
ARCH=""
TARGET=""
SELECTED_SERVICES=""
ENABLE_SWAGGER=0
BARE_METAL=0
MANIFEST_ONLY=""
IO_PROTOCOL_FEATURES=""
IO_PROTOCOL_FEATURES_SET=0

# Parse all arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --services=*)
            SELECTED_SERVICES="${1#*=}"
            shift
            ;;
        -s|--services)
            SELECTED_SERVICES="$2"
            shift 2
            ;;
        --enable-swagger)
            ENABLE_SWAGGER=1
            shift
            ;;
        --bare-metal)
            BARE_METAL=1
            shift
            ;;
        --manifest-only=*)
            MANIFEST_ONLY="${1#*=}"
            shift
            ;;
        --manifest-only)
            MANIFEST_ONLY="$2"
            shift 2
            ;;
        --io-features=*)
            IO_PROTOCOL_FEATURES="${1#*=}"
            IO_PROTOCOL_FEATURES_SET=1
            shift
            ;;
        --io-features)
            IO_PROTOCOL_FEATURES="$2"
            IO_PROTOCOL_FEATURES_SET=1
            shift 2
            ;;
        *)
            if [[ -z "$VERSION" ]]; then
                VERSION="$1"
            elif [[ -z "$ARCH" ]]; then
                ARCH="$1"
            elif [[ -z "$TARGET" ]]; then
                TARGET="$1"
            fi
            shift
            ;;
    esac
done

# Set defaults
VERSION="${VERSION:-$(date +%Y%m%d)}"
ARCH="${ARCH:-arm64}"

# Set defaults based on architecture
case "$ARCH" in
  arm64)
    TARGET="${TARGET:-aarch64-unknown-linux-musl}"
    DOCKER_PLATFORM="linux/arm64"
    ;;
  amd64)
    TARGET="${TARGET:-x86_64-unknown-linux-musl}"
    DOCKER_PLATFORM="linux/amd64"
    ;;
  *)
    echo -e "${RED}Error: Unknown architecture '$ARCH'. Use 'arm64' or 'amd64'${NC}"
    exit 1
    ;;
esac

# Contract/CI mode produces exactly the metadata that a matching build would
# ship, without requiring makeself, cross toolchains, Docker, or host mutation.
if [[ -n "$MANIFEST_ONLY" ]]; then
    MANIFEST_TEMP_DIR=$(mktemp -d)
    trap 'rm -rf "$MANIFEST_TEMP_DIR"' EXIT
    MANIFEST_GENERATE_ARGS=(generate "$TARGET" "$MANIFEST_TEMP_DIR")
    if [[ "$IO_PROTOCOL_FEATURES_SET" == 1 ]]; then
        MANIFEST_GENERATE_ARGS+=("$IO_PROTOCOL_FEATURES")
    fi
    cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
        "${MANIFEST_GENERATE_ARGS[@]}" >/dev/null
    mkdir -p "$(dirname "$MANIFEST_ONLY")"
    cp "$MANIFEST_TEMP_DIR/runtime-manifest.json" "$MANIFEST_ONLY"
    exit 0
fi

ARCH_LABEL=$(printf '%s' "$ARCH" | tr '[:lower:]' '[:upper:]')

# All Rust services bundled into the aetherems image
RUST_SERVICES="aether-io,aether-automation,aether-history,aether-api,aether-uplink,aether-alarm"

# Expand service group shortcuts
expand_service_groups() {
    local input="$1"
    local expanded=""
    
    IFS=',' read -ra ITEMS <<< "$input"
    for item in "${ITEMS[@]}"; do
        item=$(echo "$item" | xargs)  # trim whitespace
        case "$item" in
            rust)
                expanded="${expanded}${RUST_SERVICES},"
                ;;
            py)
                # 4 services rewritten from Python
                expanded="${expanded}aether-alarm,aether-api,aether-history,aether-uplink,"
                ;;
            *)
                expanded="${expanded}${item},"
                ;;
        esac
    done
    
    # Remove trailing comma and duplicates
    echo "$expanded" | sed 's/,$//' | tr ',' '\n' | sort -u | tr '\n' ',' | sed 's/,$//'
}

# Save original input before group expansion (used for dev mode detection below)
ORIGINAL_SERVICES_INPUT="${SELECTED_SERVICES}"

# Expand service groups if shortcuts are used
if [[ -n "$SELECTED_SERVICES" ]]; then
    SELECTED_SERVICES=$(expand_service_groups "$SELECTED_SERVICES")
fi

# ── Dev mode detection ────────────────────────────────────────────────────────
# Triggered when the ORIGINAL input (before expansion) is:
#   • A single Rust service name  →  aetherems:dev-<name>, restart that 1 container
#   • "py"                        →  aetherems:dev-py,    restart 4 py-rewritten containers
# aetherems:latest is NEVER touched in dev mode.
DEV_SERVICE=""        # image tag suffix  (e.g. "uplink" or "py")
DEV_SERVICES_LIST=""  # space-separated container service names to restart
_RUST_SVC_LIST="aether-io aether-automation aether-history aether-api aether-uplink aether-alarm"

if [[ -n "$ORIGINAL_SERVICES_INPUT" ]]; then
    _orig=$(echo "$ORIGINAL_SERVICES_INPUT" | xargs)
    if [[ "$_orig" == "py" ]]; then
        DEV_SERVICE="py"
        DEV_SERVICES_LIST="aether-alarm aether-api aether-history aether-uplink"
    else
        _cnt=$(echo "$ORIGINAL_SERVICES_INPUT" | tr ',' '\n' | grep -c .)
        if [[ $_cnt -eq 1 ]]; then
            for _s in $_RUST_SVC_LIST; do
                if [[ "$_orig" == "$_s" ]]; then
                    DEV_SERVICE="$_s"
                    DEV_SERVICES_LIST="$_s"
                    break
                fi
            done
        fi
        unset _cnt
    fi
    unset _orig
fi
unset _RUST_SVC_LIST
# ─────────────────────────────────────────────────────────────────────────────

# Adjust package name based on selected services
if [[ -n "$SELECTED_SERVICES" ]]; then
    # Create a shorter suffix for the package name
    SERVICES_SUFFIX=$(echo "$SELECTED_SERVICES" | tr ',' '-')
    PACKAGE_NAME="AetherEdge-${ARCH}-${VERSION}-${SERVICES_SUFFIX}"
else
    PACKAGE_NAME="AetherEdge-${ARCH}-${VERSION}"
fi

if [[ "$ENABLE_SWAGGER" == "1" ]]; then
    PACKAGE_NAME="${PACKAGE_NAME}-swagger"
fi

# Service to image mapping – all services now use the unified aetherems image
service_to_image() {
    case "$1" in
        aether-io|aether-automation|aether-history|aether-api|aether-uplink|aether-alarm)
            echo "aetherems:latest"
            ;;
        aether-apps|apps)
            echo "aether-apps:latest"
            ;;
        aether-redis|redis)
            echo "redis:8-alpine"
            ;;
        aether-timescaledb|timescaledb)
            echo "timescale/timescaledb:2.25.2-pg17"
            ;;
        *)
            return 1
            ;;
    esac
}

add_csv_item() {
    local current="$1"
    local item="$2"

    if [[ -z "$current" ]]; then
        echo "$item"
    elif [[ ",$current," == *",$item,"* ]]; then
        echo "$current"
    else
        echo "$current,$item"
    fi
}

csv_contains() {
    local current="$1"
    local item="$2"

    [[ ",$current," == *",$item,"* ]]
}

# Parse selected services
BUILD_IMAGES=""
if [[ -n "$SELECTED_SERVICES" ]]; then
    IFS=',' read -ra SERVICES_ARRAY <<< "$SELECTED_SERVICES"
    for service in "${SERVICES_ARRAY[@]}"; do
        service=$(echo "$service" | xargs)  # trim whitespace
        if image=$(service_to_image "$service"); then
            BUILD_IMAGES=$(add_csv_item "$BUILD_IMAGES" "$image")
        else
            echo -e "${RED}Error: Unknown service '$service'${NC}"
            echo "Available services: aether-io aether-automation aether-history aether-api aether-uplink aether-alarm apps redis timescaledb"
            exit 1
        fi
    done
else
    # Default edge-kernel distribution contains only the six Rust services.
    # Frontend and external stores are selected explicitly.
    BUILD_IMAGES="aetherems:latest"
fi

if [[ "$BARE_METAL" == 1 ]] && ! csv_contains "$BUILD_IMAGES" "aetherems:latest"; then
    echo -e "${RED}Error: a bare-metal package must include the Rust core (use --services rust,...).${NC}"
    exit 1
fi
if [[ "$BARE_METAL" != 1 && -z "$DEV_SERVICE" ]] \
    && ! csv_contains "$BUILD_IMAGES" "aetherems:latest"; then
    echo -e "${RED}Error: a fresh Docker installer must include the Rust core (use --services rust,...).${NC}"
    exit 1
fi
if [[ "$BARE_METAL" == 1 ]] \
    && csv_contains "$BUILD_IMAGES" "timescale/timescaledb:2.25.2-pg17"; then
    echo -e "${RED}Error: the TimescaleDB extension is not bundled in bare-metal packages.${NC}"
    exit 1
fi

# Detect CPU cores
if command -v nproc &> /dev/null; then
    CPU_CORES=$(nproc)
elif command -v sysctl &> /dev/null; then
    CPU_CORES=$(sysctl -n hw.ncpu)
else
    CPU_CORES=4
fi

echo -e "${BLUE}================================================${NC}"
echo -e "${BLUE}    AetherEdge ${ARCH_LABEL} Installer Builder     ${NC}"
echo -e "${BLUE}================================================${NC}"
echo ""
echo -e "Version:      ${GREEN}$VERSION${NC}"
echo -e "Architecture: ${GREEN}$ARCH${NC}"
echo -e "Target:       ${GREEN}$TARGET${NC}"
echo -e "Platform:     ${GREEN}$DOCKER_PLATFORM${NC}"
echo -e "CPU Cores:    ${GREEN}$CPU_CORES${NC}"
if [[ "$ENABLE_SWAGGER" == "1" ]]; then
    echo -e "Swagger UI:   ${GREEN}ENABLED (--enable-swagger)${NC}"
else
    echo -e "Swagger UI:   ${YELLOW}DISABLED${NC}"
fi
if [[ -n "$DEV_SERVICE" ]]; then
    echo -e "Mode:         ${YELLOW}DEV — shared test machine, aetherems:latest untouched${NC}"
    echo -e "Dev tag:      ${YELLOW}aetherems:dev-${DEV_SERVICE}${NC}"
    echo -e "Containers:   ${YELLOW}${DEV_SERVICES_LIST}${NC}"
elif [[ -n "$SELECTED_SERVICES" ]]; then
    echo -e "Services:     ${YELLOW}$SELECTED_SERVICES (partial build)${NC}"
    echo -e "Images:       ${YELLOW}${BUILD_IMAGES}${NC}"
else
    echo -e "Services:     ${GREEN}RUST CORE (default build)${NC}"
fi
echo ""

# Check for makeself
if ! command -v makeself &> /dev/null; then
    echo -e "${YELLOW}Warning: makeself not found. Installing...${NC}"
    if [[ "$OSTYPE" == "darwin"* ]]; then
        brew install makeself
    else
        echo -e "${RED}Please install makeself first${NC}"
        exit 1
    fi
fi

# Helper functions
copy_config_files() {
    local src="$1"
    local dst="$2"
    if [[ -d "$src" ]]; then
        find "$src" -type d | while read dir; do
            rel_dir="${dir#$src}"
            [[ -n "$rel_dir" ]] && mkdir -p "$dst$rel_dir"
        done
        find "$src" -type f \( -name "*.yaml" -o -name "*.yml" -o -name "*.csv" -o -name "*.json" \) | while read file; do
            rel_path="${file#$src}"
            cp "$file" "$dst$rel_path"
        done
    fi
}

copy_docker_images() {
    local src="$1"
    local dst="$2"
    if [[ -d "$src" ]]; then
        mkdir -p "$dst"
        find "$src" -name "*.tar.gz" -type f -exec cp {} "$dst/" \;
    fi
}

# 持久化镜像缓存目录（跨构建复用，按架构隔离）
IMAGE_CACHE_DIR="$ROOT_DIR/build/image-cache/$ARCH"
mkdir -p "$IMAGE_CACHE_DIR"

# 获取远端镜像针对当前架构的 digest（不下载镜像本身）
# 返回值写入 stdout；失败时输出空字符串
_get_remote_digest() {
    local full_image="$1"
    skopeo inspect --override-os linux --override-arch "$ARCH" \
        --format '{{.Digest}}' \
        "docker://$full_image" 2>/dev/null || true
}

pull_and_save_image() {
    local image=$1
    local output_name=$2
    local output_path="$BUILD_DIR/docker/$output_name"
    local size
    local temp_tag

    # 补全官方镜像的完整路径（skopeo 需要 docker.io/library/ 前缀）
    local full_image="$image"
    [[ "$image" != *"/"* ]] && full_image="docker.io/library/$image"

    # ── skopeo 路径（含缓存）──────────────────────────────────────────────────
    if command -v skopeo &> /dev/null; then
        local cache_tar="$IMAGE_CACHE_DIR/${output_name}"
        local cache_digest_file="$IMAGE_CACHE_DIR/${output_name%.gz}.digest"

        # 获取远端 digest（仅元数据请求，速度很快）
        echo -e "${BLUE}Checking remote digest for $image ($ARCH)...${NC}"
        local remote_digest
        remote_digest=$(_get_remote_digest "$full_image")

        local cached_digest=""
        [[ -f "$cache_digest_file" ]] && cached_digest=$(cat "$cache_digest_file")

        if [[ -n "$remote_digest" && "$remote_digest" == "$cached_digest" && -f "$cache_tar" ]]; then
            # 缓存命中：直接复用，跳过下载
            echo -e "${GREEN}✓ Cache hit for $image ($remote_digest)${NC}"
            cp "$cache_tar" "$output_path"
            size=$(ls -lh "$output_path" | awk '{print $5}')
            echo -e "${GREEN}✓ Restored $output_name from cache ($size)${NC}"
            return 0
        fi

        if [[ -n "$remote_digest" && "$remote_digest" != "$cached_digest" && -f "$cache_tar" ]]; then
            echo -e "${YELLOW}  Cache outdated (local: ${cached_digest:0:19}… remote: ${remote_digest:0:19}…), re-downloading...${NC}"
        elif [[ ! -f "$cache_tar" ]]; then
            echo -e "${BLUE}  No cache found, downloading $image...${NC}"
        fi

        # 下载到缓存（最多重试 3 次）
        local base_tar="${cache_tar%.gz}"
        local skopeo_ok=0
        for attempt in 1 2 3; do
            [[ $attempt -gt 1 ]] && echo -e "${YELLOW}  Retrying skopeo (attempt $attempt/3)...${NC}" && sleep 5
            rm -f "$base_tar"
            if skopeo copy --override-os linux --override-arch "$ARCH" \
                "docker://$full_image" \
                "docker-archive:$base_tar:$image" > /dev/null; then
                skopeo_ok=1
                break
            fi
        done

        if [[ $skopeo_ok -eq 1 ]]; then
            gzip -f "$base_tar"
            # 写入 digest 到缓存（仅下载成功后才更新）
            [[ -n "$remote_digest" ]] && echo "$remote_digest" > "$cache_digest_file"
            cp "$cache_tar" "$output_path"
            size=$(ls -lh "$output_path" | awk '{print $5}')
            echo -e "${GREEN}✓ Saved $output_name via skopeo and cached ($size)${NC}"
            return 0
        else
            echo -e "${YELLOW}Warning: skopeo failed after 3 attempts, falling back to docker...${NC}"
        fi
    fi

    # ── docker 兜底路径（无缓存）────────────────────────────────────────────
    # Docker 24+ containerd 镜像存储对多架构 manifest list 执行 docker save 有 bug，
    # 使用 buildx 将镜像重新打包为单架构副本后再 save。
    echo -e "${BLUE}Pulling $image for $ARCH via docker...${NC}"
    docker pull --platform "$DOCKER_PLATFORM" "$image"

    temp_tag="aether-save-temp-$(date +%s%N)"
    if echo "FROM --platform=$DOCKER_PLATFORM $image" \
        | docker buildx build --platform "$DOCKER_PLATFORM" --load -t "$temp_tag" - 2>/dev/null; then
        docker save "$temp_tag" | gzip > "$output_path"
        docker rmi "$temp_tag" > /dev/null 2>&1
    else
        echo -e "${YELLOW}Warning: buildx re-tag failed, trying direct docker save...${NC}"
        docker save "$image" | gzip > "$output_path"
    fi

    size=$(ls -lh "$output_path" | awk '{print $5}')
    echo -e "${GREEN}✓ Saved $output_name ($size)${NC}"
}

# Clean and create build directory
echo -e "${YELLOW}Preparing build directory...${NC}"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"/{tools,docker,config,scripts}
mkdir -p "$OUTPUT_DIR"

# Generate the mandatory feature-exact runtime metadata from the same default
# feature source that is passed to the aether-io build below. The target-specific
# file is staged outside the source tree and later copied into Docker and
# bare-metal configuration payloads.
RUNTIME_MANIFEST_DIR="$BUILD_DIR/runtime"
mkdir -p "$RUNTIME_MANIFEST_DIR"
RUNTIME_MANIFEST_GENERATE_ARGS=(generate "$TARGET" "$RUNTIME_MANIFEST_DIR")
if [[ "$IO_PROTOCOL_FEATURES_SET" == 1 ]]; then
    RUNTIME_MANIFEST_GENERATE_ARGS+=("$IO_PROTOCOL_FEATURES")
fi
cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
    "${RUNTIME_MANIFEST_GENERATE_ARGS[@]}" >/dev/null
RUNTIME_MANIFEST_PATH="$RUNTIME_MANIFEST_DIR/runtime-manifest.json"
if [[ ! -s "$RUNTIME_MANIFEST_PATH" ]]; then
    echo -e "${RED}Error: feature-exact runtime manifest was not generated${NC}"
    exit 1
fi
if [[ "$IO_PROTOCOL_FEATURES_SET" == 1 ]]; then
    RUNTIME_CARGO_FEATURES=""
    IFS=',' read -ra SELECTED_IO_FEATURE_ARRAY <<< "$IO_PROTOCOL_FEATURES"
    for feature in "${SELECTED_IO_FEATURE_ARRAY[@]}"; do
        feature=$(echo "$feature" | xargs)
        [[ -z "$feature" ]] && continue
        RUNTIME_CARGO_FEATURES=$(add_csv_item \
            "$RUNTIME_CARGO_FEATURES" "aether-io/$feature")
    done
else
    RUNTIME_CARGO_FEATURES=$(cargo run --quiet -p aether-runtime-catalog \
        --bin aether-runtime-manifest -- print-default-features)
fi

# Step 1+2: Build Rust binaries and Docker images
echo ""
if csv_contains "$BUILD_IMAGES" "aetherems:latest"; then
    echo -e "${BLUE}[1/5] Building all Rust binaries for $ARCH...${NC}"

    # Check for cargo-zigbuild
    if ! command -v cargo-zigbuild &> /dev/null; then
        echo -e "${YELLOW}Installing cargo-zigbuild...${NC}"
        cargo install cargo-zigbuild
    fi

    # Check if rust target is installed
    if ! rustup target list --installed | grep -q "$TARGET"; then
        echo -e "${YELLOW}Installing $TARGET target...${NC}"
        rustup target add $TARGET
    fi

    # Build the CLI and six services. A custom IO feature selection is built
    # separately so `--no-default-features` applies only to aether-io.
    CARGO_FEATURES=""
    OTHER_CARGO_FEATURES=""
    IO_BUILD_FEATURES="$RUNTIME_CARGO_FEATURES"
    IFS=',' read -ra RUNTIME_FEATURE_ARRAY <<< "$RUNTIME_CARGO_FEATURES"
    for feature in "${RUNTIME_FEATURE_ARRAY[@]}"; do
        CARGO_FEATURES=$(add_csv_item "$CARGO_FEATURES" "$feature")
    done
    if [[ "$ENABLE_SWAGGER" == "1" ]]; then
        # Explicitly enable Swagger UI where it is feature-gated.
        for feature in \
            aether-io/swagger-ui \
            aether-automation/swagger-ui \
            aether-api/swagger-ui \
            aether-alarm/swagger-ui \
            aether-history/swagger-ui \
            aether-uplink/swagger-ui; do
            CARGO_FEATURES=$(add_csv_item "$CARGO_FEATURES" "$feature")
            if [[ "$feature" == aether-io/* ]]; then
                IO_BUILD_FEATURES=$(add_csv_item "$IO_BUILD_FEATURES" "$feature")
            else
                OTHER_CARGO_FEATURES=$(add_csv_item "$OTHER_CARGO_FEATURES" "$feature")
            fi
        done
    fi
    if csv_contains "$BUILD_IMAGES" "timescale/timescaledb:2.25.2-pg17"; then
        CARGO_FEATURES=$(add_csv_item \
            "$CARGO_FEATURES" "aether-history/postgres-storage")
        OTHER_CARGO_FEATURES=$(add_csv_item \
            "$OTHER_CARGO_FEATURES" "$(printf 'aether-%s/%s' history postgres-storage)")
    fi
    CARGO_FEATURE_ARGS=()
    [[ -n "$CARGO_FEATURES" ]] \
        && CARGO_FEATURE_ARGS=(--features "$CARGO_FEATURES")

    if [[ "$IO_PROTOCOL_FEATURES_SET" == 1 ]]; then
        # Cargo cannot disable defaults for only one member in a multi-package
        # invocation. Build IO separately so a trimmed adapter selection is
        # real, then build the remaining services with their normal defaults.
        IO_BUILD_FEATURES=$(add_csv_item "$IO_BUILD_FEATURES" "aether-io/openapi")
        IO_FEATURE_ARGS=()
        [[ -n "$IO_BUILD_FEATURES" ]] \
            && IO_FEATURE_ARGS=(--features "$IO_BUILD_FEATURES")
        CARGO_BUILD_JOBS=$CPU_CORES cargo zigbuild --release --target "$TARGET" \
            -p aether-io --no-default-features \
            ${IO_FEATURE_ARGS[@]+"${IO_FEATURE_ARGS[@]}"}

        OTHER_FEATURE_ARGS=()
        [[ -n "$OTHER_CARGO_FEATURES" ]] \
            && OTHER_FEATURE_ARGS=(--features "$OTHER_CARGO_FEATURES")
        CARGO_BUILD_JOBS=$CPU_CORES cargo zigbuild --release --target "$TARGET" \
            -p aether -p aether-automation -p aether-alarm -p aether-api -p aether-history -p aether-uplink \
            ${OTHER_FEATURE_ARGS[@]+"${OTHER_FEATURE_ARGS[@]}"}
    else
        CARGO_BUILD_JOBS=$CPU_CORES cargo zigbuild --release --target "$TARGET" \
            -p aether -p aether-io -p aether-automation -p aether-alarm -p aether-api -p aether-history -p aether-uplink \
            ${CARGO_FEATURE_ARGS[@]+"${CARGO_FEATURE_ARGS[@]}"}
    fi

    if [[ -f "$ROOT_DIR/target/$TARGET/release/aether" ]]; then
        cp "$ROOT_DIR/target/$TARGET/release/aether" "$BUILD_DIR/tools/"
        echo -e "${GREEN}✓ Built aether CLI${NC}"
    else
        echo -e "${RED}Error: Failed to build aether${NC}"
        exit 1
    fi

    chmod +x "$BUILD_DIR/tools/"* 2>/dev/null || true

    for service in aether-io aether-automation aether-alarm aether-api aether-history aether-uplink; do
        if [[ ! -f "$ROOT_DIR/target/$TARGET/release/$service" ]]; then
            echo -e "${RED}Error: Failed to build $service${NC}"
            exit 1
        fi
        echo -e "${GREEN}✓ Built $service${NC}"
    done

    if [[ "$BARE_METAL" == 1 ]]; then
        INCLUDE_REDIS_STATIC=0
        INCLUDE_FRONTEND_STATIC=0
        if csv_contains "$BUILD_IMAGES" "redis:8-alpine"; then
            INCLUDE_REDIS_STATIC=1
        fi
        if csv_contains "$BUILD_IMAGES" "aether-apps:latest"; then
            INCLUDE_FRONTEND_STATIC=1
        fi
        echo -e "${BLUE}[2/4] Building selected static dependencies...${NC}"
        INCLUDE_REDIS="$INCLUDE_REDIS_STATIC" \
            INCLUDE_NGINX="$INCLUDE_FRONTEND_STATIC" \
            ./scripts/build-static-deps.sh "$ARCH"

        if [[ "$INCLUDE_FRONTEND_STATIC" == "1" ]]; then
            echo -e "${BLUE}[3/4] Building optional frontend assets...${NC}"
            (cd apps && corepack enable && corepack prepare pnpm@latest --activate && pnpm install --frozen-lockfile && pnpm run build)
        else
            echo -e "${BLUE}[3/4] Skipping optional frontend assets.${NC}"
        fi

        echo -e "${BLUE}[4/4] Packaging bare-metal installer...${NC}"
        BM_PKG_DIR="$BUILD_DIR/baremetal-pkg"
        rm -rf "$BM_PKG_DIR"
        mkdir -p "$BM_PKG_DIR/bin" "$BM_PKG_DIR/systemd" "$BM_PKG_DIR/config.template" "$BM_PKG_DIR/script-host"

        for svc in aether aether-io aether-automation aether-history aether-api aether-uplink aether-alarm; do
            cp "target/$TARGET/release/$svc" "$BM_PKG_DIR/bin/$svc"
        done
        # Keep in sync with scripts/build-static-deps.sh's REDIS_VERSION/NGINX_VERSION defaults.
        if [[ "$INCLUDE_REDIS_STATIC" == "1" ]]; then
            cp "build/cache/static-deps/redis-server-${REDIS_VERSION:-8.0.2}-$ARCH/redis-server" "$BM_PKG_DIR/bin/redis-server"
            cp "build/cache/static-deps/redis-server-${REDIS_VERSION:-8.0.2}-$ARCH/redis-cli" "$BM_PKG_DIR/bin/redis-cli"
        fi
        if [[ "$INCLUDE_FRONTEND_STATIC" == "1" ]]; then
            mkdir -p "$BM_PKG_DIR/apps-dist"
            cp "build/cache/static-deps/nginx-${NGINX_VERSION:-1.27.4}-$ARCH/nginx" "$BM_PKG_DIR/bin/nginx"
            cp -r apps/dist/. "$BM_PKG_DIR/apps-dist/"
            cp apps/nginx.conf "$BM_PKG_DIR/nginx.conf"
        fi
        cp scripts/systemd/*.service scripts/systemd/*.target "$BM_PKG_DIR/systemd/"
        if [[ "$INCLUDE_REDIS_STATIC" != "1" ]]; then
            rm -f "$BM_PKG_DIR/systemd/aether-redis.service"
        fi
        if [[ "$INCLUDE_FRONTEND_STATIC" != "1" ]]; then
            rm -f "$BM_PKG_DIR/systemd/aether-apps.service"
        fi
        cp scripts/install-baremetal.sh "$BM_PKG_DIR/install.sh"
        cp libs/aether-script-host/main.py "$BM_PKG_DIR/script-host/main.py"
        cp "$LICENSE_FILE" "$BM_PKG_DIR/LICENSE"
        cp "$NOTICE_FILE" "$BM_PKG_DIR/NOTICE"
        find config.template -type f \( -name "*.yaml" -o -name "*.yml" -o -name "*.csv" -o -name "*.json" \) | while read -r f; do
            mkdir -p "$BM_PKG_DIR/$(dirname "$f")"
            cp "$f" "$BM_PKG_DIR/$f"
        done
        cp "$RUNTIME_MANIFEST_PATH" "$BM_PKG_DIR/config.template/runtime-manifest.json"

        chmod +x "$BM_PKG_DIR/bin/"* "$BM_PKG_DIR/install.sh"

        BM_VARIANT=""
        if [[ "$INCLUDE_FRONTEND_STATIC" == "1" ]]; then
            BM_VARIANT="${BM_VARIANT}-frontend"
        fi
        if [[ "$INCLUDE_REDIS_STATIC" == "1" ]]; then
            BM_VARIANT="${BM_VARIANT}-redis"
        fi
        BM_OUTPUT_NAME="AetherEdge-baremetal-${ARCH}-${VERSION}${BM_VARIANT}.run"
        makeself --gzip "$BM_PKG_DIR" "$OUTPUT_DIR/$BM_OUTPUT_NAME" \
            "AetherEMS bare-metal installer ($ARCH, $VERSION)" \
            bash ./install.sh
        verify_installer_license "$OUTPUT_DIR/$BM_OUTPUT_NAME"

        echo -e "${GREEN}Bare-metal installer: $OUTPUT_DIR/$BM_OUTPUT_NAME${NC}"
        exit 0
    fi

    # Step 2: Build aetherems Docker image using pre-compiled binaries
    echo ""
    echo -e "${BLUE}[2/5] Building Docker images for $ARCH...${NC}"
    if [[ -n "$DEV_SERVICE" ]]; then
        # Dev mode: tag as aetherems:dev-<service>, do NOT overwrite aetherems:latest
        _DEV_TAG="aetherems:dev-${DEV_SERVICE}"
        echo -e "${BLUE}Building AetherEMS Docker image (dev tag: ${_DEV_TAG})...${NC}"
        if docker build --platform $DOCKER_PLATFORM \
            --build-arg TARGET_TRIPLE=$TARGET \
            --build-arg RUNTIME_MANIFEST_PATH=build/installer/runtime/runtime-manifest.json \
            -f "$ROOT_DIR/Dockerfile" \
            -t "$_DEV_TAG" \
            "$ROOT_DIR"; then
            docker save "$_DEV_TAG" | gzip > "$BUILD_DIR/docker/aetherems.tar.gz"
            sync
            echo -e "${GREEN}✓ Saved aetherems.tar.gz (tag: ${_DEV_TAG})${NC}"
        else
            echo -e "${RED}Error: Docker build failed${NC}"
            exit 1
        fi
    else
        echo -e "${BLUE}Building AetherEMS Docker image (all services)...${NC}"
        if docker build --platform $DOCKER_PLATFORM \
            --build-arg TARGET_TRIPLE=$TARGET \
            --build-arg RUNTIME_MANIFEST_PATH=build/installer/runtime/runtime-manifest.json \
            -f "$ROOT_DIR/Dockerfile" \
            -t aetherems:latest \
            "$ROOT_DIR"; then
            docker save aetherems:latest | gzip > "$BUILD_DIR/docker/aetherems.tar.gz"
            sync
            echo -e "${GREEN}✓ Saved aetherems.tar.gz${NC}"
        else
            echo -e "${RED}Error: Docker build failed${NC}"
            exit 1
        fi
    fi
else
    echo -e "${YELLOW}[1/5] Skipping Rust binaries (aetherems not selected)${NC}"
    echo ""
    echo -e "${BLUE}[2/5] Building Docker images for $ARCH...${NC}"
fi

# Build Frontend if needed
if csv_contains "$BUILD_IMAGES" "aether-apps:latest"; then
    echo -e "${BLUE}Building Frontend (Vue.js)...${NC}"
    FRONTEND_DOCKERFILE="$ROOT_DIR/apps/Dockerfile"
    if [[ -f "$FRONTEND_DOCKERFILE" ]]; then
        echo -e "${BLUE}Building aether-apps:latest for $ARCH...${NC}"
        if docker build --platform $DOCKER_PLATFORM \
            -f "$FRONTEND_DOCKERFILE" \
            -t aether-apps:latest \
            "$ROOT_DIR/apps"; then
            docker save aether-apps:latest | gzip > "$BUILD_DIR/docker/apps.tar.gz"
            sync
            size=$(ls -lh "$BUILD_DIR/docker/apps.tar.gz" | awk '{print $5}')
            echo -e "${GREEN}✓ Saved apps.tar.gz ($size)${NC}"
        else
            echo -e "${YELLOW}Warning: Frontend build failed, continuing without frontend...${NC}"
        fi
    else
        echo -e "${YELLOW}Warning: Frontend Dockerfile not found at $FRONTEND_DOCKERFILE${NC}"
        echo -e "${YELLOW}Skipping frontend build...${NC}"
    fi
else
    echo -e "${YELLOW}⊘ Skipping Frontend (not selected)${NC}"
fi

# Pull official images if needed
echo -e "${BLUE}Pulling official images...${NC}"
if csv_contains "$BUILD_IMAGES" "redis:8-alpine"; then
    pull_and_save_image "redis:8-alpine" "aether-redis.tar.gz"
else
    echo -e "${YELLOW}⊘ Skipping redis:8-alpine (not selected)${NC}"
fi

if csv_contains "$BUILD_IMAGES" "timescale/timescaledb:2.25.2-pg17"; then
    pull_and_save_image "timescale/timescaledb:2.25.2-pg17" "aether-timescaledb.tar.gz"
else
    echo -e "${YELLOW}⊘ Skipping timescaledb (not selected)${NC}"
fi

# Verify images
echo -e "${YELLOW}Verifying Docker images...${NC}"

# Build list of expected images based on what was selected
EXPECTED_IMAGES=""
if csv_contains "$BUILD_IMAGES" "aetherems:latest"; then
    EXPECTED_IMAGES=$(add_csv_item "$EXPECTED_IMAGES" "aetherems")
fi
if csv_contains "$BUILD_IMAGES" "aether-apps:latest"; then
    EXPECTED_IMAGES=$(add_csv_item "$EXPECTED_IMAGES" "apps")
fi
if csv_contains "$BUILD_IMAGES" "redis:8-alpine"; then
    EXPECTED_IMAGES=$(add_csv_item "$EXPECTED_IMAGES" "aether-redis")
fi
if csv_contains "$BUILD_IMAGES" "timescale/timescaledb:2.25.2-pg17"; then
    EXPECTED_IMAGES=$(add_csv_item "$EXPECTED_IMAGES" "aether-timescaledb")
fi

# Verify only the images that should exist
IFS=',' read -ra EXPECTED_IMAGES_ARRAY <<< "$EXPECTED_IMAGES"
for img in "${EXPECTED_IMAGES_ARRAY[@]}"; do
    if [[ ! -f "$BUILD_DIR/docker/$img.tar.gz" ]]; then
        echo -e "${RED}✗ $img.tar.gz not found!${NC}"
        exit 1
    fi
    size=$(ls -lh "$BUILD_DIR/docker/$img.tar.gz" | awk '{print $5}')
    echo -e "${GREEN}✓ $img.tar.gz ($size)${NC}"
done

# Show summary
echo ""
echo -e "${GREEN}[DONE] Docker images prepared${NC}"
if [[ -n "$SELECTED_SERVICES" ]]; then
    echo -e "${YELLOW}Built ${#EXPECTED_IMAGES_ARRAY[@]} image(s) for selected services${NC}"
else
    echo -e "${GREEN}Built all ${#EXPECTED_IMAGES_ARRAY[@]} image(s)${NC}"
fi

# Step 3: Copy configuration templates
echo ""
echo -e "${BLUE}[3/5] Copying configuration templates...${NC}"

if [[ -d "$ROOT_DIR/config.template" ]]; then
    copy_config_files "$ROOT_DIR/config.template" "$BUILD_DIR/config.template"
    cp "$RUNTIME_MANIFEST_PATH" "$BUILD_DIR/config.template/runtime-manifest.json"
    echo -e "${GREEN}✓ Copied config.template${NC}"
fi

[[ -f "$ROOT_DIR/docker-compose.yml" ]] && cp "$ROOT_DIR/docker-compose.yml" "$BUILD_DIR/"

echo -e "${GREEN}[DONE] Configuration templates copied${NC}"

# Step 4: Copy and customize installation script
echo ""
echo -e "${BLUE}[4/5] Copying installation script...${NC}"

if [[ -f "$ROOT_DIR/scripts/install.sh" ]]; then
    cp "$ROOT_DIR/scripts/install.sh" "$BUILD_DIR/install.sh"
    chmod +x "$BUILD_DIR/install.sh"

    # Customize script for target architecture by setting variables
    if [[ "$ARCH" != "arm64" ]]; then
        echo -e "${YELLOW}Customizing install.sh for ${ARCH_LABEL}...${NC}"
        case "$ARCH" in
            amd64)
                sed -i.bak \
                    -e 's/^INSTALLER_ARCH_LABEL=.*/INSTALLER_ARCH_LABEL="AMD64"/' \
                    -e 's/^INSTALLER_ARCH_UNAME=.*/INSTALLER_ARCH_UNAME="x86_64"/' \
                    -e 's/^INSTALLER_ARCH_SHORT=.*/INSTALLER_ARCH_SHORT="amd64"/' \
                    "$BUILD_DIR/install.sh"
                rm -f "$BUILD_DIR/install.sh.bak"
                ;;
        esac
    fi
    echo -e "${GREEN}[DONE] Installation script copied${NC}"
else
    echo -e "${RED}Error: install.sh not found${NC}"
    exit 1
fi

# Step 5: Create self-extracting installer / dev deploy package
echo ""
echo -e "${BLUE}[5/5] Creating self-extracting package...${NC}"

TEMP_PKG_DIR="/tmp/AetherEdge-temp-$$"
mkdir -p "$TEMP_PKG_DIR"
cp "$LICENSE_FILE" "$TEMP_PKG_DIR/LICENSE"
cp "$NOTICE_FILE" "$TEMP_PKG_DIR/NOTICE"

[[ -d "$BUILD_DIR/docker" ]] && copy_docker_images "$BUILD_DIR/docker" "$TEMP_PKG_DIR/docker"

if [[ -n "$DEV_SERVICE" ]]; then
    # ── Dev mode: lightweight deploy script ──────────────────────────────────
    # Loads aetherems:dev-<service> and restarts ONLY that one container.
    # aetherems:latest and all other containers are untouched.
    _DEV_TAG="aetherems:dev-${DEV_SERVICE}"
    _COMPOSE_FILE="/opt/AetherEdge/docker-compose.yml"

    cat > "$TEMP_PKG_DIR/deploy.sh" << DEPLOY_EOF
#!/usr/bin/env bash
set -euo pipefail
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
DEV_SERVICE="${DEV_SERVICE}"
DEV_TAG="${_DEV_TAG}"
DEV_SERVICES="${DEV_SERVICES_LIST}"
COMPOSE_FILE="${_COMPOSE_FILE}"

echo -e "\${BLUE}================================================\${NC}"
echo -e "\${BLUE}  AetherEMS Dev Deploy: \${DEV_TAG}\${NC}"
echo -e "\${BLUE}================================================\${NC}"
echo -e "Image:      \${YELLOW}\${DEV_TAG}\${NC}"
echo -e "Containers: \${YELLOW}\${DEV_SERVICES}\${NC}"
echo ""

echo -e "\${BLUE}Loading image...\${NC}"
docker load < docker/aetherems.tar.gz
echo -e "\${GREEN}✓ Loaded \${DEV_TAG}\${NC}"

if [[ ! -f "\$COMPOSE_FILE" ]]; then
    echo -e "\${RED}Error: docker-compose.yml not found at \$COMPOSE_FILE\${NC}"
    echo -e "\${YELLOW}Is AetherEMS fully installed on this machine?\${NC}"
    exit 1
fi

# Detect compose command: prefer plugin form, fall back to standalone
if docker compose version &>/dev/null 2>&1; then
    _COMPOSE="docker compose"
elif command -v docker-compose &>/dev/null; then
    _COMPOSE="docker-compose"
else
    echo -e "\${RED}Error: neither 'docker compose' nor 'docker-compose' found\${NC}"
    exit 1
fi

# Generate a temporary override file so the exact dev image is used regardless
# of whether the installed docker-compose.yml supports the IMAGE_TAG variable.
_OVERRIDE=$(mktemp /tmp/dev-override-XXXXXX.yml)
cat > "\$_OVERRIDE" << OVERRIDE_EOF
services:
OVERRIDE_EOF
for svc in \$DEV_SERVICES; do
    printf "  %s:\n    image: %s\n" "\$svc" "\$DEV_TAG" >> "\$_OVERRIDE"
done

echo -e "\${BLUE}Restarting containers: \${DEV_SERVICES}...\${NC}"
# shellcheck disable=SC2086
\$_COMPOSE -f "\$COMPOSE_FILE" -f "\$_OVERRIDE" up --no-deps -d \$DEV_SERVICES
rm -f "\$_OVERRIDE"
echo -e "\${GREEN}✓ Done — containers are now running \${DEV_TAG}\${NC}"
echo ""
for svc in \$DEV_SERVICES; do
    echo -e "  Logs: \${YELLOW}docker logs -f \${svc}\${NC}"
done
echo -e "  Stop: \${YELLOW}\$_COMPOSE -f \$COMPOSE_FILE stop \$DEV_SERVICES\${NC}"
DEPLOY_EOF
    chmod +x "$TEMP_PKG_DIR/deploy.sh"

    INSTALLER_DESC="AetherEMS DEV ${ARCH_LABEL} — ${DEV_SERVICE} ${VERSION}"
    makeself --gzip "$TEMP_PKG_DIR" "$OUTPUT_DIR/${PACKAGE_NAME}.run" \
        "$INSTALLER_DESC" \
        bash ./deploy.sh
else
    # ── Normal mode: full installer ───────────────────────────────────────────
    cp "$BUILD_DIR/install.sh" "$TEMP_PKG_DIR/"
    chmod +x "$TEMP_PKG_DIR/install.sh"

    [[ -d "$BUILD_DIR/config.template" ]] && copy_config_files "$BUILD_DIR/config.template" "$TEMP_PKG_DIR/config.template"

    mkdir -p "$TEMP_PKG_DIR/tools"
    if [[ -f "$BUILD_DIR/tools/aether" ]]; then
        cp "$BUILD_DIR/tools/aether" "$TEMP_PKG_DIR/tools/"
        echo -e "${GREEN}✓ Included aether CLI${NC}"
    elif csv_contains "$BUILD_IMAGES" "aetherems:latest"; then
        echo -e "${RED}Error: aether binary not found but Rust services are selected${NC}"
        rm -rf "$TEMP_PKG_DIR"
        exit 1
    else
        echo -e "${YELLOW}⊘ Skipping aether CLI (no Rust services selected)${NC}"
    fi

    [[ -f "$BUILD_DIR/docker-compose.yml" ]] && cp "$BUILD_DIR/docker-compose.yml" "$TEMP_PKG_DIR/"

    # Include dpkg packages if present
    if [[ -d "$ROOT_DIR/dpkg" ]]; then
        cp -r "$ROOT_DIR/dpkg" "$TEMP_PKG_DIR/dpkg"
        chmod +x "$TEMP_PKG_DIR/dpkg/"*.sh 2>/dev/null || true
        echo -e "${GREEN}✓ Included dpkg packages ($(ls "$ROOT_DIR/dpkg/"*.deb 2>/dev/null | wc -l) .deb file(s))${NC}"
    fi

    if [[ -n "$SELECTED_SERVICES" ]]; then
        INSTALLER_DESC="AetherEMS ${ARCH_LABEL} Partial Update ($SELECTED_SERVICES) $VERSION"
    else
        INSTALLER_DESC="AetherEMS ${ARCH_LABEL} Full Installer $VERSION"
    fi

    makeself --gzip "$TEMP_PKG_DIR" "$OUTPUT_DIR/${PACKAGE_NAME}.run" \
        "$INSTALLER_DESC" \
        bash ./install.sh
fi

verify_installer_license "$OUTPUT_DIR/${PACKAGE_NAME}.run"

rm -rf "$TEMP_PKG_DIR"

RUN_SIZE=$(ls -lh "$OUTPUT_DIR/${PACKAGE_NAME}.run" 2>/dev/null | awk '{print $5}')

echo ""
echo -e "${GREEN}================================================${NC}"
echo -e "${GREEN}       Build Complete!                          ${NC}"
echo -e "${GREEN}================================================${NC}"
echo ""
echo -e "Package: ${GREEN}$OUTPUT_DIR/${PACKAGE_NAME}.run${NC} (${YELLOW}$RUN_SIZE${NC})"
if [[ -n "$DEV_SERVICE" ]]; then
    echo -e "Type:       ${YELLOW}Dev Deploy${NC}"
    echo -e "Image:      ${YELLOW}aetherems:dev-${DEV_SERVICE}${NC}"
    echo -e "Containers: ${YELLOW}${DEV_SERVICES_LIST}${NC}"
    echo -e "            ${YELLOW}aetherems:latest NOT affected${NC}"
elif [[ -n "$SELECTED_SERVICES" ]]; then
    echo -e "Type:    ${YELLOW}Partial Update${NC}"
    echo -e "Services: ${YELLOW}$SELECTED_SERVICES${NC}"
    echo -e "Images:   ${YELLOW}${BUILD_IMAGES}${NC}"
else
    echo -e "Type:    ${GREEN}Full Installation${NC}"
fi
echo ""
echo "Deploy:"
echo "  scp $OUTPUT_DIR/${PACKAGE_NAME}.run user@testmachine:/tmp/"
echo "  ssh user@testmachine 'chmod +x /tmp/${PACKAGE_NAME}.run && sudo /tmp/${PACKAGE_NAME}.run'"
echo ""

# Cleanup
rm -rf "$BUILD_DIR"
echo -e "${GREEN}Done!${NC}"
