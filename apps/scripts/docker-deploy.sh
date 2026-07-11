#!/usr/bin/env bash
# ============================================================
# AetherEMS 前端 Docker 部署脚本（Linux / macOS）
#
# 用法：
#   ./scripts/docker-deploy.sh [选项]
#
# 模式（三选一）：
#   默认                    本地运行（当前平台，--load）
#   --export                构建 ARM64 镜像并导出为 .tar 文件（可 scp 到 ARM64 服务器）
#   --push --registry <R>   构建 amd64+arm64 并推送到镜像仓库
#
# 选项：
#   --tag <TAG>             镜像标签（默认 latest）
#   --platform <PLAT>       目标平台，默认根据模式自动选择
#   --no-cache              完整重建（不使用缓存）
#   --registry <R>          仓库地址前缀，如 docker.io/myuser 或 192.168.1.10:5000
#   --remote-host <H>       export 模式下自动 scp + 加载到远程主机（如 root@192.168.30.21）
#   --push                  推送到 registry（需配合 --registry）
#
# 示例：
#   ./scripts/docker-deploy.sh                              # 本地 amd64 运行
#   ./scripts/docker-deploy.sh --export                     # 导出 arm64 tar
#   ./scripts/docker-deploy.sh --export --remote-host root@192.168.30.21  # 导出并部署到远程
#   ./scripts/docker-deploy.sh --push --registry docker.io/myuser         # 多架构推送
# ============================================================
set -euo pipefail

# ── 默认配置 ──────────────────────────────────────────────────────────────────
IMAGE_NAME="aether-apps"
CONTAINER_NAME="aether-apps"
HOST_PORT=8080
TAG="latest"
NO_CACHE=""
MODE="local"          # local | export | push
PLATFORM=""           # 由模式自动决定（可手动覆盖）
REGISTRY=""
REMOTE_HOST=""
BUILDER_NAME="aether-multiarch-builder"

# ── 解析参数 ──────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag)       TAG="$2";          shift 2 ;;
        --platform)  PLATFORM="$2";     shift 2 ;;
        --no-cache)  NO_CACHE="--no-cache"; shift ;;
        --registry)  REGISTRY="$2";     shift 2 ;;
        --remote-host) REMOTE_HOST="$2"; shift 2 ;;
        --push)      MODE="push";        shift ;;
        --export)    MODE="export";      shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# ── 根据模式设置平台 ───────────────────────────────────────────────────────────
if [[ -z "$PLATFORM" ]]; then
    case "$MODE" in
        local)  PLATFORM="linux/$(uname -m | sed 's/x86_64/amd64/')" ;;
        export) PLATFORM="linux/arm64" ;;
        push)   PLATFORM="linux/amd64,linux/arm64" ;;
    esac
fi

# ── 镜像全名 ──────────────────────────────────────────────────────────────────
if [[ -n "$REGISTRY" ]]; then
    FULL_IMAGE="${REGISTRY}/${IMAGE_NAME}:${TAG}"
else
    FULL_IMAGE="${IMAGE_NAME}:${TAG}"
fi

EXPORT_TAR="$(pwd)/${IMAGE_NAME}-arm64-${TAG}.tar"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPS_DIR="$(dirname "$SCRIPT_DIR")"

# ── 颜色输出 ──────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'
RED='\033[0;31m'; GRAY='\033[0;37m'; NC='\033[0m'

step()  { echo -e "\n${YELLOW}[$1] $2${NC}"; }
ok()    { echo -e "  ${GREEN}✓ $1${NC}"; }
info()  { echo -e "  ${GRAY}· $1${NC}"; }
fail()  { echo -e "${RED}[错误] $1${NC}"; exit 1; }

echo ""
echo -e "${CYAN}================================================${NC}"
echo -e "${CYAN}   AetherEMS 前端 Docker 部署${NC}"
echo -e "${CYAN}   模式: ${MODE}  平台: ${PLATFORM}${NC}"
echo -e "${CYAN}================================================${NC}"

