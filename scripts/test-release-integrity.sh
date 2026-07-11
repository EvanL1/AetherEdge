#!/usr/bin/env bash
# shellcheck disable=SC2016 # GitHub Actions expressions are asserted as literals.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALLER="$ROOT_DIR/tools/aether/install.sh"
RELEASE_WORKFLOW="$ROOT_DIR/.github/workflows/release.yml"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_file_contains() {
    local file=$1
    local expected=$2

    grep -Fq "$expected" "$file" \
        || fail "$file does not contain required release-integrity rule: $expected"
}

link_command() {
    local destination=$1
    local command_name=$2
    local command_path

    command_path="$(command -v "$command_name")" \
        || fail "test prerequisite is missing: $command_name"
    ln -s "$command_path" "$destination/$command_name"
}

create_test_path() {
    local bin_dir=$1
    local command_name

    for command_name in awk basename chmod cp grep head mkdir mktemp rm sed touch tr; do
        link_command "$bin_dir" "$command_name"
    done
}

write_test_commands() {
    local bin_dir=$1
    local hash_command=$2

    cat > "$bin_dir/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) printf '%s\n' "$AETHER_TEST_OS" ;;
    -m) printf '%s\n' "$AETHER_TEST_ARCH" ;;
    *) exit 2 ;;
esac
EOF

    cat > "$bin_dir/curl" <<'EOF'
#!/bin/sh
output=''
url=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            output=$2
            shift 2
            ;;
        -*) shift ;;
        *)
            url=$1
            shift
            ;;
    esac
done

case "$url" in
    https://api.github.com/*)
        printf '{"tag_name":"v1.2.3"}\n'
        ;;
    *.sha256)
        filename=${url##*/}
        filename=${filename%.sha256}
        printf '%s  %s\n' "$AETHER_TEST_EXPECTED_HASH" "$filename" > "$output"
        printf 'checksum-download\n' >> "$AETHER_TEST_EVENTS"
        ;;
    *)
        printf 'test archive payload\n' > "$output"
        printf 'archive-download\n' >> "$AETHER_TEST_EVENTS"
        ;;
esac
EOF

    cat > "$bin_dir/tar" <<'EOF'
#!/bin/sh
destination=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        -C)
            destination=$2
            shift 2
            ;;
        *) shift ;;
    esac
done
printf 'tar\n' >> "$AETHER_TEST_EVENTS"
mkdir -p "$destination"
touch "$destination/aether"
EOF

    if [[ -n "$hash_command" ]]; then
        cat > "$bin_dir/$hash_command" <<'EOF'
#!/bin/sh
command_name=${0##*/}
archive=''
for argument in "$@"; do
    archive=$argument
done
[ -f "$archive" ] || exit 3
case "$archive" in
    *.sha256) exit 4 ;;
esac
printf '%s\n' "$command_name" >> "$AETHER_TEST_EVENTS"
printf '%s  %s\n' "$AETHER_TEST_ACTUAL_HASH" "$archive"
EOF
    fi

    chmod +x "$bin_dir/uname" "$bin_dir/curl" "$bin_dir/tar"
    if [[ -n "$hash_command" ]]; then
        chmod +x "$bin_dir/$hash_command"
    fi
}

run_installer_case() {
    local case_name=$1
    local os=$2
    local arch=$3
    local hash_command=$4
    local expected_hash=$5
    local actual_hash=$6
    local expected_status=$7
    local case_dir bin_dir status

    case_dir="$TEST_ROOT/$case_name"
    bin_dir="$case_dir/bin"
    mkdir -p "$bin_dir" "$case_dir/home" "$case_dir/install"
    create_test_path "$bin_dir"
    write_test_commands "$bin_dir" "$hash_command"

    status=0
    PATH="$bin_dir" \
        HOME="$case_dir/home" \
        AETHER_INSTALL_DIR="$case_dir/install" \
        AETHER_TEST_OS="$os" \
        AETHER_TEST_ARCH="$arch" \
        AETHER_TEST_EXPECTED_HASH="$expected_hash" \
        AETHER_TEST_ACTUAL_HASH="$actual_hash" \
        AETHER_TEST_EVENTS="$case_dir/events" \
        /bin/bash "$INSTALLER" > "$case_dir/stdout" 2> "$case_dir/stderr" || status=$?

    if [[ "$expected_status" == success && $status -ne 0 ]]; then
        cat "$case_dir/stderr" >&2
        fail "$case_name: installer unexpectedly failed with status $status"
    fi
    if [[ "$expected_status" == failure && $status -eq 0 ]]; then
        fail "$case_name: installer unexpectedly succeeded"
    fi

    CASE_DIR=$case_dir
}

