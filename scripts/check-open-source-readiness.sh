#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

readonly REQUIRED_FILES=(
    README.md
    README-CN.md
    LICENSE-MIT
    LICENSE-APACHE
    NOTICE
    CONTRIBUTING.md
    SECURITY.md
    CODE_OF_CONDUCT.md
    GOVERNANCE.md
    SUPPORT.md
    deny.toml
    .github/dependabot.yml
    .github/PULL_REQUEST_TEMPLATE.md
    .github/ISSUE_TEMPLATE/bug_report.yml
    .github/ISSUE_TEMPLATE/feature_request.yml
    .github/ISSUE_TEMPLATE/config.yml
    .github/workflows/security.yml
)

readonly SDK_SOURCE_PACKAGES=(
    "aether-domain:crates/aether-domain"
    "aether-cloudlink:crates/aether-cloudlink"
    "aether-dataplane:crates/aether-dataplane"
    "aether-ports:crates/aether-ports"
    "aether-application:crates/aether-application"
    "aether-pack:crates/aether-pack"
    "aether-data-processing:crates/aether-data-processing"
    "aether-edge-sdk:crates/aether-sdk"
    "aether-testkit:crates/aether-testkit"
    "aether-store-local:libs/aether-store-local"
    "aether-shm-bridge:libs/aether-shm-bridge"
    "aether-http-data-processor:services/api/adapters/http-data-processor"
    "aether-sqlite-history-query:services/api/adapters/sqlite-history-query"
)

# ADR-0022: the crates.io release set is aether-edge-sdk plus the transitive
# closure of its normal and optional dependencies, plus aether-testkit for
# port conformance suites. Adding a name here makes it a permanent public
# registry entry, so this list is the deliberate gate. Every Rust package in
# the workspace that is absent from it must keep publish = false.
readonly REGISTRY_RELEASE_PACKAGES=(
    aether-application
    aether-cloudlink
    aether-data-processing
    aether-dataplane
    aether-domain
    aether-edge-sdk
    aether-pack
    aether-ports
    aether-shm-bridge
    aether-store-local
    aether-testkit
)

generate_validation_credential() {
    if ! command -v openssl >/dev/null 2>&1; then
        echo "openssl is required to generate ephemeral Compose validation credentials" >&2
        return 1
    fi
    openssl rand -hex 32
}

readonly COMPOSE_VALIDATION_JWT_SECRET="$(generate_validation_credential)"
readonly COMPOSE_VALIDATION_UPLINK_TOKEN="$(generate_validation_credential)"

failures=0

fail() {
    echo "ERROR: $*" >&2
    failures=$((failures + 1))
}

if [[ -e LICENSE ]]; then
    fail "root LICENSE must not combine multiple standard licenses; keep LICENSE-MIT and LICENSE-APACHE separate"
fi

if [[ "$COMPOSE_VALIDATION_JWT_SECRET" == "$COMPOSE_VALIDATION_UPLINK_TOKEN" ]]; then
    fail "generated Compose validation credentials must be distinct"
fi

