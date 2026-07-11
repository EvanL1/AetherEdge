//! Control-chain microbenchmarks for AetherEMS.
//!
//! Measures the latency of each segment in the M2C control chain:
//!   automation detects anomaly via SHM → rule engine evaluates
//!     → automation writes C/A point to SHM → UDS notify io
//!       → io dispatches to protocol adapter
//!
//! Run with:
//!   cargo bench -p aether-rtdb-shm --bench control_chain
//!
//! # Groups
//! 1. `shm_hot_path`         — PointSlot set/load, UnifiedWriter::set_action
//! 2. `m2c_uds`              — ShmNotifier round-trip to loopback UDS listener
//! 3. `shm_dispatch`         — Full ShmDispatch::dispatch() with mock notifier
//! 4. `onchange_tick_phase0` — fetch_point_snapshot / should_trigger_onchange
//!    (the paths PointWatch eliminates)

#![allow(clippy::disallowed_methods)] // benchmark code – unwrap() is fine

use std::collections::{BTreeMap, HashMap};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tempfile::tempdir;
use tokio::runtime::Runtime;

use aether_routing::{RouteContext, RoutingCache};
use aether_rtdb_shm::{
    ActionDispatch, ActionWriter, ChannelPointCounts, DispatchOutcome, PointSlot, SharedConfig,
    ShmDispatch, ShmNotifier, UnifiedReader, UnifiedWriter,
};

// ─────────────────────────────────────────────────────────────────────────────
// Shared setup helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal channel layout: channel 1001 with 4 T + 1 C + 1 A points.
fn bench_channel_points() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    // [T, S, C, A]
    map.insert(1001u32, [4u32, 0, 1, 1]);
    ChannelPointCounts::from_map(map)
}

fn bench_config(dir: &std::path::Path) -> SharedConfig {
    SharedConfig::default()
        .with_path(dir.join("bench.shm"))
        .with_max_slots(1024)
}

