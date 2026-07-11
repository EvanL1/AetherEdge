#!/usr/bin/env bash
# Cross-compile optional static dependencies for bare-metal Aether installs.
# Set INCLUDE_NGINX=1 for the browser-client bundle and INCLUDE_REDIS=1 for
# the Redis extension bundle. Runs on the build machine (still requires Docker
# when either component is selected); targets
# get zero new system dependencies. Results are cached under
# build/cache/static-deps/<name>-<version>-<arch>/ so repeat builds are
# instant once warm.
#
# Redis has no `BUILD_STATIC` make variable (despite the name existing in
# some build folklore) -- true static linking comes from LDFLAGS=-static,
# verified with `file` against the built binary (must say "statically
# linked", not "dynamically linked, interpreter /lib/ld-musl-*"). A
# dynamically-linked-against-musl binary would fail to run on any
# glibc-based target (Debian/Ubuntu/RHEL), defeating the entire point of
# this script.
set -euo pipefail
cd "$(dirname "$0")/.."

# Keep in sync with scripts/build-installer.sh's REDIS_VERSION/NGINX_VERSION defaults
REDIS_VERSION="${REDIS_VERSION:-8.0.2}"
NGINX_VERSION="${NGINX_VERSION:-1.27.4}"
ARCH="${1:-arm64}"
INCLUDE_REDIS="${INCLUDE_REDIS:-0}"
INCLUDE_NGINX="${INCLUDE_NGINX:-0}"
REDIS_SHA256="${REDIS_SHA256:-}"
NGINX_SHA256="${NGINX_SHA256:-}"

# Digests are for the exact release archives downloaded below, not GitHub's
# separately generated source archives. Version overrides must provide their
# own independently verified digest through the matching environment variable.
if [[ -z "$REDIS_SHA256" && "$REDIS_VERSION" == "8.0.2" ]]; then
  REDIS_SHA256="e9296b67b54c91befbcca046d67071c780a1f7c9f9e1ae5ed94773c3bb9b542f"
fi
if [[ -z "$NGINX_SHA256" && "$NGINX_VERSION" == "1.27.4" ]]; then
  NGINX_SHA256="294816f879b300e621fa4edd5353dd1ec00badb056399eceb30de7db64b753b2"
fi

case "$ARCH" in
  arm64) DOCKER_PLATFORM="linux/arm64" ;;
  amd64) DOCKER_PLATFORM="linux/amd64" ;;
  *) echo "Usage: $0 [arm64|amd64]" >&2; exit 1 ;;
esac

if [[ ! "$REDIS_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ \
    || ! "$NGINX_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Redis and nginx versions must use an exact numeric x.y.z release" >&2
  exit 1
fi

require_trusted_sha256() {
  local digest=$1
  local component=$2

  if [[ ! "$digest" =~ ^[[:xdigit:]]{64}$ ]]; then
    echo "$component download requires a trusted 64-character SHA-256 digest." >&2
    echo "Set ${component}_SHA256 from a separately verified release manifest." >&2
    return 1
  fi
}

validate_static_elf() {
  local binary_path=$1
  local architecture=$2
  local component=$3
  local description

  if [[ ! -f "$binary_path" || ! -x "$binary_path" || -L "$binary_path" ]]; then
    echo "$component is missing, non-executable, or a symlink: $binary_path" >&2
    return 1
  fi
  description=$(file -b "$binary_path") || return 1
  if [[ "$description" != *ELF* ]]; then
    echo "$component is not an ELF executable: $description" >&2
    return 1
  fi
  if [[ "$description" != *"statically linked"* \
      && "$description" != *"static-pie linked"* ]]; then
    echo "$component is not statically linked: $description" >&2
    return 1
  fi
  case "$architecture" in
    arm64)
      [[ "$description" == *"ARM aarch64"* || "$description" == *aarch64* ]] || {
        echo "$component is not an ARM64 ELF: $description" >&2
        return 1
      }
      ;;
    amd64)
      [[ "$description" == *"x86-64"* || "$description" == *x86_64* ]] || {
        echo "$component is not an AMD64 ELF: $description" >&2
        return 1
      }
      ;;
  esac
}

validate_cache_provenance() {
  local marker_path=$1
  local expected_digest=$2
  local component=$3
  local recorded_digest

  if [[ ! -f "$marker_path" || -L "$marker_path" ]]; then
    echo "$component cache has no trusted source-digest marker: $marker_path" >&2
    return 1
  fi
  recorded_digest=$(tr -d '\r\n' < "$marker_path" | tr '[:upper:]' '[:lower:]')
  expected_digest=$(printf '%s' "$expected_digest" | tr '[:upper:]' '[:lower:]')
  if [[ "$recorded_digest" != "$expected_digest" ]]; then
    echo "$component cache was built from a different or unverified source digest" >&2
    return 1
  fi
}

if [[ "$INCLUDE_REDIS" == "1" || "$INCLUDE_NGINX" == "1" ]]; then
  command -v file >/dev/null 2>&1 || {
    echo "The 'file' utility is required to verify static dependency artifacts" >&2
    exit 1
  }
fi

CACHE_DIR="$(pwd)/build/cache/static-deps"
REDIS_OUT="$CACHE_DIR/redis-server-$REDIS_VERSION-$ARCH"
NGINX_OUT="$CACHE_DIR/nginx-$NGINX_VERSION-$ARCH"
mkdir -p "$CACHE_DIR"