manifest_package_name() {
    awk '
        /^\[package\][[:space:]]*$/ { in_package = 1; next }
        /^\[/ && in_package { exit }
        in_package && /^name[[:space:]]*=/ {
            line = $0
            sub(/^[^=]*=[[:space:]]*"/, "", line)
            sub(/"[[:space:]]*$/, "", line)
            print line
            exit
        }
    ' "$1"
}

is_sdk_source_package() {
    local candidate=$1
    local entry
    for entry in "${SDK_SOURCE_PACKAGES[@]}"; do
        if [[ ${entry%%:*} == "$candidate" ]]; then
            return 0
        fi
    done
    return 1
}

is_registry_release_package() {
    local candidate=$1
    local entry
    for entry in "${REGISTRY_RELEASE_PACKAGES[@]}"; do
        if [[ $entry == "$candidate" ]]; then
            return 0
        fi
    done
    return 1
}

echo "Checking community health and supply-chain policy files..."
for path in "${REQUIRED_FILES[@]}"; do
    if [[ ! -s "$path" ]]; then
        fail "required open-source file is missing or empty: $path"
    fi
done

if ! rg -q '^channel[[:space:]]*=[[:space:]]*"1\.90\.0"' rust-toolchain.toml; then
    fail "rust-toolchain.toml must pin Rust 1.90.0"
fi

echo "Checking SDK source package metadata..."
for entry in "${SDK_SOURCE_PACKAGES[@]}"; do
    package=${entry%%:*}
    directory=${entry#*:}
    manifest="$directory/Cargo.toml"

    if [[ ! -s "$manifest" ]]; then
        fail "$package manifest is missing: $manifest"
        continue
    fi
    if ! rg -q "^name[[:space:]]*=[[:space:]]*\"$package\"" "$manifest"; then
        fail "$manifest does not declare package name $package"
    fi
    if ! rg -q '^version(\.workspace)?[[:space:]]*=' "$manifest"; then
        fail "$manifest does not declare or inherit a version"
    fi
    if ! rg -q '^edition[[:space:]]*=[[:space:]]*"2024"' "$manifest"; then
        fail "$manifest must use Rust edition 2024"
    fi
    if ! rg -q '^rust-version[[:space:]]*=[[:space:]]*"1\.90(\.0)?"' "$manifest"; then
        fail "$manifest must declare MSRV 1.90"
    fi
    if ! rg -q '^description[[:space:]]*=[[:space:]]*"[^"].*"' "$manifest"; then
        fail "$manifest must declare a non-empty description"
    fi
    if ! rg -q '^license(\.workspace)?[[:space:]]*=' "$manifest"; then
        fail "$manifest must declare or inherit an SPDX license expression"
    fi
    if rg -q '^license-file[[:space:]]*=' "$manifest"; then
        fail "$manifest must not combine license with license-file"
    fi
    for license_name in LICENSE-MIT LICENSE-APACHE; do
        package_license="$directory/$license_name"
        if [[ ! -s "$package_license" ]]; then
            fail "$package must include $license_name in its package root"
        elif ! cmp -s "$license_name" "$package_license"; then
            fail "$package_license differs from the repository license text"
        fi
    done
    if ! rg -q '^repository(\.workspace)?[[:space:]]*=' "$manifest"; then
        fail "$manifest must declare or inherit its repository"
    fi
    if ! rg -q '^documentation[[:space:]]*=[[:space:]]*"https://docs\.aetheriot\.dev/' "$manifest"; then
        fail "$manifest must link to the versioned AetherEdge documentation"
    fi
    if ! rg -q '^readme[[:space:]]*=[[:space:]]*"README\.md"' "$manifest"; then
        fail "$manifest must declare README.md"
    fi
    if [[ ! -s "$directory/README.md" ]]; then
        fail "$package README is missing or empty: $directory/README.md"
    fi
    if is_registry_release_package "$package"; then
        if rg -q '^publish[[:space:]]*=[[:space:]]*false' "$manifest"; then
            fail "$package is in the ADR-0022 registry release set and must not set publish=false"
        fi
    elif ! rg -q '^publish[[:space:]]*=[[:space:]]*false' "$manifest"; then
        fail "$package is a source-only implementation package and must set publish=false"
    fi
done

echo "Checking that every package outside the registry release set is private..."
while IFS= read -r manifest; do
    package=$(manifest_package_name "$manifest")
    if [[ -z "$package" ]] || is_sdk_source_package "$package"; then
        continue
    fi
    if is_registry_release_package "$package"; then
        fail "$package is published but missing from SDK_SOURCE_PACKAGES metadata checks"
    fi
    if ! rg -q '^publish[[:space:]]*=[[:space:]]*false' "$manifest"; then
        fail "$package must set publish=false in $manifest"
    fi
done < <(
    find workspace-hack crates examples libs services tools firmware \
        -name Cargo.toml -type f -print | sort
)

echo "Checking the default Compose runtime has no external database..."
runtime_snapshot_line=$(grep -nFx 'snapshot_runtime_data_for_rollback' scripts/install.sh \
    | tail -1 | cut -d: -f1)
compose_secret_line=$(grep -nFx 'ensure_compose_jwt_secret' scripts/install.sh \
    | tail -1 | cut -d: -f1)
compose_publish_line=$(grep -nF 'publish_compose_atomically "docker-compose.yml"' \
    scripts/install.sh | tail -1 | cut -d: -f1)
compose_start_line=$(grep -nF 'run_docker_compose up -d --force-recreate' \
    scripts/install.sh | tail -1 | cut -d: -f1)
if [[ -z "$runtime_snapshot_line" || -z "$compose_secret_line" \
    || -z "$compose_publish_line" \
    || -z "$compose_start_line" \
    || "$runtime_snapshot_line" -ge "$compose_secret_line" \
    || "$compose_secret_line" -ge "$compose_publish_line" \
    || "$compose_secret_line" -ge "$compose_start_line" ]]; then
    fail "install.sh must snapshot .env before establishing JWT identity and publishing Compose"
fi
if awk '
    /^run_docker_compose\(\)[[:space:]]*\{/ { in_wrapper = 1; next }
    in_wrapper && /ensure_compose_jwt_secret/ { mutates_secret = 1 }
    in_wrapper && /^}/ { exit }
    END { exit(mutates_secret ? 0 : 1) }
' scripts/install.sh; then
    fail "the Compose wrapper must not mutate secrets during rollback"
fi
if ! rg -q '\$SUDO chmod 600 "\$env_file"' scripts/install.sh; then
    fail "install.sh must keep the Compose .env file at mode 0600"
fi

readonly CI_SETUP_ACTION='.github/actions/setup-rust-env/action.yml'
if ! rg -Fq 'echo "JWT_SECRET_KEY=$jwt_secret" >> "$GITHUB_ENV"' "$CI_SETUP_ACTION" \
    || ! rg -Fq 'echo "AETHER_UPLINK_CONTROL_TOKEN=$uplink_token" >> "$GITHUB_ENV"' \
        "$CI_SETUP_ACTION"; then
    fail "$CI_SETUP_ACTION must generate ephemeral CI credentials"
fi

while IFS= read -r workflow; do
    if ! rg -Fq 'uses: ./.github/actions/setup-rust-env' "$workflow"; then
        fail "$workflow invokes Docker Compose without the credential-generating CI setup action"
    fi
done < <(rg -l 'docker compose' .github/workflows --glob '*.yml' --glob '*.yaml' || true)

if ! command -v docker >/dev/null 2>&1 || ! docker compose version >/dev/null 2>&1; then
    fail "docker with Compose support is required to validate docker-compose.yml"
else
    if AETHER_UPLINK_CONTROL_TOKEN="$COMPOSE_VALIDATION_UPLINK_TOKEN" \
        JWT_SECRET_KEY='' docker compose -f docker-compose.yml config >/dev/null 2>&1; then
        fail "docker-compose.yml must reject an empty JWT_SECRET_KEY"
    fi

    default_services=""
    if ! default_services=$(
        JWT_SECRET_KEY="$COMPOSE_VALIDATION_JWT_SECRET" \
            AETHER_UPLINK_CONTROL_TOKEN="$COMPOSE_VALIDATION_UPLINK_TOKEN" \
            docker compose -f docker-compose.yml config --services
    ); then
        fail "default docker-compose.yml failed with a valid JWT test key"
    fi
    for service in aether-redis timescaledb; do
        if rg -q "^${service}$" <<<"$default_services"; then
            fail "$service is enabled in the default Compose runtime"
        fi
    done

    redis_services=""
    if ! redis_services=$(
        JWT_SECRET_KEY="$COMPOSE_VALIDATION_JWT_SECRET" \
            AETHER_UPLINK_CONTROL_TOKEN="$COMPOSE_VALIDATION_UPLINK_TOKEN" \
            docker compose -f docker-compose.yml --profile redis config --services
    ); then
        fail "the optional Redis infrastructure profile is invalid"
    fi
    if ! rg -q '^aether-redis$' <<<"$redis_services"; then
        fail "the optional Redis infrastructure profile is missing"
    fi

    postgres_services=""
    if ! postgres_services=$(
        JWT_SECRET_KEY="$COMPOSE_VALIDATION_JWT_SECRET" \
            AETHER_UPLINK_CONTROL_TOKEN="$COMPOSE_VALIDATION_UPLINK_TOKEN" \
            docker compose -f docker-compose.yml --profile postgres-storage config --services
    ); then
        fail "the optional PostgreSQL history profile is invalid"
    fi
    if ! rg -q '^timescaledb$' <<<"$postgres_services"; then
        fail "the optional PostgreSQL history profile is missing"
    fi
fi

echo "Checking the signed source-release boundary..."
# ADR-0022 keeps the signed source release and adds a registry release beside
# it. The registry step must publish the workspace as one ordered unit rather
# than hand-rolling a per-crate order, and must run only after the GitHub
# Release job has succeeded.
if ! rg -q 'cargo publish --workspace --locked' .github/workflows/release.yml; then
    fail "the release workflow must publish the registry set with cargo publish --workspace --locked"
fi
if ! rg -q '^\s+needs: release$' .github/workflows/release.yml; then
    fail "the crates.io job must run only after the GitHub Release job succeeds"
fi
if ! rg -q 'aetheriot-source-\$\{GITHUB_REF_NAME\}\.tar\.gz' \
    .github/workflows/release.yml; then
    fail "the source release workflow must build a versioned source archive"
fi
if ! rg -q 'release/aetheriot-source-\*\.tar\.gz' \
    .github/workflows/release.yml; then
    fail "the source archive must be covered by release provenance"
fi

if ((failures > 0)); then
    echo "Open-source readiness failed with $failures error(s)" >&2
    exit 1
fi

echo "Open-source readiness passed"
