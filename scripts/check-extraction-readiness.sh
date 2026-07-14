#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MODE=full
RELEASED_VERSION=""
KERNEL_ARTIFACT_SHA256=""
ENERGY_PACK_ARTIFACT_SHA256=""
DOWNSTREAM_REPOSITORY=""
DOWNSTREAM_CI_RUN_URL=""
DOWNSTREAM_CI_COMMIT=""
DOWNSTREAM_CI_CONCLUSION=""

usage() {
    cat <<'EOF'
Usage:
  scripts/check-extraction-readiness.sh --local-only
  scripts/check-extraction-readiness.sh [external evidence options]
  scripts/check-extraction-readiness.sh --evidence-only [external evidence options]

External evidence options (all required outside --local-only):
  --released-version VERSION
  --kernel-artifact-sha256 SHA256
  --energy-pack-artifact-sha256 SHA256
  --downstream-repository OWNER/REPOSITORY
  --downstream-ci-run-url HTTPS_GITHUB_ACTIONS_RUN_URL
  --downstream-ci-commit GIT_COMMIT
  --downstream-ci-conclusion success

--local-only proves only repository-local extraction gates. It does not claim
that a second repository, an external release, an attestation, or downstream
CI exists. The default full check fails closed when any external evidence input
is absent or structurally inconsistent.
EOF
}

set_mode() {
    local requested=$1
    if [[ "$MODE" != full ]]; then
        echo "ERROR: only one of --local-only and --evidence-only may be used" >&2
        exit 2
    fi
    MODE=$requested
}

require_option_value() {
    local option=$1
    local remaining=$2
    if [[ "$remaining" -lt 2 ]]; then
        echo "ERROR: $option requires a value" >&2
        exit 2
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --local-only)
            set_mode local
            shift
            ;;
        --evidence-only)
            set_mode evidence
            shift
            ;;
        --released-version)
            require_option_value "$1" "$#"
            RELEASED_VERSION=$2
            shift 2
            ;;
        --kernel-artifact-sha256)
            require_option_value "$1" "$#"
            KERNEL_ARTIFACT_SHA256=$2
            shift 2
            ;;
        --energy-pack-artifact-sha256)
            require_option_value "$1" "$#"
            ENERGY_PACK_ARTIFACT_SHA256=$2
            shift 2
            ;;
        --downstream-repository)
            require_option_value "$1" "$#"
            DOWNSTREAM_REPOSITORY=$2
            shift 2
            ;;
        --downstream-ci-run-url)
            require_option_value "$1" "$#"
            DOWNSTREAM_CI_RUN_URL=$2
            shift 2
            ;;
        --downstream-ci-commit)
            require_option_value "$1" "$#"
            DOWNSTREAM_CI_COMMIT=$2
            shift 2
            ;;
        --downstream-ci-conclusion)
            require_option_value "$1" "$#"
            DOWNSTREAM_CI_CONCLUSION=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

workspace_version() {
    sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1
}

assert_profiled_service() {
    local service=$1
    local profile=$2
    awk -v service="$service" -v profile="$profile" '
        $0 == "  " service ":" { active = 1; next }
        active && /^  [^[:space:]]/ { exit(found ? 0 : 1) }
        active && index($0, "profiles:") && index($0, "\"" profile "\"") {
            found = 1
        }
        END { exit(found ? 0 : 1) }
    ' docker-compose.yml \
        || fail "docker-compose service $service must remain opt-in behind profile $profile"
}