assert_before() {
    local file=$1
    local first=$2
    local second=$3
    local first_line second_line

    first_line="$(grep -nFx "$first" "$file" | head -1 | sed 's/:.*//')"
    second_line="$(grep -nFx "$second" "$file" | head -1 | sed 's/:.*//')"
    [[ -n "$first_line" ]] || fail "missing event '$first' in $file"
    [[ -n "$second_line" ]] || fail "missing event '$second' in $file"
    (( first_line < second_line )) \
        || fail "event '$first' must occur before '$second'"
}

readonly MATCHING_HASH="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
readonly DIFFERENT_HASH="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT

echo "Testing Linux checksum verification before extraction..."
run_installer_case linux-success Linux x86_64 sha256sum "$MATCHING_HASH" "$MATCHING_HASH" success
assert_before "$CASE_DIR/events" checksum-download sha256sum
assert_before "$CASE_DIR/events" sha256sum tar
[[ -x "$CASE_DIR/install/aether" ]] || fail "Linux installer did not install the verified binary"

echo "Testing macOS uses shasum before extraction..."
run_installer_case macos-success Darwin arm64 shasum "$MATCHING_HASH" "$MATCHING_HASH" success
assert_before "$CASE_DIR/events" checksum-download shasum
assert_before "$CASE_DIR/events" shasum tar

echo "Testing checksum mismatch fails closed..."
run_installer_case checksum-mismatch Linux x86_64 sha256sum "$MATCHING_HASH" "$DIFFERENT_HASH" failure
if [[ -f "$CASE_DIR/events" ]] && grep -Fxq tar "$CASE_DIR/events"; then
    fail "checksum mismatch reached archive extraction"
fi

echo "Testing a malformed checksum fails closed..."
run_installer_case malformed-checksum Linux x86_64 sha256sum not-a-sha256 "$MATCHING_HASH" failure
if [[ -f "$CASE_DIR/events" ]] && grep -Fxq tar "$CASE_DIR/events"; then
    fail "malformed checksum reached archive extraction"
fi

echo "Testing a missing checksum tool fails closed..."
run_installer_case missing-checksum-tool Linux x86_64 '' "$MATCHING_HASH" "$MATCHING_HASH" failure
if [[ -f "$CASE_DIR/events" ]] && grep -Fxq tar "$CASE_DIR/events"; then
    fail "missing checksum tool reached archive extraction"
fi

echo "Testing a missing macOS checksum tool fails closed..."
run_installer_case missing-macos-checksum-tool Darwin arm64 '' "$MATCHING_HASH" "$MATCHING_HASH" failure
if [[ -f "$CASE_DIR/events" ]] && grep -Fxq tar "$CASE_DIR/events"; then
    fail "missing macOS checksum tool reached archive extraction"
fi

echo "Testing platforms without release artifacts fail before download..."
run_installer_case unsupported-macos-x86 Darwin x86_64 shasum "$MATCHING_HASH" "$MATCHING_HASH" failure
[[ ! -s "$CASE_DIR/events" ]] \
    || fail "unsupported macOS x86_64 attempted a release download"
grep -Fq 'no published Aether CLI artifact' "$CASE_DIR/stderr" \
    || fail "unsupported macOS x86_64 did not explain the release matrix"

run_installer_case unsupported-windows-arm64 MINGW64_NT arm64 sha256sum "$MATCHING_HASH" "$MATCHING_HASH" failure
[[ ! -s "$CASE_DIR/events" ]] \
    || fail "unsupported Windows arm64 attempted a release download"

echo "Testing full installer checksums remain in the release workflow..."
# The following arguments are literal GitHub Actions and shell snippets.
assert_file_contains "$RELEASE_WORKFLOW" './scripts/test-release-integrity.sh'
assert_file_contains "$RELEASE_WORKFLOW" 'sha256sum "$ARTIFACT_NAME" > "${ARTIFACT_NAME}.sha256"'
assert_file_contains "$RELEASE_WORKFLOW" 'sha256sum "$AETHER_TAR_NAME" > "${AETHER_TAR_NAME}.sha256"'
assert_file_contains "$RELEASE_WORKFLOW" 'release/${{ steps.version.outputs.artifact_name }}.sha256'
assert_file_contains "$RELEASE_WORKFLOW" 'release/AetherEdge-arm64-${{ steps.version.outputs.version }}.run.sha256'
assert_file_contains "$RELEASE_WORKFLOW" 'release/AetherEdge-amd64-${{ steps.version.outputs.version }}.run.sha256'
assert_file_contains "$RELEASE_WORKFLOW" '(cd release && sha256sum -c ./*.sha256)'

echo "Release integrity tests passed."
