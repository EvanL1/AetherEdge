# Aether IO resilience benchmarks

These benchmarks quantify bounded failure behavior added to the IO command and
data planes. They are deliberately separate from the normal test suite:
machine-dependent latency, throughput, and RSS values are observations, while
safety invariants still fail the benchmark immediately when violated.

Run the complete suite from the repository root:

```bash
./scripts/run-resilience-benchmarks.sh
```

JSON reports are written to a unique UTC-and-process-named directory under
`target/aether-bench` by default. A `complete.json` marker is written only
after all three targets succeed. Set `AETHER_BENCH_OUTPUT_DIR` to name a run:

```bash
AETHER_BENCH_OUTPUT_DIR="$PWD/target/aether-bench/baseline" \
  ./scripts/run-resilience-benchmarks.sh
```

The runner refuses to overwrite any report in a named directory. Choose a new
directory for every comparison so an interrupted run cannot be mistaken for a
complete baseline.

## Benchmarks

| Target | Measures | Hard invariants |
|---|---|---|
| `io_resilience` | Poll-gap p50/p95/p99/max under continuously-ready protocol, business-command, and data-event lanes; async log-admission and ticker jitter with a deliberately slow disk sink | Every source progresses; overdue polling is not starved; disk delay never runs on the Tokio polling task |
| `command_path` | One UDS admission dispatched through the production command consumer, followed by 10,000 identical retries through UDS/ledger dedup, plus committed-state SQLite process-reopen recovery | One ledger row, one queue delivery, one real `ChannelRuntime::write_control` invocation, 10,000 duplicate acknowledgements, and zero observed replay after reopen |
| `capacity_rss` | Fill, latest-wins replacement, and drain cost for the bounded DataEvent point lane; resident-memory delta | Exact configured cardinality, no hidden drops, stable cardinality during coalescing, complete drain |

Run one target directly:

```bash
cargo bench --locked -p aether-io \
  --features bench-support \
  --bench capacity_rss
```

`capacity_rss` defaults to 100,000 unique points and batches of 256. Its input
can be changed without editing code:

```bash
AETHER_BENCH_POINT_CAPACITY=250000 \
AETHER_BENCH_BATCH_SIZE=256 \
cargo bench --locked -p aether-io \
  --features bench-support \
  --bench capacity_rss
```

## Interpreting results

- Compare JSON from the same host, power policy, Rust toolchain, and build
  profile. Do not treat cross-host latency or RSS deltas as regressions.
- A p99 value is not an SLA. Production adapters, kernels, field buses, disks,
  and MQTT brokers add latency that these deterministic harnesses do not model.
- Zero duplicate adapter calls and zero process-reopen replay are correctness
  assertions within the command ledger's configured retention window. The
  reopen scenario does not claim kill-during-write or power-loss durability.
- RSS after drain may stay above its initial value because the process allocator
  can retain freed pages. `rss_fill_delta_bytes` is the useful capacity value.
- The suite uses synthetic protocol inputs and a synthetic slow disk sink. It
  does not replace soak tests with physical Modbus/CAN devices or a real broker.