if [[ "$INCLUDE_REDIS" == "1" ]]; then
  require_trusted_sha256 "$REDIS_SHA256" REDIS
  if [[ -e "$REDIS_OUT" || -L "$REDIS_OUT" ]]; then
    [[ -d "$REDIS_OUT" && ! -L "$REDIS_OUT" ]] || {
      echo "Redis cache path is not a regular directory: $REDIS_OUT" >&2
      exit 1
    }
    validate_cache_provenance \
      "$REDIS_OUT/.source-sha256" "$REDIS_SHA256" Redis
    validate_static_elf "$REDIS_OUT/redis-server" "$ARCH" redis-server
    validate_static_elf "$REDIS_OUT/redis-cli" "$ARCH" redis-cli
    echo "redis-server $REDIS_VERSION ($ARCH) cached at $REDIS_OUT"
  else
    echo "Building static redis-server $REDIS_VERSION for $ARCH..."
    mkdir -p "$REDIS_OUT"
    docker run --rm --platform "$DOCKER_PLATFORM" \
      -e REDIS_VERSION="$REDIS_VERSION" \
      -e REDIS_SHA256="$REDIS_SHA256" \
      -v "$REDIS_OUT:/out" \
      alpine:3.19 sh -c '
        set -eu
        apk add --no-cache build-base linux-headers curl
        archive=/tmp/redis.tar.gz
        curl --proto "=https" --tlsv1.2 -fsSL \
          "https://download.redis.io/releases/redis-${REDIS_VERSION}.tar.gz" \
          -o "$archive"
        printf "%s  %s\n" "$REDIS_SHA256" "$archive" | sha256sum -c -
        tar -xzf "$archive" -C /tmp
        cd "/tmp/redis-${REDIS_VERSION}"
        make -j$(nproc) BUILD_TLS=no LDFLAGS="-static"
        cp src/redis-server /out/redis-server
        cp src/redis-cli /out/redis-cli
        strip /out/redis-server /out/redis-cli
      '
    validate_static_elf "$REDIS_OUT/redis-server" "$ARCH" redis-server
    validate_static_elf "$REDIS_OUT/redis-cli" "$ARCH" redis-cli
    printf '%s\n' "$REDIS_SHA256" > "$REDIS_OUT/.source-sha256"
    chmod 0444 "$REDIS_OUT/.source-sha256"
    echo "redis-server built: $REDIS_OUT/redis-server"
  fi
else
  echo "Skipping optional redis-server (set INCLUDE_REDIS=1 to include)"
fi

if [[ "$INCLUDE_NGINX" == "1" ]]; then
  require_trusted_sha256 "$NGINX_SHA256" NGINX
  if [[ -e "$NGINX_OUT" || -L "$NGINX_OUT" ]]; then
    [[ -d "$NGINX_OUT" && ! -L "$NGINX_OUT" ]] || {
      echo "nginx cache path is not a regular directory: $NGINX_OUT" >&2
      exit 1
    }
    validate_cache_provenance \
      "$NGINX_OUT/.source-sha256" "$NGINX_SHA256" nginx
    validate_static_elf "$NGINX_OUT/nginx" "$ARCH" nginx
    echo "nginx $NGINX_VERSION ($ARCH) cached at $NGINX_OUT"
  else
    echo "Building static nginx $NGINX_VERSION for $ARCH..."
    mkdir -p "$NGINX_OUT"
    docker run --rm --platform "$DOCKER_PLATFORM" \
      -e NGINX_VERSION="$NGINX_VERSION" \
      -e NGINX_SHA256="$NGINX_SHA256" \
      -v "$NGINX_OUT:/out" \
      alpine:3.19 sh -c '
        set -eu
        apk add --no-cache build-base pcre2-dev zlib-dev zlib-static curl
        archive=/tmp/nginx.tar.gz
        curl --proto "=https" --tlsv1.2 -fsSL \
          "https://nginx.org/download/nginx-${NGINX_VERSION}.tar.gz" \
          -o "$archive"
        printf "%s  %s\n" "$NGINX_SHA256" "$archive" | sha256sum -c -
        tar -xzf "$archive" -C /tmp
        cd "/tmp/nginx-${NGINX_VERSION}"
        ./configure \
          --with-http_auth_request_module \
          --with-http_v2_module \
          --without-http_uwsgi_module \
          --without-http_scgi_module \
          --without-http_fastcgi_module \
          --without-mail_pop3_module \
          --without-mail_imap_module \
          --without-mail_smtp_module \
          --with-cc-opt="-static" \
          --with-ld-opt="-static"
        make -j$(nproc)
        cp objs/nginx /out/nginx
        strip /out/nginx
      '
    validate_static_elf "$NGINX_OUT/nginx" "$ARCH" nginx
    printf '%s\n' "$NGINX_SHA256" > "$NGINX_OUT/.source-sha256"
    chmod 0444 "$NGINX_OUT/.source-sha256"
    echo "nginx built: $NGINX_OUT/nginx"
  fi
else
  echo "Skipping optional nginx (set INCLUDE_NGINX=1 to include the browser client)"
fi

echo 'Requested static dependencies are ready under build/cache/static-deps/'
