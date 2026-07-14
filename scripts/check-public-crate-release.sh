#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CATALOG="$SCRIPT_DIR/public-crates.txt"
METADATA_JSON=''
CATALOG_ONLY=false

usage() {
    cat <<'EOF'
Usage: scripts/check-public-crate-release.sh [options]

Validate the complete public-crate catalog and, by default, assemble every
crate archive and compile a clean-room consumer from those exact archives.

Options:
  --catalog-only         Validate catalog completeness and dependency order only
  --catalog PATH         Override the ordered public-crate catalog
  --metadata-json PATH   Use captured Cargo metadata (for deterministic tests)
  -h, --help             Show this help
EOF
}

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

while (($# > 0)); do
    case "$1" in
        --catalog-only)
            CATALOG_ONLY=true
            shift
            ;;
        --catalog)
            (($# >= 2)) || fail "--catalog requires a path"
            CATALOG=$2
            shift 2
            ;;
        --metadata-json)
            (($# >= 2)) || fail "--metadata-json requires a path"
            METADATA_JSON=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

command -v jq >/dev/null 2>&1 || fail "jq is required to validate Cargo metadata"
[[ -s "$CATALOG" ]] || fail "public crate catalog is missing or empty: $CATALOG"

TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEMP_ROOT"' EXIT

if [[ -z "$METADATA_JSON" ]]; then
    METADATA_JSON="$TEMP_ROOT/cargo-metadata.json"
    cargo metadata --format-version 1 --no-deps > "$METADATA_JSON"
fi
[[ -s "$METADATA_JSON" ]] || fail "Cargo metadata is missing or empty: $METADATA_JSON"
jq -e '.workspace_root and (.packages | type == "array")' "$METADATA_JSON" >/dev/null \
    || fail "invalid Cargo metadata document: $METADATA_JSON"

WORKSPACE_ROOT="$(jq -r '.workspace_root' "$METADATA_JSON")"
[[ -d "$WORKSPACE_ROOT" ]] || fail "metadata workspace root does not exist: $WORKSPACE_ROOT"

declare -a PUBLIC_PACKAGES=()
declare -a PUBLIC_DIRECTORIES=()
declare -A PACKAGE_POSITION=()

line_number=0
while IFS=$'\t' read -r package directory extra || [[ -n "${package:-}${directory:-}${extra:-}" ]]; do
    line_number=$((line_number + 1))
    [[ -z "${package//[[:space:]]/}" ]] && continue
    [[ "$package" == \#* ]] && continue
    [[ -n "$package" && -n "$directory" && -z "${extra:-}" ]] \
        || fail "invalid public crate catalog entry at $CATALOG:$line_number"
    [[ "$package" =~ ^[a-z0-9][a-z0-9_-]*$ ]] \
        || fail "invalid public crate package at $CATALOG:$line_number: $package"
    [[ "$directory" != /* && "$directory" != *'..'* ]] \
        || fail "catalog directory must be a confined workspace-relative path: $directory"
    if [[ -n "${PACKAGE_POSITION[$package]+present}" ]]; then
        fail "duplicate public crate catalog package: $package"
    fi

    PACKAGE_POSITION[$package]=${#PUBLIC_PACKAGES[@]}
    PUBLIC_PACKAGES+=("$package")
    PUBLIC_DIRECTORIES+=("$directory")
done < "$CATALOG"

((${#PUBLIC_PACKAGES[@]} > 0)) || fail "public crate catalog contains no packages"

metadata_package_count() {
    local package=$1
    jq --arg package "$package" '[.packages[] | select(.name == $package)] | length' \
        "$METADATA_JSON"
}

metadata_publishable() {
    local package=$1
    jq -e --arg package "$package" \
        '.packages[] | select(.name == $package and .publish != [])' \
        "$METADATA_JSON" >/dev/null
}

for index in "${!PUBLIC_PACKAGES[@]}"; do
    package=${PUBLIC_PACKAGES[$index]}
    directory=${PUBLIC_DIRECTORIES[$index]}
    count="$(metadata_package_count "$package")"
    [[ "$count" == 1 ]] || fail "catalog package must resolve exactly once in Cargo metadata: $package"
    metadata_publishable "$package" || fail "catalog package is marked private: $package"

    manifest_path="$(jq -r --arg package "$package" \
        '.packages[] | select(.name == $package) | .manifest_path' "$METADATA_JSON")"
    actual_directory="$(cd "$(dirname "$manifest_path")" && pwd -P)"
    expected_directory="$(cd "$WORKSPACE_ROOT/$directory" && pwd -P)"
    [[ "$actual_directory" == "$expected_directory" ]] \
        || fail "catalog path for $package is $directory, metadata resolves $actual_directory"
done

while IFS= read -r package; do
    [[ -n "${PACKAGE_POSITION[$package]+present}" ]] \
        || fail "publishable workspace package is missing from the catalog: $package"
done < <(jq -r '.packages[] | select(.publish != []) | .name' "$METADATA_JSON" | sort -u)

while IFS=$'\t' read -r package dependency dependency_publish; do
    [[ -n "$package" && -n "$dependency" ]] || continue
    if [[ "$dependency_publish" == private ]]; then
        fail "public package $package depends on private workspace package $dependency"
    fi
    [[ -n "${PACKAGE_POSITION[$dependency]+present}" ]] \
        || fail "public dependency is missing from the catalog: $dependency (required by $package)"
    if (( PACKAGE_POSITION[$package] <= PACKAGE_POSITION[$dependency] )); then
        fail "$package must appear after dependency $dependency in the public crate catalog"
    fi
done < <(
    jq -r '
        [.packages[] | {name, publish}] as $workspace
        | .packages[] as $package
        | select($package.publish != [])
        | $package.dependencies[]
        | select(.path != null)
        | .name as $dependency
        | ($workspace[] | select(.name == $dependency)) as $target
        | [$package.name, $dependency, (if $target.publish == [] then "private" else "public" end)]
        | @tsv
    ' "$METADATA_JSON"
)

versions="$(for package in "${PUBLIC_PACKAGES[@]}"; do
    jq -r --arg package "$package" '.packages[] | select(.name == $package) | .version' \
        "$METADATA_JSON"
done | sort -u)"
[[ "$(wc -l <<< "$versions" | tr -d ' ')" == 1 ]] \
    || fail "public crates must share one release version; found: ${versions//$'\n'/, }"
PUBLIC_VERSION="$versions"

echo "public crate catalog valid: ${#PUBLIC_PACKAGES[@]} packages at version $PUBLIC_VERSION"
if [[ "$CATALOG_ONLY" == true ]]; then
    exit 0
fi

[[ "$WORKSPACE_ROOT" == "$ROOT_DIR" ]] \
    || fail "archive verification requires metadata from the current workspace"

PACKAGE_ROOT="$TEMP_ROOT/packages"
CONSUMER_ROOT="$TEMP_ROOT/consumer"
mkdir -p "$PACKAGE_ROOT" "$CONSUMER_ROOT/src"

for package in "${PUBLIC_PACKAGES[@]}"; do
    echo "Packaging $package@$PUBLIC_VERSION..."
    cargo package \
        --package "$package" \
        --allow-dirty \
        --exclude-lockfile \
        --no-verify \
        --quiet
    archive="$ROOT_DIR/target/package/${package}-${PUBLIC_VERSION}.crate"
    [[ -s "$archive" ]] || fail "cargo package did not create $archive"
    tar -xzf "$archive" -C "$PACKAGE_ROOT"
    [[ -s "$PACKAGE_ROOT/${package}-${PUBLIC_VERSION}/Cargo.toml" ]] \
        || fail "packaged manifest is missing for $package"
done

cat > "$CONSUMER_ROOT/Cargo.toml" <<EOF
[package]
name = "aether-public-crate-consumer"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
EOF

for package in "${PUBLIC_PACKAGES[@]}"; do
    printf '%s = "=%s"\n' "$package" "$PUBLIC_VERSION" >> "$CONSUMER_ROOT/Cargo.toml"
done

cat >> "$CONSUMER_ROOT/Cargo.toml" <<'EOF'

[patch.crates-io]
EOF

for package in "${PUBLIC_PACKAGES[@]}"; do
    package_path="$PACKAGE_ROOT/${package}-${PUBLIC_VERSION}"
    printf '%s = { path = "%s" }\n' "$package" "$package_path" \
        >> "$CONSUMER_ROOT/Cargo.toml"
done

cat > "$CONSUMER_ROOT/src/lib.rs" <<'EOF'
//! Clean-room compilation probe for packaged Aether crates.
EOF

echo "Compiling a clean-room consumer from packaged crate contents..."
CARGO_TARGET_DIR="${AETHER_PUBLIC_CRATE_TARGET_DIR:-$ROOT_DIR/target/public-crate-check}" \
    cargo check --manifest-path "$CONSUMER_ROOT/Cargo.toml"

echo "Public crate release readiness passed."