# ── 前置检查 ──────────────────────────────────────────────────────────────────
step "0/5" "检查环境"
docker info >/dev/null 2>&1 || fail "Docker 未运行，请先启动 Docker"
docker buildx version >/dev/null 2>&1 || fail "当前 Docker 版本不支持 buildx，请升级到 Docker 20.10+"
ok "Docker 与 buildx 就绪"

[ -f "${APPS_DIR}/Dockerfile" ] || fail "在 ${APPS_DIR} 中未找到 Dockerfile"

# ── Step 1：准备 buildx builder ───────────────────────────────────────────────
step "1/5" "准备多架构 Builder: ${BUILDER_NAME}"

# 如果是本地单平台部署，使用默认 builder 即可（避免不必要的 docker-container driver）
if [[ "$MODE" == "local" ]]; then
    info "本地单平台模式，使用默认 builder"
else
    # 检查是否已存在可用 builder
    if ! docker buildx ls | grep -q "^${BUILDER_NAME}"; then
        info "创建新 builder（docker-container driver，支持跨平台）"
        docker buildx create \
            --name "$BUILDER_NAME" \
            --driver docker-container \
            --driver-opt network=host \
            --use
        docker buildx inspect --bootstrap >/dev/null 2>&1
    else
        docker buildx use "$BUILDER_NAME"
        info "复用已有 builder: ${BUILDER_NAME}"
    fi
    ok "Builder 就绪"
fi

# ── Step 2：清理旧容器（仅本地模式） ────────────────────────────────────────────
if [[ "$MODE" == "local" ]]; then
    step "2/5" "清理端口 ${HOST_PORT} 上的旧容器"
    CONTAINERS=$(docker ps -a --filter "publish=${HOST_PORT}" --format "{{.ID}} {{.Names}}" 2>/dev/null || true)
    if [ -n "$CONTAINERS" ]; then
        while IFS=' ' read -r cid cname; do
            [ -z "$cid" ] && continue
            info "停止容器: ${cname} (${cid})"
            docker stop "$cid" >/dev/null
            docker rm   "$cid" >/dev/null
            ok "已移除: ${cname}"
        done <<< "$CONTAINERS"
    else
        BY_NAME=$(docker ps -a --filter "name=^${CONTAINER_NAME}$" --format "{{.ID}}" 2>/dev/null || true)
        if [ -n "$BY_NAME" ]; then
            docker stop "$BY_NAME" >/dev/null
            docker rm   "$BY_NAME" >/dev/null
            ok "已移除: ${CONTAINER_NAME}"
        else
            info "无旧容器，跳过清理"
        fi
    fi
else
    step "2/5" "跳过容器清理（非本地模式）"
    info "export / push 模式不需要本地清理"
fi

# ── Step 3：构建镜像 ──────────────────────────────────────────────────────────
step "3/5" "构建 Docker 镜像"
info "镜像: ${FULL_IMAGE}"
info "平台: ${PLATFORM}"

BUILD_OPTS=(
    "buildx" "build"
    "--platform" "$PLATFORM"
    "-t" "$FULL_IMAGE"
    $NO_CACHE
)

case "$MODE" in
    local)
        # 加载到本地 daemon（仅支持单平台）
        BUILD_OPTS+=("--load")
        info "输出: 加载到本地 Docker daemon"
        ;;
    export)
        # 导出为 Docker tar 格式（仅支持单平台）
        BUILD_OPTS+=("--output" "type=docker,dest=${EXPORT_TAR}")
        info "输出: ${EXPORT_TAR}"
        ;;
    push)
        # 推送到远程 registry（支持多平台）
        [[ -z "$REGISTRY" ]] && fail "push 模式需要指定 --registry"
        BUILD_OPTS+=("--push")
        info "输出: 推送到 ${REGISTRY}"
        ;;
esac

BUILD_OPTS+=("$APPS_DIR")

docker "${BUILD_OPTS[@]}"
ok "构建完成: ${FULL_IMAGE}"

