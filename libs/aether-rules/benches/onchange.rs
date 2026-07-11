//! Benchmarks for the OnChange trigger hot path in aether-rules.
//!
//! Run with:
//!   cargo bench -p aether-rules
//!
//! Three benchmark groups:
//!   1. `deadband_absolute`  — `ValueDeadband::Absolute::exceeds(last, new)`
//!   2. `deadband_percent`   — `ValueDeadband::Percent::exceeds(last, new)`
//!   3. `should_trigger_*`   — `should_trigger_onchange` with 1 / 10 / 100 points
//!
//! NOTE: asynchronous live-state snapshot I/O is deliberately excluded; the
//! benchmark isolates deadband decision cost.

use aether_rules::{OnChangeState, PointKind, PointRef, ValueDeadband, should_trigger_onchange};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_point_refs(n: usize) -> Vec<PointRef> {
    (0..n as u32)
        .map(|i| PointRef {
            instance: 1,
            point_type: PointKind::Measurement,
            point: i,
        })
        .collect()
}

/// Build a snapshot where every point has a finite value.
fn make_snapshot(refs: &[PointRef], value: f64) -> HashMap<String, Option<f64>> {
    refs.iter().map(|p| (p.cache_key(), Some(value))).collect()
}

/// Build an OnChangeState that has seen `old_value` for every point in `refs`.
fn make_seen_state(refs: &[PointRef], old_value: f64) -> OnChangeState {
    let mut state = OnChangeState::default();
    for p in refs {
        state.last_value.insert(p.cache_key(), old_value);
    }
    state
}

// ── group 1: ValueDeadband::exceeds ──────────────────────────────────────────

fn bench_deadband(c: &mut Criterion) {
    let mut group = c.benchmark_group("deadband");

    let absolute = ValueDeadband::Absolute { threshold: 0.5 };
    let percent = ValueDeadband::Percent { threshold: 1.0 };

    // Pairs: (label, last, new) — mix triggering and non-triggering cases
    let cases: &[(&str, f64, f64)] = &[
        ("no_change", 100.0, 100.0),
        ("below_threshold", 100.0, 100.3),
        ("above_threshold", 100.0, 101.0),
        ("zero_crossing", 0.0, 0.1),
    ];

    for (label, last, new) in cases {
        group.bench_with_input(
            BenchmarkId::new("absolute", label),
            &(*last, *new),
            |b, &(last, new)| {
                b.iter(|| black_box(absolute.exceeds(black_box(last), black_box(new))))
            },
        );
        group.bench_with_input(
            BenchmarkId::new("percent", label),
            &(*last, *new),
            |b, &(last, new)| {
                b.iter(|| black_box(percent.exceeds(black_box(last), black_box(new))))
            },
        );
    }

    group.finish();
}

// ── group 2: should_trigger_onchange at varying point counts ─────────────────

fn bench_should_trigger(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_trigger_onchange");

    let vd = ValueDeadband::Absolute { threshold: 0.5 };
    let now = Instant::now();

    for n in [1usize, 10, 100] {
        let refs = make_point_refs(n);

        // Scenario A: fresh state (no prior observation) → always triggers
        {
            let state = OnChangeState::default();
            let snapshot = make_snapshot(&refs, 42.0);
            group.bench_with_input(BenchmarkId::new("first_observation", n), &n, |b, _| {
                b.iter(|| {
                    black_box(should_trigger_onchange(
                        black_box(&state),
                        black_box(&refs),
                        black_box(None),
                        black_box(Some(&vd)),
                        black_box(&snapshot),
                        black_box(now),
                    ))
                })
            });
        }

        // Scenario B: value changed beyond deadband → triggers
        {
            let state = make_seen_state(&refs, 100.0);
            let snapshot = make_snapshot(&refs, 101.5); // delta = 1.5 > 0.5
            group.bench_with_input(BenchmarkId::new("value_changed", n), &n, |b, _| {
                b.iter(|| {
                    black_box(should_trigger_onchange(
                        black_box(&state),
                        black_box(&refs),
                        black_box(None),
                        black_box(Some(&vd)),
                        black_box(&snapshot),
                        black_box(now),
                    ))
                })
            });
        }

        // Scenario C: value within deadband → does NOT trigger (full scan)
        {
            let state = make_seen_state(&refs, 100.0);
            let snapshot = make_snapshot(&refs, 100.2); // delta = 0.2 < 0.5
            group.bench_with_input(BenchmarkId::new("no_trigger_deadband", n), &n, |b, _| {
                b.iter(|| {
                    black_box(should_trigger_onchange(
                        black_box(&state),
                        black_box(&refs),
                        black_box(None),
                        black_box(Some(&vd)),
                        black_box(&snapshot),
                        black_box(now),
                    ))
                })
            });
        }

        // Scenario D: time deadband blocks trigger
        {
            let mut state = make_seen_state(&refs, 100.0);
            state.last_trigger = Some(now); // triggered just now
            let snapshot = make_snapshot(&refs, 101.5);
            let time_deadband_ms: Option<u64> = Some(500);
            group.bench_with_input(
                BenchmarkId::new("blocked_by_time_deadband", n),
                &n,
                |b, _| {
                    b.iter(|| {
                        black_box(should_trigger_onchange(
                            black_box(&state),
                            black_box(&refs),
                            black_box(time_deadband_ms),
                            black_box(Some(&vd)),
                            black_box(&snapshot),
                            black_box(now),
                        ))
                    })
                },
            );
        }
    }

    group.finish();
}

// ── criterion wiring ──────────────────────────────────────────────────────────

fn configured() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(Duration::from_secs(3))
}

criterion_group! {
    name = benches;
    config = configured();
    targets = bench_deadband, bench_should_trigger
}
criterion_main!(benches);
