#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECKER="$ROOT_DIR/scripts/check-extraction-readiness.sh"
WORKSPACE_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)
GOOD_SHA_A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
GOOD_SHA_B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
GOOD_COMMIT=cccccccccccccccccccccccccccccccccccccccc

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

run_checker() {
    local output=$1
    shift
    local status=0
    "$CHECKER" "$@" >"$output.stdout" 2>"$output.stderr" || status=$?
    CHECK_STATUS=$status
}

[[ -x "$CHECKER" ]] || fail "extraction-readiness checker is missing or not executable"

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

echo "Testing absent external evidence fails closed..."
run_checker "$TEMP_DIR/missing" --evidence-only
[[ $CHECK_STATUS -ne 0 ]] || fail "missing external evidence was accepted"
for input in \
    released-version \
    kernel-artifact-sha256 \
    energy-pack-artifact-sha256 \
    downstream-repository \
    downstream-ci-run-url \
    downstream-ci-commit \
    downstream-ci-conclusion; do
    grep -Fq -- "--$input" "$TEMP_DIR/missing.stderr" \
        || fail "missing-evidence error did not name --$input"
done

run_checker "$TEMP_DIR/default-missing"
[[ $CHECK_STATUS -ne 0 ]] || fail "default full gate accepted absent external evidence"
if grep -Fq 'Checking locally provable' "$TEMP_DIR/default-missing.stdout"; then
    fail "default full gate did expensive local work before rejecting absent evidence"
fi

echo "Testing malformed or unsuccessful evidence fails closed..."
run_checker "$TEMP_DIR/bad-digest" --evidence-only \
    --released-version "$WORKSPACE_VERSION" \
    --kernel-artifact-sha256 not-a-digest \
    --energy-pack-artifact-sha256 "$GOOD_SHA_B" \
    --downstream-repository example/aether-ems \
    --downstream-ci-run-url https://github.com/example/aether-ems/actions/runs/123 \
    --downstream-ci-commit "$GOOD_COMMIT" \
    --downstream-ci-conclusion success
[[ $CHECK_STATUS -ne 0 ]] || fail "malformed Kernel digest was accepted"

run_checker "$TEMP_DIR/failed-ci" --evidence-only \
    --released-version "$WORKSPACE_VERSION" \
    --kernel-artifact-sha256 "$GOOD_SHA_A" \
    --energy-pack-artifact-sha256 "$GOOD_SHA_B" \
    --downstream-repository example/aether-ems \
    --downstream-ci-run-url https://github.com/example/aether-ems/actions/runs/123 \
    --downstream-ci-commit "$GOOD_COMMIT" \
    --downstream-ci-conclusion failure
[[ $CHECK_STATUS -ne 0 ]] || fail "failed downstream CI was accepted"

echo "Testing complete, internally consistent evidence is accepted..."
run_checker "$TEMP_DIR/good" --evidence-only \
    --released-version "$WORKSPACE_VERSION" \
    --kernel-artifact-sha256 "$GOOD_SHA_A" \
    --energy-pack-artifact-sha256 "$GOOD_SHA_B" \
    --downstream-repository example/aether-ems \
    --downstream-ci-run-url https://github.com/example/aether-ems/actions/runs/123 \
    --downstream-ci-commit "$GOOD_COMMIT" \
    --downstream-ci-conclusion success
[[ $CHECK_STATUS -eq 0 ]] || {
    cat "$TEMP_DIR/good.stderr" >&2
    fail "complete evidence was rejected"
}
grep -Fq 'external extraction evidence is structurally complete' "$TEMP_DIR/good.stdout" \
    || fail "successful evidence validation was not reported"

echo "Extraction readiness evidence tests passed."