check_local_gates() {
    local runtime_manifest_source="libs/aether-runtime-catalog/src/bin/aether-runtime-manifest.rs"
    local runtime_features_source="distributions/aetherems/runtime-io-features.txt"
    local temp_dir release_target runtime_features normalized_features artifact_entries

    echo "Checking locally provable ADR-0007 extraction gates..."
    [[ -s "$runtime_manifest_source" ]] \
        || fail "runtime-manifest binary source is missing: $runtime_manifest_source"
    if git check-ignore -q "$runtime_manifest_source"; then
        fail "runtime-manifest binary source is ignored and would be absent from a clean checkout"
    fi
    [[ -s "$runtime_features_source" ]] \
        || fail "AetherEMS runtime feature authority is missing: $runtime_features_source"
    IFS= read -r runtime_features < "$runtime_features_source"
    [[ -n "$runtime_features" ]] \
        || fail "AetherEMS runtime feature authority is empty"
    normalized_features=$(cargo run --quiet -p aether-runtime-catalog \
        --bin aether-runtime-manifest -- normalize-io-features \
        --io-features "$runtime_features")
    [[ "$runtime_features" == "$normalized_features" ]] \
        || fail "AetherEMS runtime features are not canonical: $runtime_features"
    cargo check --quiet -p aether-runtime-catalog --bin aether-runtime-manifest

    ./scripts/check-energy-pack-boundary.sh
    ./scripts/check-safe-default-config.sh
    ./scripts/check-runtime-manifest.sh

    cargo test --quiet -p aether-example-minimal-gateway --test composition_contract
    cargo test --quiet -p aether-example-energy-gateway --test composition_contract
    cargo test --quiet -p aether-example-energy-gateway --test pack_artifact_contract
    cargo run --quiet -p aether-example-minimal-gateway >/dev/null
    cargo run --quiet -p aether-example-energy-gateway >/dev/null

    assert_profiled_service aether-redis redis
    assert_profiled_service timescaledb postgres-storage
    if rg -q '^default[[:space:]]*=.*postgres-storage' services/history/Cargo.toml; then
        fail "PostgreSQL history storage must not be a default service feature"
    fi
    if sed -n '/^default-members[[:space:]]*=/,/^]/p' Cargo.toml \
        | rg -q 'redis-bridge|postgres-history'; then
        fail "an external database adapter is a default workspace member"
    fi

    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' RETURN
    # Energy Pack artifacts are Linux-target-bound because their declared
    # protocol set includes GPIO and CAN. Both released Linux architectures
    # derive the same protocol catalog from this composition.
    release_target=aarch64-unknown-linux-musl
    mkdir -p "$temp_dir/runtime"
    cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
        generate "$release_target" "$temp_dir/runtime" "$runtime_features" >/dev/null
    cargo run --quiet -p aether-runtime-catalog --bin aether-runtime-manifest -- \
        verify --path "$temp_dir/runtime/runtime-manifest.json" \
        --aether-version "$(workspace_version)" >/dev/null
    ./scripts/build-pack-artifact.sh \
        packs/energy \
        "$temp_dir/runtime/runtime-manifest.json" \
        "$temp_dir/energy.bundle" >/dev/null

    artifact_entries=$(
        find "$temp_dir/energy.bundle" -mindepth 1 -maxdepth 1 -exec basename {} \; \
            | LC_ALL=C sort
    )
    [[ "$artifact_entries" == $'pack\npack-artifact.json' ]] \
        || fail "Energy Pack artifact top level is not Pack-only: $artifact_entries"
    if find "$temp_dir/energy.bundle" -type f \
        \( -name Cargo.toml -o -name '*.rs' -o -name 'aether' -o -name 'aether-*.exe' \) \
        -print -quit | grep -q .; then
        fail "Energy Pack artifact contains Kernel source or an executable"
    fi
    while IFS= read -r -d '' artifact_file; do
        [[ ! -x "$artifact_file" ]] \
            || fail "Energy Pack artifact contains executable data: $artifact_file"
    done < <(find "$temp_dir/energy.bundle" -type f -print0)
    rm -rf "$temp_dir"
    trap - RETURN

    echo "local extraction gates passed; external release/repository/CI evidence was not evaluated"
}

check_sha256() {
    local option=$1
    local digest=$2
    if ! grep -Eq '^[[:xdigit:]]{64}$' <<<"$digest"; then
        fail "$option must be exactly one SHA-256 digest"
    fi
}

check_external_evidence() {
    local missing=0
    local option value
    local expected_version run_suffix

    while IFS='|' read -r option value; do
        if [[ -z "$value" ]]; then
            echo "ERROR: missing required external evidence --$option" >&2
            missing=1
        fi
    done <<EOF
released-version|$RELEASED_VERSION
kernel-artifact-sha256|$KERNEL_ARTIFACT_SHA256
energy-pack-artifact-sha256|$ENERGY_PACK_ARTIFACT_SHA256
downstream-repository|$DOWNSTREAM_REPOSITORY
downstream-ci-run-url|$DOWNSTREAM_CI_RUN_URL
downstream-ci-commit|$DOWNSTREAM_CI_COMMIT
downstream-ci-conclusion|$DOWNSTREAM_CI_CONCLUSION
EOF
    [[ $missing -eq 0 ]] || return 1

    expected_version=$(workspace_version)
    [[ "$RELEASED_VERSION" == "$expected_version" ]] \
        || fail "--released-version must equal workspace version $expected_version"
    check_sha256 --kernel-artifact-sha256 "$KERNEL_ARTIFACT_SHA256"
    check_sha256 --energy-pack-artifact-sha256 "$ENERGY_PACK_ARTIFACT_SHA256"
    [[ "$KERNEL_ARTIFACT_SHA256" != "$ENERGY_PACK_ARTIFACT_SHA256" ]] \
        || fail "Kernel and Energy Pack artifacts must have distinct digests"
    [[ "$DOWNSTREAM_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
        || fail "--downstream-repository must be OWNER/REPOSITORY"
    case "$DOWNSTREAM_CI_RUN_URL" in
        "https://github.com/$DOWNSTREAM_REPOSITORY/actions/runs/"*) ;;
        *) fail "--downstream-ci-run-url must name an Actions run in --downstream-repository" ;;
    esac
    run_suffix=${DOWNSTREAM_CI_RUN_URL#"https://github.com/$DOWNSTREAM_REPOSITORY/actions/runs/"}
    [[ "$run_suffix" =~ ^[0-9]+(/job/[0-9]+)?/?$ ]] \
        || fail "--downstream-ci-run-url has an invalid GitHub Actions run id"
    [[ "$DOWNSTREAM_CI_COMMIT" =~ ^([[:xdigit:]]{40}|[[:xdigit:]]{64})$ ]] \
        || fail "--downstream-ci-commit must be a full Git commit id"
    [[ "$DOWNSTREAM_CI_CONCLUSION" == success ]] \
        || fail "--downstream-ci-conclusion must be success"

    echo "external extraction evidence is structurally complete"
    echo "note: this local check validates supplied evidence identifiers; it does not query or create external releases, repositories, attestations, or CI runs"
}

case "$MODE" in
    local)
        check_local_gates
        ;;
    evidence)
        check_external_evidence
        ;;
    full)
        check_external_evidence
        check_local_gates
        ;;
esac
