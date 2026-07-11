#!/bin/bash
# AetherEMS Quick Check Script

set -e

# Color definitions
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
NC='\033[0m'

echo -e "${GREEN}=== AetherEMS Quick Check ===${NC}"

# Sync git submodules (e.g. product-lib)
echo -e "${YELLOW}Syncing git submodules...${NC}"
git submodule update --init --recursive

# Check if submodules are behind their remote tracking branch
echo -e "${YELLOW}Checking submodule freshness...${NC}"
git submodule foreach --quiet '
  git fetch --quiet origin 2>/dev/null
  LOCAL=$(git rev-parse HEAD)
  REMOTE=$(git rev-parse origin/HEAD 2>/dev/null || git rev-parse origin/main 2>/dev/null || echo "")
  if [ -n "$REMOTE" ] && [ "$LOCAL" != "$REMOTE" ]; then
    BEHIND=$(git rev-list --count HEAD..origin/HEAD 2>/dev/null || git rev-list --count HEAD..origin/main 2>/dev/null || echo "?")
    echo "⚠️  Submodule $name is ${BEHIND} commit(s) behind remote"
    echo "   Run: git submodule update --remote && git add $sm_path && git commit"
  fi
'

# Check for forbidden mod.rs files (project convention)
echo -e "${YELLOW}Checking for mod.rs files...${NC}"
MOD_RS_FILES=$(find . -name "mod.rs" -not -path "./target/*" 2>/dev/null || true)
if [ -n "$MOD_RS_FILES" ]; then
    echo -e "${RED}ERROR: mod.rs files are forbidden (project convention)${NC}"
    echo "$MOD_RS_FILES"
    exit 1
fi
echo -e "${GREEN}No mod.rs files found${NC}"

# Enforce the AI-native core/extension dependency boundary.
echo -e "${YELLOW}Checking architecture boundaries...${NC}"
./scripts/check-architecture.sh

# Check compilation
echo -e "${YELLOW}Checking compilation...${NC}"
cargo check --workspace

# Format check
echo -e "${YELLOW}Checking code format...${NC}"
cargo fmt --all -- --check

# Clippy check (all features enabled)
echo -e "${YELLOW}Running Clippy...${NC}"
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Runtime panic-boundary check (tests may still use unwrap/expect for clarity).
echo -e "${YELLOW}Checking runtime unwrap/expect usage...${NC}"
cargo clippy --workspace --lib --bins -- -D clippy::unwrap_used -D clippy::expect_used

# Prefer cargo-nextest when available (2-3x faster, per-test process isolation).
# Falls back to cargo test so developers who haven't installed nextest are
# not blocked. Install with: curl -LsSf https://get.nexte.st/latest/mac | tar zxf - -C ~/.cargo/bin
if command -v cargo-nextest &> /dev/null; then
    TEST_RUNNER=(cargo nextest run)
    INT_TEST_SELECTOR=(-E 'kind(test)')
    echo -e "${GREEN}Using cargo-nextest (faster)${NC}"
else
    TEST_RUNNER=(cargo test)
    INT_TEST_SELECTOR=(--test '*')
    echo -e "${YELLOW}Hint: install cargo-nextest for faster tests${NC}"
fi

# Run unit tests (no external dependencies required)
echo -e "${YELLOW}Running unit tests...${NC}"
"${TEST_RUNNER[@]}"
"${TEST_RUNNER[@]}" \
    -p aether-shm-bridge \
    -p aether-redis-bridge \
    -p aether-postgres-history \
    -p aether-example-minimal-gateway \
    -p aether-example-energy-gateway
"${TEST_RUNNER[@]}" --workspace --lib --bins

# Check command line arguments
RUN_INTEGRATION=false
RUN_COVERAGE=false

for arg in "$@"; do
    case $arg in
        --with-integration)
            RUN_INTEGRATION=true
            ;;
        --with-coverage)
            RUN_COVERAGE=true
            ;;
    esac
done

# Run integration tests (optional - requires Redis)
if [ "$RUN_INTEGRATION" = true ]; then
    echo -e "${YELLOW}Running integration tests...${NC}"
    "${TEST_RUNNER[@]}" --workspace "${INT_TEST_SELECTOR[@]}"
else
    echo -e "${YELLOW}Skipping integration tests (use --with-integration to run)${NC}"
    echo -e "${YELLOW}Integration tests require Redis${NC}"
fi

# Run coverage analysis (optional)
if [ "$RUN_COVERAGE" = true ]; then
    echo -e "${YELLOW}Running coverage analysis...${NC}"
    if command -v cargo-llvm-cov &> /dev/null; then
        cargo llvm-cov --workspace --lib --bins
    else
        echo -e "${RED}cargo-llvm-cov not installed. Run: cargo install cargo-llvm-cov${NC}"
    fi
fi

echo -e "${GREEN}All checks passed!${NC}"
