#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKER="$SCRIPT_DIR/check-public-crate-release.sh"
PUBLISHER="$SCRIPT_DIR/publish-public-crates.sh"
SEMVER_CHECKER="$SCRIPT_DIR/check-public-api-compatibility.sh"
ARCHIVE_VERIFIER="$SCRIPT_DIR/verify-published-crate-archive.sh"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

[[ -x "$CHECKER" ]] || fail "public-crate release checker is missing or not executable"
[[ -x "$PUBLISHER" ]] || fail "public-crate publisher is missing or not executable"
[[ -x "$SEMVER_CHECKER" ]] || fail "public API compatibility checker is missing or not executable"
[[ -x "$ARCHIVE_VERIFIER" ]] || fail "published crate archive verifier is missing or not executable"

TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT

WORKSPACE_ROOT="$TEST_ROOT/workspace"
mkdir -p "$WORKSPACE_ROOT/crates/domain" "$WORKSPACE_ROOT/crates/ports" \
    "$WORKSPACE_ROOT/crates/sdk" "$WORKSPACE_ROOT/libs/private-helper"

for manifest in \
    crates/domain/Cargo.toml \
    crates/ports/Cargo.toml \
    crates/sdk/Cargo.toml \
    libs/private-helper/Cargo.toml; do
    printf '[package]\nname = "fixture"\nversion = "0.5.0"\n' \
        > "$WORKSPACE_ROOT/$manifest"
done

METADATA="$TEST_ROOT/metadata.json"
cat > "$METADATA" <<EOF
{
  "workspace_root": "$WORKSPACE_ROOT",
  "packages": [
    {
      "name": "aether-domain",
      "version": "0.5.0",
      "manifest_path": "$WORKSPACE_ROOT/crates/domain/Cargo.toml",
      "publish": null,
      "dependencies": []
    },
    {
      "name": "aether-ports",
      "version": "0.5.0",
      "manifest_path": "$WORKSPACE_ROOT/crates/ports/Cargo.toml",
      "publish": null,
      "dependencies": [
        {"name": "aether-domain", "path": "$WORKSPACE_ROOT/crates/domain", "kind": null}
      ]
    },
    {
      "name": "aether-edge-sdk",
      "version": "0.5.0",
      "manifest_path": "$WORKSPACE_ROOT/crates/sdk/Cargo.toml",
      "publish": null,
      "dependencies": [
        {"name": "aether-ports", "path": "$WORKSPACE_ROOT/crates/ports", "kind": null}
      ]
    },
    {
      "name": "private-helper",
      "version": "0.5.0",
      "manifest_path": "$WORKSPACE_ROOT/libs/private-helper/Cargo.toml",
      "publish": [],
      "dependencies": []
    }
  ]
}
EOF

run_checker() {
    local catalog=$1
    local stdout=$2
    local stderr=$3
    local status=0

    "$CHECKER" \
        --catalog-only \
        --catalog "$catalog" \
        --metadata-json "$METADATA" \
        > "$stdout" 2> "$stderr" || status=$?
    return "$status"
}

VALID_CATALOG="$TEST_ROOT/valid.txt"
cat > "$VALID_CATALOG" <<'EOF'
# package<TAB>workspace-relative path
aether-domain	crates/domain
aether-ports	crates/ports
aether-edge-sdk	crates/sdk
EOF

echo "Testing a complete dependency-ordered public crate catalog..."
run_checker "$VALID_CATALOG" "$TEST_ROOT/valid.out" "$TEST_ROOT/valid.err" \
    || { cat "$TEST_ROOT/valid.err" >&2; fail "valid public crate catalog was rejected"; }

REVERSED_CATALOG="$TEST_ROOT/reversed.txt"
cat > "$REVERSED_CATALOG" <<'EOF'
aether-ports	crates/ports
aether-domain	crates/domain
aether-edge-sdk	crates/sdk
EOF

