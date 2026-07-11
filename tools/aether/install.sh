#!/usr/bin/env bash
# Aether CLI installer — auto-detects platform, downloads binary, adds to PATH
# Usage: curl -fsSL https://raw.githubusercontent.com/EvanL1/Aether/main/tools/aether/install.sh | bash
set -euo pipefail

REPO="EvanL1/Aether"
INSTALL_DIR="${AETHER_INSTALL_DIR:-$HOME/.local/bin}"
INSTALL_TMPDIR=""

cleanup() {
    if [ -n "$INSTALL_TMPDIR" ]; then
        rm -rf "$INSTALL_TMPDIR"
    fi
}

trap cleanup EXIT

# ─── Detect platform ────────────────────────────────────────────────────────

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="darwin" ;;
        MINGW*|MSYS*|CYGWIN*) os="windows" ;;
        *) echo "Error: unsupported OS: $os" >&2; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) echo "Error: unsupported architecture: $arch" >&2; exit 1 ;;
    esac

    case "${os}-${arch}" in
        linux-x86_64|linux-aarch64|darwin-aarch64|windows-x86_64)
            echo "${os}-${arch}"
            ;;
        *)
            echo "Error: no published Aether CLI artifact for ${os}-${arch}" >&2
            echo "Supported release artifacts: linux-x86_64, linux-aarch64, darwin-aarch64, windows-x86_64" >&2
            return 1
            ;;
    esac
}

# ─── Find latest release ────────────────────────────────────────────────────

latest_tag() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep -o '"tag_name": *"v[0-9][^"]*"' \
        | head -1 \
        | sed 's/.*"tag_name": *"//;s/"//'
}

# ─── Verify release integrity ───────────────────────────────────────────────

require_checksum_tool() {
    local platform=$1

    case "$platform" in
        linux-*|windows-*)
            if ! command -v sha256sum >/dev/null 2>&1; then
                echo "Error: sha256sum is required to verify the downloaded release" >&2
                return 1
            fi
            ;;
        darwin-*)
            if ! command -v shasum >/dev/null 2>&1; then
                echo "Error: shasum is required to verify the downloaded release" >&2
                return 1
            fi
            ;;
        *)
            echo "Error: unsupported platform for checksum verification: $platform" >&2
            return 1
            ;;
    esac
}

verify_checksum() {
    local platform=$1
    local archive_path=$2
    local checksum_path=$3
    local expected actual

    if ! expected="$(awk '
        NF {
            count++
            if (count == 1) {
                checksum = $1
            }
        }
        END {
            if (count != 1) {
                exit 1
            }
            print checksum
        }
    ' "$checksum_path")"; then
        echo "Error: malformed checksum file: $checksum_path" >&2
        return 1
    fi

    if ! printf '%s\n' "$expected" | grep -Eq '^[[:xdigit:]]{64}$'; then
        echo "Error: malformed SHA-256 digest in $checksum_path" >&2
        return 1
    fi

    case "$platform" in
        linux-*|windows-*)
            if ! actual="$(sha256sum "$archive_path" | awk '{print $1}')"; then
                echo "Error: failed to calculate SHA-256 for $archive_path" >&2
                return 1
            fi
            ;;
        darwin-*)
            if ! actual="$(shasum -a 256 "$archive_path" | awk '{print $1}')"; then
                echo "Error: failed to calculate SHA-256 for $archive_path" >&2
                return 1
            fi
            ;;
        *)
            echo "Error: unsupported platform for checksum verification: $platform" >&2
            return 1
            ;;
    esac

    expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
    actual="$(printf '%s' "$actual" | tr '[:upper:]' '[:lower:]')"
    if [ "$actual" != "$expected" ]; then
        echo "Error: checksum verification failed for $(basename "$archive_path")" >&2
        echo "  Expected: $expected" >&2
        echo "  Actual:   $actual" >&2
        return 1
    fi

    echo "  SHA-256 verified"
}

# ─── Main ────────────────────────────────────────────────────────────────────

main() {
    local platform tag archive archive_name url checksum_url archive_path checksum_path

    echo "Detecting platform..."
    platform="$(detect_platform)"
    echo "  Platform: ${platform}"
    require_checksum_tool "$platform"

    echo "Finding latest release..."
    tag="$(latest_tag)"
    if [ -z "$tag" ]; then
        echo "Error: no aether release found" >&2
        exit 1
    fi
    echo "  Version: ${tag}"

    archive="aether-${platform}"
    if [ "${platform%%-*}" = "windows" ]; then
        archive_name="${archive}.zip"
    else
        archive_name="${archive}.tar.gz"
    fi
    url="https://github.com/${REPO}/releases/download/${tag}/${archive_name}"
    checksum_url="${url}.sha256"

    echo "Downloading ${url}..."
    mkdir -p "$INSTALL_DIR"

    INSTALL_TMPDIR="$(mktemp -d)"
    archive_path="$INSTALL_TMPDIR/$archive_name"
    checksum_path="${archive_path}.sha256"

    curl -fsSL "$url" -o "$archive_path"
    curl -fsSL "$checksum_url" -o "$checksum_path"
    verify_checksum "$platform" "$archive_path" "$checksum_path"

    if [ "${platform%%-*}" = "windows" ]; then
        unzip -qo "$archive_path" -d "$INSTALL_TMPDIR"
        cp "$INSTALL_TMPDIR/aether.exe" "$INSTALL_DIR/"
    else
        tar xzf "$archive_path" -C "$INSTALL_TMPDIR"
        chmod +x "$INSTALL_TMPDIR/aether"
        cp "$INSTALL_TMPDIR/aether" "$INSTALL_DIR/"
    fi

    echo ""
    echo "Installed: ${INSTALL_DIR}/aether"

    # Check if INSTALL_DIR is in PATH
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        echo ""
        echo "Add to PATH (add to your shell profile):"
        echo ""
        echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
        echo ""

        # Detect shell and suggest the right file
        case "${SHELL:-}" in
            */zsh)  echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc" ;;
            */bash) echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc" ;;
            *)      echo "  # Add the export line to your shell config" ;;
        esac
    fi

    echo ""
    echo "Verify: aether --version"
    echo "Plan a safe local first run: aether setup"
    echo "Runtime note: this CLI installer does not install service binaries, images, or Compose."
    echo "Install the AetherEdge .run package before using 'aether services' on a new host."
}

main "$@"