/// A `RouteContext` targeting channel 1001, Control point 0.
fn bench_route_ctx() -> RouteContext {
    RouteContext {
        channel_id: "1001".to_string(),
        point_type: "C".to_string(),
        io_point_id: "0".to_string(),
        target_channel_id: 1001,
        target_point_type: 2, // Control
        target_point_id: 0,
        timestamp_ms: 1_748_000_000_000i64,
        expires_at_ms: 1_748_000_005_000i64,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 1: SHM hot path (no IPC)
// ─────────────────────────────────────────────────────────────────────────────

fn bench_shm_hot_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("shm_hot_path");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(100);

    // ── point_slot_set ────────────────────────────────────────────────────────
    {
        let slot = PointSlot::new();
        group.bench_function("point_slot_set", |b| {
            let mut ts = 1_748_000_000_000u64;
            b.iter(|| {
                ts = ts.wrapping_add(1);
                black_box(&slot).set(black_box(42.0), black_box(42.0), black_box(ts));
            });
        });
    }

    // ── point_slot_load_consistent ────────────────────────────────────────────
    {
        let slot = PointSlot::new();
        slot.set(123.4, 123.4, 1_748_000_000_000);
        group.bench_function("point_slot_load_consistent", |b| {
            b.iter(|| {
                black_box(black_box(&slot).load_consistent());
            });
        });
    }

    // ── point_slot_set_then_load ──────────────────────────────────────────────
    {
        let slot = PointSlot::new();
        group.bench_function("point_slot_set_then_load", |b| {
            let mut ts = 1_748_000_000_000u64;
            b.iter(|| {
                ts = ts.wrapping_add(1);
                black_box(&slot).set(black_box(99.0), black_box(99.0), black_box(ts));
                black_box(black_box(&slot).load_consistent());
            });
        });
    }

    // ── unified_writer_set_action ─────────────────────────────────────────────
    {
        let dir = tempdir().unwrap();
        let config = bench_config(dir.path());
        let channel_points = bench_channel_points();
        let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

        group.bench_function("unified_writer_set_action", |b| {
            let mut ts = 1_748_000_000_000u64;
            b.iter(|| {
                ts = ts.wrapping_add(1);
                black_box(black_box(&writer).set_action(
                    black_box(1001),
                    black_box(2), // Control
                    black_box(0),
                    black_box(1.0),
                    black_box(ts),
                ));
            });
        });
    }

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 2: M2C dispatch — UDS loopback latency
// ─────────────────────────────────────────────────────────────────────────────

fn bench_m2c_uds(c: &mut Criterion) {
    let mut group = c.benchmark_group("m2c_uds");
    group.measurement_time(Duration::from_secs(8));
    group.sample_size(50);

    // Start a tokio runtime to host the UDS listener task and drive the notifier.
    let rt = Runtime::new().unwrap();

    // Bind a UDS listener in a tempdir, then spawn a reader task that reads
    // and discards every 56-byte message as fast as it arrives.
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("bench-m2c.sock");
    let sock_str = sock_path.to_str().unwrap().to_string();

    let listener = rt.block_on(async { tokio::net::UnixListener::bind(&sock_path).unwrap() });

    // Spawn the draining reader.
    rt.spawn(async move {
        let mut buf = [0u8; 64];
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                while stream.read(&mut buf).await.unwrap_or(0) != 0 {}
            });
        }
    });

    // Connect the notifier.
    let mut notifier = rt.block_on(async { ShmNotifier::connect(&sock_str).await.unwrap() });

    group.bench_function("shm_notifier_notify_uds_loopback", |b| {
        use aether_model::PointType;
        use aether_rtdb_shm::notifier::NotifyResult;
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            let result: NotifyResult = rt.block_on(async {
                black_box(&mut notifier)
                    .notify(
                        black_box(1001),
                        black_box(PointType::Control),
                        black_box(0),
                        black_box(1.0),
                        black_box(1_748_000_000_000),
                        black_box(1_748_000_005_000),
                    )
                    .await
            });
            black_box(result);
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 3: End-to-end ShmDispatch (mock notifier — measures SHM write + Mutex
//          + generation check overhead; UDS kernel latency measured above)
// ─────────────────────────────────────────────────────────────────────────────

fn bench_shm_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("shm_dispatch");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(100);

    let rt = Runtime::new().unwrap();
    let dir = tempdir().unwrap();
    let config = bench_config(dir.path());
    let channel_points = bench_channel_points();
    // io role creates the SHM; the dispatch path uses the automation-side
    // restricted ActionWriter, matching production.
    let _owner = UnifiedWriter::create(&config, &channel_points).unwrap();
    let writer = Arc::new(ActionWriter::open(&config, &channel_points).unwrap());

    // Wire up a ShmDispatch with the real writer but a disabled notifier
    // (path = "" → NotifyResult::off(), no kernel UDS round-trip).
    let dispatch = ShmDispatch::new();
    dispatch.set_writer(Arc::clone(&writer), config.clone());
    let notifier = Arc::new(tokio::sync::Mutex::new(ShmNotifier::disabled()));
    dispatch.set_notifier(notifier);
    let dispatch = Arc::new(dispatch);

    let ctx = bench_route_ctx();

    group.bench_function("shm_dispatch_full_path", |b| {
        b.iter(|| {
            let outcome: DispatchOutcome = rt.block_on(async {
                black_box(&*dispatch)
                    .dispatch(black_box(&ctx), black_box(42.0))
                    .await
            });
            let _ = black_box(outcome);
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 4: OnChange tick Phase 0 — the paths PointWatch will eliminate
// ─────────────────────────────────────────────────────────────────────────────

use aether_rules::{OnChangeState, PointKind, PointRef, ValueDeadband, should_trigger_onchange};

fn make_point_refs(n: usize) -> Vec<PointRef> {
    (0..n as u32)
        .map(|i| PointRef {
            instance: 1 + i / 10, // spread across a few instances
            point_type: PointKind::Measurement,
            point: i % 10,
        })
        .collect()
}

fn make_snapshot_map(refs: &[PointRef], value: f64) -> HashMap<String, Option<f64>> {
    refs.iter().map(|p| (p.cache_key(), Some(value))).collect()
}

fn make_seen_state(refs: &[PointRef], value: f64) -> OnChangeState {
    let mut state = OnChangeState::default();
    for p in refs {
        state.last_value.insert(p.cache_key(), value);
    }
    state
}

fn bench_onchange_tick(c: &mut Criterion) {
    let mut group = c.benchmark_group("onchange_tick_phase0");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(50);

    // Uses UnifiedReader::get_instance() exactly like the production
    // scheduler live-state adapter.
    for n in [10usize, 100, 1000] {
        let tmp = tempdir().unwrap();
        let config = bench_config(tmp.path());

        // Single channel with `n` T points so we can map them 1:1 to subscriptions.
        let mut map = BTreeMap::new();
        map.insert(1001u32, [n as u32, 0, 0, 0]);
        let channel_points = ChannelPointCounts::from_map(map);

        let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
        for i in 0..n as u32 {
            writer.set_direct(i as usize, 220.0 + i as f64 * 0.1, 220.0, 1000);
        }
        drop(writer);

        // C2M routing: (1001, T, i) → (1+i/10, M, i%10). Same distribution as
        // populate_memory_rtdb so the two benches measure the same shape.
        let mut c2m_data = std::collections::HashMap::new();
        for i in 0..n as u32 {
            let inst = 1 + i / 10;
            let pt = i % 10;
            c2m_data.insert(format!("1001:T:{i}"), format!("{inst}:M:{pt}"));
        }
        let routing = RoutingCache::from_maps(
            c2m_data,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );

        let reader = UnifiedReader::open(&config, &channel_points).unwrap();

        let subs: Vec<PointRef> = (0..n as u32)
            .map(|i| PointRef {
                instance: 1 + i / 10,
                point_type: PointKind::Measurement,
                point: i % 10,
            })
            .collect();

        group.bench_with_input(BenchmarkId::new("phase0_shm_direct", n), &n, |b, _| {
            b.iter(|| {
                let mut out: HashMap<String, Option<f64>> = HashMap::with_capacity(subs.len());
                for pref in &subs {
                    let v = reader
                        .get_instance(pref.instance, 0u8, pref.point, &routing)
                        .map(|(v, _)| v)
                        .filter(|x| x.is_finite());
                    out.insert(pref.cache_key(), v);
                }
                black_box(out)
            });
        });
    }

    // ── should_trigger_onchange (pure CPU, no I/O) ────────────────────────────
    let vd = ValueDeadband::Absolute { threshold: 0.5 };
    let now = std::time::Instant::now();

    for n in [10usize, 100, 1000] {
        let refs = make_point_refs(n);

        // Scenario: value changed beyond deadband → triggers
        {
            let state = make_seen_state(&refs, 100.0);
            let snapshot = make_snapshot_map(&refs, 101.5);
            group.bench_with_input(
                BenchmarkId::new("should_trigger_value_changed", n),
                &n,
                |b, _| {
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
                },
            );
        }

        // Scenario: no trigger (full scan, deadband blocks)
        {
            let state = make_seen_state(&refs, 100.0);
            let snapshot = make_snapshot_map(&refs, 100.2);
            group.bench_with_input(
                BenchmarkId::new("should_trigger_no_change", n),
                &n,
                |b, _| {
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
                },
            );
        }
    }

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Criterion wiring
// ─────────────────────────────────────────────────────────────────────────────

fn configured() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(Duration::from_secs(5))
        .warm_up_time(Duration::from_secs(1))
}

criterion_group! {
    name = benches;
    config = configured();
    targets =
        bench_shm_hot_path,
        bench_m2c_uds,
        bench_shm_dispatch,
        bench_onchange_tick
}
criterion_main!(benches);