echo "Testing a dependent crate cannot precede its public dependency..."
if run_checker "$REVERSED_CATALOG" "$TEST_ROOT/reversed.out" "$TEST_ROOT/reversed.err"; then
    fail "dependency-inverted public crate catalog was accepted"
fi
grep -Fq 'must appear after dependency aether-domain' "$TEST_ROOT/reversed.err" \
    || fail "dependency-order rejection was not explicit"

OMITTED_CATALOG="$TEST_ROOT/omitted.txt"
cat > "$OMITTED_CATALOG" <<'EOF'
aether-domain	crates/domain
aether-ports	crates/ports
EOF

echo "Testing every publishable workspace crate must be catalogued..."
if run_checker "$OMITTED_CATALOG" "$TEST_ROOT/omitted.out" "$TEST_ROOT/omitted.err"; then
    fail "catalog omitting a public workspace crate was accepted"
fi
grep -Fq 'publishable workspace package is missing from the catalog: aether-edge-sdk' \
    "$TEST_ROOT/omitted.err" \
    || fail "missing-public-package rejection was not explicit"

DUPLICATE_CATALOG="$TEST_ROOT/duplicate.txt"
cat > "$DUPLICATE_CATALOG" <<'EOF'
aether-domain	crates/domain
aether-ports	crates/ports
aether-ports	crates/ports
aether-edge-sdk	crates/sdk
EOF

echo "Testing duplicate catalog entries fail closed..."
if run_checker "$DUPLICATE_CATALOG" "$TEST_ROOT/duplicate.out" "$TEST_ROOT/duplicate.err"; then
    fail "duplicate public crate catalog entry was accepted"
fi
grep -Fq 'duplicate public crate catalog package: aether-ports' "$TEST_ROOT/duplicate.err" \
    || fail "duplicate-package rejection was not explicit"

PRIVATE_DEP_METADATA="$TEST_ROOT/private-dependency-metadata.json"
jq '(.packages[] | select(.name == "aether-edge-sdk") | .dependencies) += [{
    "name": "private-helper",
    "path": $private_path,
    "kind": null
}]' --arg private_path "$WORKSPACE_ROOT/libs/private-helper" "$METADATA" \
    > "$PRIVATE_DEP_METADATA"

echo "Testing a public crate cannot depend on a private workspace package..."
status=0
"$CHECKER" \
    --catalog-only \
    --catalog "$VALID_CATALOG" \
    --metadata-json "$PRIVATE_DEP_METADATA" \
    > "$TEST_ROOT/private.out" 2> "$TEST_ROOT/private.err" || status=$?
[[ $status -ne 0 ]] || fail "public-to-private dependency was accepted"
grep -Fq 'depends on private workspace package private-helper' "$TEST_ROOT/private.err" \
    || fail "public-to-private dependency rejection was not explicit"

echo "Testing publication defaults to a non-mutating ordered plan..."
"$PUBLISHER" > "$TEST_ROOT/publish-plan.out" 2> "$TEST_ROOT/publish-plan.err" \
    || { cat "$TEST_ROOT/publish-plan.err" >&2; fail "publication plan failed"; }
grep -Fq 'publication plan only; pass --execute in the protected tag workflow' \
    "$TEST_ROOT/publish-plan.out" \
    || fail "publisher did not explain its non-mutating default"
grep -Fq 'aether-edge-sdk' "$TEST_ROOT/publish-plan.out" \
    || fail "publication plan omitted the SDK"

echo "Testing publication execution is rejected outside protected tag CI..."
status=0
env -u GITHUB_ACTIONS -u GITHUB_REF_TYPE -u GITHUB_REF_NAME -u CARGO_REGISTRY_TOKEN \
    "$PUBLISHER" --execute \
    > "$TEST_ROOT/publish-execute.out" 2> "$TEST_ROOT/publish-execute.err" || status=$?
