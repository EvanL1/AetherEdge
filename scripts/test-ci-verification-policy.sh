#!/usr/bin/env bash
# shellcheck disable=SC2016 # GitHub expressions and commands are asserted literally.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT_DIR
readonly AGENT_INSTRUCTIONS="$ROOT_DIR/AGENTS.md"
readonly PULL_REQUEST_TEMPLATE="$ROOT_DIR/.github/PULL_REQUEST_TEMPLATE.md"
readonly CODE_CHECK_WORKFLOW="$ROOT_DIR/.github/workflows/rust-check.yml"
readonly TOPOLOGY_SOAK_WORKFLOW="$ROOT_DIR/.github/workflows/topology-soak.yml"
readonly ARCHITECTURE_CHECK="$ROOT_DIR/scripts/check-architecture.sh"
readonly DISTRIBUTION_CHECK="$ROOT_DIR/scripts/check-distribution-contracts.sh"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file=$1
    local expected=$2

    rg --fixed-strings --quiet -- "$expected" "$file" \
        || fail "$file is missing required CI policy: $expected"
}

assert_not_contains() {
    local file=$1
    local forbidden=$2

    if rg --fixed-strings --quiet -- "$forbidden" "$file"; then
        fail "$file retains obsolete CI policy: $forbidden"
    fi
}

assert_job_needs_quality_check() {
    local job=$1

    awk -v job="$job" '
        $0 == "  " job ":" { in_job = 1; next }
        in_job && /^  [A-Za-z0-9_-]+:$/ { exit }
        in_job && $0 == "    needs: quality-check" { found = 1 }
        END { exit !(in_job && found) }
    ' "$CODE_CHECK_WORKFLOW" \
        || fail "$job must run directly after quality-check"
}

echo "Checking local verification is risk-proportional..."
assert_contains "$AGENT_INSTRUCTIONS" \
    'Full-workspace verification is owned by pull-request CI.'
assert_contains "$AGENT_INSTRUCTIONS" \
    'workspace suite locally by default.'
assert_contains "$AGENT_INSTRUCTIONS" \
    'CI runs; retrieve detailed logs only for failures'

echo "Checking the pull-request template asks for focused local evidence..."
assert_contains "$PULL_REQUEST_TEMPLATE" 'Focused affected check(s)'
assert_contains "$PULL_REQUEST_TEMPLATE" 'Full workspace verification is provided by PR CI.'
assert_not_contains "$PULL_REQUEST_TEMPLATE" '- [ ] Full workspace Clippy check'
assert_not_contains "$PULL_REQUEST_TEMPLATE" '- [ ] `cargo test --workspace --lib --bins`'

echo "Checking Code Check is authoritative and avoids duplicate branch runs..."
[[ "$(rg --fixed-strings --count-matches 'branches: [main, develop]' "$CODE_CHECK_WORKFLOW")" == 2 ]] \
    || fail "Code Check must run on main/develop pushes and PRs targeting main/develop"
assert_not_contains "$CODE_CHECK_WORKFLOW" 'feature/*'
assert_contains "$CODE_CHECK_WORKFLOW" 'cancel-in-progress: ${{ github.event_name == '\''pull_request'\'' }}'
assert_contains "$CODE_CHECK_WORKFLOW" './scripts/test-ci-verification-policy.sh'
assert_contains "$CODE_CHECK_WORKFLOW" 'cargo fmt --all -- --check'
assert_contains "$CODE_CHECK_WORKFLOW" './scripts/check-architecture.sh'
assert_contains "$CODE_CHECK_WORKFLOW" './scripts/check-distribution-contracts.sh'
assert_contains "$CODE_CHECK_WORKFLOW" \
    'cargo clippy --workspace --all-targets --all-features -- -D warnings'
assert_contains "$CODE_CHECK_WORKFLOW" 'cargo nextest run --workspace --lib --bins'
assert_contains "$CODE_CHECK_WORKFLOW" \
    'cargo check --manifest-path firmware/Cargo.toml --target thumbv7em-none-eabihf'
assert_not_contains "$ARCHITECTURE_CHECK" 'ruby -r'
assert_not_contains "$ARCHITECTURE_CHECK" 'python3 '
assert_not_contains "$ARCHITECTURE_CHECK" 'docker compose'
assert_not_contains "$ARCHITECTURE_CHECK" 'test-installer-layout.sh'
assert_contains "$ARCHITECTURE_CHECK" \
    'cargo test -p aether-architecture-tests --test workspace_boundaries'
assert_contains "$ARCHITECTURE_CHECK" \
    'channel_management_capabilities_remain_high_risk_and_audited'
assert_contains "$DISTRIBUTION_CHECK" './scripts/check-shm-only-runtime.sh'
assert_contains "$DISTRIBUTION_CHECK" './scripts/test-installer-layout.sh'
for job in unit-tests coverage-report config-validation; do
    assert_job_needs_quality_check "$job"
done

echo "Checking topology soak is path-scoped but remains scheduled and dispatchable..."
assert_contains "$TOPOLOGY_SOAK_WORKFLOW" 'workflow_dispatch:'
assert_contains "$TOPOLOGY_SOAK_WORKFLOW" 'schedule:'
assert_contains "$TOPOLOGY_SOAK_WORKFLOW" 'cancel-in-progress: ${{ github.event_name == '\''pull_request'\'' }}'
for required_path in \
    'crates/aether-dataplane/**' \
    'crates/aether-ports/**' \
    'extensions/shm-bridge/**' \
    'services/history/**' \
    'services/io/**' \
    'services/uplink/**'; do
    assert_contains "$TOPOLOGY_SOAK_WORKFLOW" "$required_path"
done

echo "CI verification policy tests passed."
