#!/bin/bash
# AetherEMS 覆盖率分析脚本
# 使用 cargo-llvm-cov 生成覆盖率报告

set -e

# Color definitions
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
NC='\033[0m'

# Default values
OUTPUT_FORMAT="html"
OPEN_REPORT=false
INCLUDE_INTEGRATION=false
COVERAGE_DIR="target/coverage"

print_usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --html          Generate HTML report (default)"
    echo "  --lcov          Generate LCOV report for CI"
    echo "  --json          Generate JSON report"
    echo "  --text          Print coverage summary to terminal"
    echo "  --all           Generate all formats"
    echo "  --open          Open HTML report in browser"
    echo "  --integration   Include integration tests (requires Redis)"
    echo "  --help          Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0 --html --open         # Generate and open HTML report"
    echo "  $0 --lcov                # Generate LCOV for CI"
    echo "  $0 --text                # Quick terminal summary"
    echo "  $0 --all --integration   # Full coverage with integration tests"
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --html)
            OUTPUT_FORMAT="html"
            shift
            ;;
        --lcov)
            OUTPUT_FORMAT="lcov"
            shift
            ;;
        --json)
            OUTPUT_FORMAT="json"
            shift
            ;;
        --text)
            OUTPUT_FORMAT="text"
            shift
            ;;
        --all)
            OUTPUT_FORMAT="all"
            shift
            ;;
        --open)
            OPEN_REPORT=true
            shift
            ;;
        --integration)
            INCLUDE_INTEGRATION=true
            shift
            ;;
        --help)
            print_usage
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            print_usage
            exit 1
            ;;
    esac
done

# Check if cargo-llvm-cov is installed
if ! command -v cargo-llvm-cov &> /dev/null; then
    echo -e "${YELLOW}cargo-llvm-cov not found. Installing...${NC}"
    cargo install cargo-llvm-cov
fi

# Create coverage directory
mkdir -p "$COVERAGE_DIR"

echo -e "${GREEN}=== AetherEMS Coverage Analysis ===${NC}"

# Build test arguments
TEST_ARGS="--workspace"

if [ "$INCLUDE_INTEGRATION" = true ]; then
    echo -e "${CYAN}Including integration tests (requires Redis)${NC}"
    # Integration tests need single-threaded execution
    TEST_ARGS="$TEST_ARGS -- --test-threads=1"
else
    echo -e "${YELLOW}Running unit tests only (use --integration for full coverage)${NC}"
    TEST_ARGS="$TEST_ARGS --lib --bins"
fi

# Run coverage based on output format
case $OUTPUT_FORMAT in
    html)
        echo -e "${YELLOW}Generating HTML coverage report...${NC}"
        cargo llvm-cov $TEST_ARGS --html --output-dir "$COVERAGE_DIR/html"
        echo -e "${GREEN}✓ HTML report generated: $COVERAGE_DIR/html/index.html${NC}"

        if [ "$OPEN_REPORT" = true ]; then
            if command -v open &> /dev/null; then
                open "$COVERAGE_DIR/html/index.html"
            elif command -v xdg-open &> /dev/null; then
                xdg-open "$COVERAGE_DIR/html/index.html"
            else
                echo -e "${YELLOW}Cannot auto-open. Please open manually: $COVERAGE_DIR/html/index.html${NC}"
            fi
        fi
        ;;
    lcov)
        echo -e "${YELLOW}Generating LCOV report...${NC}"
        cargo llvm-cov $TEST_ARGS --lcov --output-path "$COVERAGE_DIR/lcov.info"
        echo -e "${GREEN}✓ LCOV report generated: $COVERAGE_DIR/lcov.info${NC}"
        ;;
    json)
        echo -e "${YELLOW}Generating JSON report...${NC}"
        cargo llvm-cov $TEST_ARGS --json --output-path "$COVERAGE_DIR/coverage.json"
        echo -e "${GREEN}✓ JSON report generated: $COVERAGE_DIR/coverage.json${NC}"
        ;;
    text)
        echo -e "${YELLOW}Coverage Summary:${NC}"
        cargo llvm-cov $TEST_ARGS
        ;;
    all)
        echo -e "${YELLOW}Generating all coverage reports...${NC}"

        # HTML
        cargo llvm-cov $TEST_ARGS --html --output-dir "$COVERAGE_DIR/html"
        echo -e "${GREEN}✓ HTML report: $COVERAGE_DIR/html/index.html${NC}"

        # LCOV
        cargo llvm-cov $TEST_ARGS --lcov --output-path "$COVERAGE_DIR/lcov.info"
        echo -e "${GREEN}✓ LCOV report: $COVERAGE_DIR/lcov.info${NC}"

        # JSON
        cargo llvm-cov $TEST_ARGS --json --output-path "$COVERAGE_DIR/coverage.json"
        echo -e "${GREEN}✓ JSON report: $COVERAGE_DIR/coverage.json${NC}"

        # Text summary
        echo -e "${CYAN}Coverage Summary:${NC}"
        cargo llvm-cov $TEST_ARGS

        if [ "$OPEN_REPORT" = true ]; then
            if command -v open &> /dev/null; then
                open "$COVERAGE_DIR/html/index.html"
            elif command -v xdg-open &> /dev/null; then
                xdg-open "$COVERAGE_DIR/html/index.html"
            fi
        fi
        ;;
esac

echo -e "${GREEN}=== Coverage Analysis Complete ===${NC}"