# ── Step 4：部署 ──────────────────────────────────────────────────────────────
step "4/5" "部署"

case "$MODE" in
    local)
        docker run -d \
            --name "$CONTAINER_NAME" \
            --restart unless-stopped \
            -p "${HOST_PORT}:8080" \
            "$FULL_IMAGE" >/dev/null
        ok "容器已启动: ${CONTAINER_NAME}"
        ;;
    export)
        ok "镜像已导出: ${EXPORT_TAR}"
        if [[ -n "$REMOTE_HOST" ]]; then
            info "正在上传到 ${REMOTE_HOST}..."
            scp "$EXPORT_TAR" "${REMOTE_HOST}:/tmp/${IMAGE_NAME}-arm64.tar"
            ok "上传完成"
            info "在远程主机上加载并运行..."
            ssh "$REMOTE_HOST" bash -s -- "$IMAGE_NAME" "$TAG" "$HOST_PORT" <<'REMOTE_SCRIPT'
IMAGE=$1; TAG=$2; PORT=$3; FULL="${IMAGE}:${TAG}"
docker stop "$IMAGE" 2>/dev/null || true
docker rm   "$IMAGE" 2>/dev/null || true
docker load -i "/tmp/${IMAGE}-arm64.tar"
docker run -d --name "$IMAGE" --restart unless-stopped -p "${PORT}:8080" "$FULL"
echo "Remote container started: $IMAGE"
REMOTE_SCRIPT
            ok "远程部署完成"
        else
            echo ""
            echo -e "${GRAY}将镜像部署到 ARM64 服务器：${NC}"
            echo -e "${GRAY}  scp ${EXPORT_TAR} root@<ARM64_HOST>:/tmp/${NC}"
            echo -e "${GRAY}  ssh root@<ARM64_HOST> 'docker load -i /tmp/${IMAGE_NAME}-arm64-${TAG}.tar && docker run -d --name ${CONTAINER_NAME} -p ${HOST_PORT}:8080 ${FULL_IMAGE}'${NC}"
        fi
        ;;
    push)
        ok "多架构镜像已推送: ${FULL_IMAGE}"
        echo ""
        echo -e "${GRAY}在 ARM64 服务器上运行：${NC}"
        echo -e "${GRAY}  docker pull ${FULL_IMAGE}${NC}"
        echo -e "${GRAY}  docker run -d --name ${CONTAINER_NAME} -p ${HOST_PORT}:8080 ${FULL_IMAGE}${NC}"
        ;;
esac

# ── Step 5：验证 ──────────────────────────────────────────────────────────────
step "5/5" "完成"

if [[ "$MODE" == "local" ]]; then
    sleep 2
    STATUS=$(docker ps --filter "name=^${CONTAINER_NAME}$" --filter "status=running" --format "{{.Status}}" 2>/dev/null || true)
    if [ -n "$STATUS" ]; then
        ok "容器状态: ${STATUS}"
        echo ""
        echo -e "${CYAN}================================================${NC}"
        echo -e "${CYAN}   部署成功！访问: http://localhost:${HOST_PORT}${NC}"
        echo -e "${CYAN}================================================${NC}"
        echo ""
        echo -e "${GRAY}  查看日志 : docker logs -f ${CONTAINER_NAME}${NC}"
        echo -e "${GRAY}  进入容器 : docker exec -it ${CONTAINER_NAME} sh${NC}"
        echo -e "${GRAY}  停止容器 : docker stop ${CONTAINER_NAME}${NC}"
    else
        echo ""
        echo -e "${YELLOW}[警告] 容器可能未正常运行，最近日志：${NC}"
        docker logs --tail 20 "$CONTAINER_NAME" || true
        fail "请检查上方日志排查问题"
    fi
else
    echo ""
    echo -e "${CYAN}================================================${NC}"
    echo -e "${CYAN}   构建成功！镜像: ${FULL_IMAGE}${NC}"
    echo -e "${CYAN}   平台: ${PLATFORM}${NC}"
    echo -e "${CYAN}================================================${NC}"
fi