[[ $status -ne 0 ]] || fail "publisher executed outside protected tag CI"
grep -Fq 'publishing is allowed only in GitHub Actions' "$TEST_ROOT/publish-execute.err" \
    || fail "unsafe publication rejection was not explicit"

FAKE_BIN="$TEST_ROOT/fake-bin"
SEMVER_LOG="$TEST_ROOT/semver.log"
mkdir -p "$FAKE_BIN"
cat > "$FAKE_BIN/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
    metadata)
        cat "$AETHER_TEST_METADATA"
        ;;
    info)
        [[ "${*: -1}" == "aether-ports" ]]
        ;;
    semver-checks)
        printf '%s\n' "$*" >> "$AETHER_TEST_SEMVER_LOG"
        ;;
    *)
        echo "unexpected cargo invocation: $*" >&2
        exit 70
        ;;
esac
EOF
chmod +x "$FAKE_BIN/cargo"

echo "Testing API checks skip first releases and inspect published crates..."
PATH="$FAKE_BIN:$PATH" \
    AETHER_TEST_METADATA="$METADATA" \
    AETHER_TEST_SEMVER_LOG="$SEMVER_LOG" \
    "$SEMVER_CHECKER" --catalog "$VALID_CATALOG" \
    > "$TEST_ROOT/semver.out" 2> "$TEST_ROOT/semver.err" \
    || { cat "$TEST_ROOT/semver.err" >&2; fail "public API compatibility check failed"; }
grep -Fq 'aether-domain@0.5.0 has no crates.io baseline; treating it as a first release' \
    "$TEST_ROOT/semver.out" \
    || fail "first-release crate was not reported explicitly"
grep -Fxq 'semver-checks --package aether-ports' "$SEMVER_LOG" \
    || fail "published crate was not checked against its registry baseline"
if grep -Fq 'aether-domain' "$SEMVER_LOG" || grep -Fq 'aether-edge-sdk' "$SEMVER_LOG"; then
    fail "first-release crate was passed to cargo-semver-checks without a baseline"
fi

cat > "$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

output=''
while (($# > 0)); do
    case "$1" in
        -o)
            output=$2
            shift 2
            ;;
        *) shift ;;
    esac
done
[[ -n "$output" ]]
cp "$AETHER_TEST_REMOTE_ARCHIVE" "$output"
EOF
chmod +x "$FAKE_BIN/curl"

printf 'identical crate bytes\n' > "$TEST_ROOT/local.crate"
cp "$TEST_ROOT/local.crate" "$TEST_ROOT/remote.crate"

echo "Testing an identical published archive permits a safe release resume..."
PATH="$FAKE_BIN:$PATH" AETHER_TEST_REMOTE_ARCHIVE="$TEST_ROOT/remote.crate" \
    "$ARCHIVE_VERIFIER" aether-domain 0.5.0 "$TEST_ROOT/local.crate" \
    > "$TEST_ROOT/archive-match.out" 2> "$TEST_ROOT/archive-match.err" \
    || { cat "$TEST_ROOT/archive-match.err" >&2; fail "identical archive was rejected"; }

printf 'different crate bytes\n' > "$TEST_ROOT/remote.crate"
echo "Testing a different published archive fails closed..."
status=0
PATH="$FAKE_BIN:$PATH" AETHER_TEST_REMOTE_ARCHIVE="$TEST_ROOT/remote.crate" \
    "$ARCHIVE_VERIFIER" aether-domain 0.5.0 "$TEST_ROOT/local.crate" \
    > "$TEST_ROOT/archive-mismatch.out" 2> "$TEST_ROOT/archive-mismatch.err" || status=$?
[[ $status -ne 0 ]] || fail "different published archive was accepted"
grep -Fq 'published archive digest differs from the local release archive' \
    "$TEST_ROOT/archive-mismatch.err" \
    || fail "archive mismatch rejection was not explicit"

echo "Public crate release contract tests passed."
