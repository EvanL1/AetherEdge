#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUN_ID="$(date -u '+%Y%m%dT%H%M%SZ')-$$"
OUTPUT_DIR="${AETHER_BENCH_OUTPUT_DIR:-$REPO_ROOT/target/aether-bench/$RUN_ID}"

reports=(
  io-resilience.json
  command-path.json
  capacity-rss.json
)

for report in "${reports[@]}" complete.json; do
  if [[ -e "$OUTPUT_DIR/$report" ]]; then
    printf 'Refusing to mix benchmark runs: %s already exists\n' "$OUTPUT_DIR/$report" >&2
    exit 2
  fi
done

mkdir -p "$OUTPUT_DIR"
export AETHER_BENCH_OUTPUT_DIR="$OUTPUT_DIR"

cd "$REPO_ROOT"

benches=(
  io_resilience
  command_path
  capacity_rss
)

for bench in "${benches[@]}"; do
  cargo bench --locked -p aether-io \
    --features bench-support \
    --bench "$bench"
done

for report in "${reports[@]}"; do
  if [[ ! -s "$OUTPUT_DIR/$report" ]]; then
    printf 'Benchmark did not produce a non-empty report: %s\n' "$OUTPUT_DIR/$report" >&2
    exit 1
  fi
done

completion_tmp="$OUTPUT_DIR/complete.json.tmp.$$"
printf '%s\n' \
  '{"status":"complete","reports":["io-resilience.json","command-path.json","capacity-rss.json"]}' \
  > "$completion_tmp"
mv "$completion_tmp" "$OUTPUT_DIR/complete.json"

printf 'AetherEdge resilience benchmark reports: %s\n' "$OUTPUT_DIR"
