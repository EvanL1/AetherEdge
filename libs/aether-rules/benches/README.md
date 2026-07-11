# aether-rules benchmarks

```bash
cargo bench -p aether-rules
```

HTML report is written to `target/criterion/report/index.html`.

## Benchmark groups

| Group | What it measures |
|---|---|
| `deadband/absolute/*` | `ValueDeadband::Absolute::exceeds` — pure arithmetic, ~1 ns |
| `deadband/percent/*`  | `ValueDeadband::Percent::exceeds` — division + percent calc |
| `should_trigger_onchange/first_observation/{1,10,100}` | Fresh state, triggers immediately; scales with point count |
| `should_trigger_onchange/value_changed/{1,10,100}` | Deadband exceeded on all points |
| `should_trigger_onchange/no_trigger_deadband/{1,10,100}` | Full scan, nothing exceeds deadband (worst-case scan) |
| `should_trigger_onchange/blocked_by_time_deadband/{1,10,100}` | Returns early after time gate; should be O(1) |

## Not benchmarked yet

`fetch_point_snapshot` is async and backed by the injected live-state adapter;
the benchmark intentionally isolates the pure deadband decision function.
