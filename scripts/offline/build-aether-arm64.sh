#!/usr/bin/env bash
# Build all CLI tools for ARM64 Linux
# Creates static binaries using MUSL for maximum compatibility

set -euo pipefail

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Configuration
ROOT_DIR="$(cd "$(dirname "$0")"/../.. && pwd)"
OUTPUT_DIR="$ROOT_DIR/offline-bundle/cli/linux-aarch64/bin"
TARGET="aarch64-unknown-linux-musl"

# Tool packages and their binary names (package:binary format)
TOOLS=(
    "tools/aether:aether"
)

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}  Building CLI Tools for ARM64 Linux   ${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

# Check prerequisites
echo -e "${YELLOW}Checking prerequisites...${NC}"

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "Error: Rust/Cargo not installed"
    exit 1
fi

# Check for target
if ! rustup target list --installed | grep -q "$TARGET"; then
    echo "Installing $TARGET target..."
    rustup target add "$TARGET"
fi

# Check for cargo-zigbuild (optional but recommended)
if command -v cargo-zigbuild &> /dev/null; then
    BUILD_CMD="cargo zigbuild"
    echo -e "${GREEN}[OK] Using cargo-zigbuild for better cross-compilation${NC}"
else
    BUILD_CMD="cargo build"
    echo -e "${YELLOW}Note: Install cargo-zigbuild for better cross-compilation support${NC}"
    echo "  cargo install cargo-zigbuild"
fi

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Build each tool
echo ""
echo -e "${BLUE}Building tools...${NC}"

for tool_entry in "${TOOLS[@]}"; do
    # Split package:binary format
    tool_path="${tool_entry%%:*}"
    binary_name="${tool_entry##*:}"
    package_name=$(basename "$tool_path")

    echo ""
    echo -e "${YELLOW}Building $package_name -> $binary_name${NC}"

    # Build with optimizations (now configured in Cargo.toml)
    cd "$ROOT_DIR"
    $BUILD_CMD --release --target "$TARGET" -p "$package_name"

    # Find the built binary
    built_binary="$ROOT_DIR/target/$TARGET/release/$package_name"

    # Handle special cases where binary name differs from package name
    if [[ "$package_name" == *"-cli" ]]; then
        # Remove -cli suffix for the actual binary
        alt_binary="${built_binary%-cli}"
        [[ -f "$alt_binary" ]] && built_binary="$alt_binary"
    fi

    if [[ ! -f "$built_binary" ]]; then
        echo "Warning: Binary not found at $built_binary"
        # Try to find it - check for the actual binary name from the mapping
        if [[ -f "$ROOT_DIR/target/$TARGET/release/$binary_name" ]]; then
            built_binary="$ROOT_DIR/target/$TARGET/release/$binary_name"
            echo "Found at: $built_binary"
        else
            # Last resort: try to find any matching file (macOS compatible)
            found_binary=$(find "$ROOT_DIR/target/$TARGET/release" -maxdepth 1 -type f -name "${binary_name}" -o -name "${package_name}*" | head -1)
            if [[ -n "$found_binary" ]]; then
                built_binary="$found_binary"
                echo "Found at: $built_binary"
            else
                echo "Error: Could not find binary for $package_name"
                continue
            fi
        fi
    fi

    # Strip symbols if possible (reduce size)
    if command -v llvm-strip &> /dev/null; then
        llvm-strip -s "$built_binary" 2>/dev/null || true
    elif command -v aarch64-linux-gnu-strip &> /dev/null; then
        aarch64-linux-gnu-strip "$built_binary" 2>/dev/null || true
    fi

    # Copy to output with correct name
    cp -f "$built_binary" "$OUTPUT_DIR/$binary_name"
    chmod +x "$OUTPUT_DIR/$binary_name"

    # Show size
    size=$(ls -lh "$OUTPUT_DIR/$binary_name" | awk '{print $5}')
    echo -e "${GREEN}[DONE] Built $binary_name ($size)${NC}"
done

# Summary
echo ""
echo -e "${BLUE}========================================${NC}"
echo -e "${GREEN}  Build Complete!                       ${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""
echo "Built tools in: $OUTPUT_DIR"
echo ""
ls -lh "$OUTPUT_DIR"
echo ""
echo "Total size: $(du -sh "$OUTPUT_DIR" | cut -f1)"
echo ""
echo "These are static MUSL binaries that will run on any ARM64 Linux system."