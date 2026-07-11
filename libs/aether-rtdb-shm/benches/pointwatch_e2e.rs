//! PointWatch end-to-end latency benchmark
//!
//! Measures the wall-clock latency of the full event-driven path:
//!
//! ```text
//! UnifiedWriter::set_direct       (writes seqlock + invokes PointWatchSignaler::emit)
//!   │
//!   ├─ emit: bitmap.is_watched(slot) check
//!   │       → if yes, mpsc::try_send(PointWatchEvent)
//!   │
//!   ▼
//! drain_task           (background tokio task in producer "process")
//!   │
//!   ├─ batches PointWatchEvents (up to 32 per batch)
//!   │  writes 56 × N bytes to UDS socket
//!   │
//!   ▼
//! kernel UDS round-trip
//!   │
//!   ▼
//! PointWatchListener   (background tokio task in consumer "process")
//!   │
//!   ├─ read_exact 56 bytes per event
//!   │  tx.send(event) on consumer's mpsc
//!   │
//!   ▼
//! event_rx.recv().await        (measured wall-clock end)
//! ```
//!
//! Limitation: this bench uses a single OS process with a single Tokio runtime.
//! It captures the real kernel UDS cost and task-scheduling overhead, but
//! does NOT capture cross-process scheduler-wake / address-space-switch
//! delays. Empirically the difference on Linux/ARM is <2 µs, but consumers
//! of these numbers should add a small margin (~5 %) for production.
//!
//! Run:
//!
//! ```bash
//! cargo bench -p aether-rtdb-shm --bench pointwatch_e2e
//! ```
//!
//! Or, on production hardware after cross-compile:
//!
//! ```bash
//! cargo zigbuild --release --bench pointwatch_e2e \
//!   --target aarch64-unknown-linux-musl -p aether-rtdb-shm
//! scp target/aarch64-unknown-linux-musl/release/deps/pointwatch_e2e-* \
//!   root@host:/tmp/pointwatch_e2e
//! ssh root@host '/tmp/pointwatch_e2e'
//! ```

#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_model::PointType;
use aether_rtdb_shm::{
    ChannelPointCounts, ChannelToSlotIndex, PointWatchListener, PointWatchSignaler,
    ReverseSlotIndex, SharedConfig, SubscriptionBitmap, UnifiedWriter,
    automation_bitmap_path_from_shm,
};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const CHANNEL_ID: u32 = 1001;
const POINT_ID: u32 = 0;
const TELEMETRY_POINTS: u32 = 1;
const WARMUP_EVENTS: usize = 500;
const MEASURE_EVENTS: usize = 5_000;
const INTER_EVENT_PACING_US: u64 = 100;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn percentile(sorted: &[u64], pct: f64) -> u64 {
    assert!(!sorted.is_empty());
    let idx = ((sorted.len() - 1) as f64 * pct / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn print_stats(label: &str, samples: &[u64]) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let total_ns: u128 = sorted.iter().map(|&v| v as u128).sum();
    let mean_us = (total_ns / sorted.len() as u128) as u64;
    println!("─── {} (n={}) ─────────────", label, sorted.len());
    println!("  min  = {} µs", sorted[0] / 1000);
    println!("  P50  = {} µs", percentile(&sorted, 50.0) / 1000);
    println!("  P95  = {} µs", percentile(&sorted, 95.0) / 1000);
    println!("  P99  = {} µs", percentile(&sorted, 99.0) / 1000);
    println!("  P99.9= {} µs", percentile(&sorted, 99.9) / 1000);
    println!("  max  = {} µs", sorted[sorted.len() - 1] / 1000);
    println!("  mean = {} µs", mean_us / 1000);
    println!();
}

fn main() {
    println!("PointWatch end-to-end latency bench");
    println!(
        "Warmup: {} events, measure: {} events",
        WARMUP_EVENTS, MEASURE_EVENTS
    );
    println!(
        "Inter-event pacing: {} µs (lets drain task batch reset)",
        INTER_EVENT_PACING_US
    );
    println!();

    let tmp = TempDir::new().expect("tempdir");
    let shm_path = tmp.path().join("aether-rtdb.shm");
    let uds_path_pb = tmp.path().join("pw.sock");
    let uds_path = uds_path_pb.to_str().expect("utf-8 uds path").to_string();
    let bitmap_path = automation_bitmap_path_from_shm(&shm_path);

    // ── Producer "process" setup (mirrors io/src/main.rs) ────────────────
    let config = SharedConfig::default()
        .with_path(shm_path.clone())
        .with_max_slots(64);
    let mut counts = BTreeMap::new();
    counts.insert(CHANNEL_ID, [TELEMETRY_POINTS, 0, 0, 0]);
    let channel_points = ChannelPointCounts::from_map(counts);

    let mut writer = UnifiedWriter::create(&config, &channel_points).expect("writer create");
    let index = ChannelToSlotIndex::from_unified_writer(&writer);
    let slot_count = writer.slot_count();
    let slot = index
        .lookup(CHANNEL_ID, PointType::Telemetry, POINT_ID)
        .expect("slot exists");

    let bitmap_producer =
        Arc::new(SubscriptionBitmap::create(&bitmap_path).expect("create bitmap"));
    // Producer enables the subscription bit BEFORE first write
    // (in production, automation writes it via mmap; here we set it ourselves).
    bitmap_producer.set_watched(slot);

    let reverse_index = Arc::new(ReverseSlotIndex::from_forward(&index, slot_count));

    let runtime = Runtime::new().expect("runtime");

    let measurements: Vec<u64> = runtime.block_on(async move {
        // Producer-side: signaler + drain task that writes events to UDS.
        let drain_shutdown = CancellationToken::new();
        let (signaler, drain_handle) = PointWatchSignaler::new_with_drain(
            Arc::clone(&bitmap_producer),
            reverse_index,
            uds_path.clone(),
            drain_shutdown.clone(),
        );
        writer.set_point_watcher(Some(Arc::clone(&signaler)));

        // Consumer-side: PointWatchListener UDS server that delivers events
        // to our event_rx via its own mpsc channel.
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (listener, mut event_rx) = PointWatchListener::new(Some(&uds_path), shutdown_rx);
        let _listener_handle = tokio::spawn(async move {
            let _ = listener.run().await;
        });

        // Wait for listener to bind and drain task to connect.
        // The drain task uses exponential backoff (1-5s). 500ms is enough
        // for the first connect attempt to succeed on a quiet system.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Warmup: drain stale events, prime the path.
        for i in 0..WARMUP_EVENTS {
            writer.set_direct(slot, i as f64, i as f64, now_ms());
            // bound the warmup wait so we don't hang if drops happen
            let _ = tokio::time::timeout(Duration::from_millis(50), event_rx.recv()).await;
            tokio::time::sleep(Duration::from_micros(INTER_EVENT_PACING_US)).await;
        }

        // Drain anything queued during warmup.
        while tokio::time::timeout(Duration::from_millis(5), event_rx.recv())
            .await
            .is_ok()
        {}

        // ── Measurement loop ────────────────────────────────────────────
        let mut latencies_ns = Vec::with_capacity(MEASURE_EVENTS);
        let mut drops = 0usize;

        for i in 0..MEASURE_EVENTS {
            let start = Instant::now();
            writer.set_direct(slot, i as f64 + 1000.0, i as f64, now_ms());

            // Bounded wait so a single drop doesn't deadlock the whole bench.
            match tokio::time::timeout(Duration::from_millis(50), event_rx.recv()).await {
                Ok(Some(_ev)) => {
                    latencies_ns.push(start.elapsed().as_nanos() as u64);
                },
                Ok(None) => {
                    eprintln!("event_rx closed early at iter {}", i);
                    break;
                },
                Err(_) => {
                    drops += 1;
                },
            }

            tokio::time::sleep(Duration::from_micros(INTER_EVENT_PACING_US)).await;
        }

        println!(
            "Measurement complete: {} samples, {} drops",
            latencies_ns.len(),
            drops
        );
        println!();

        // Shutdown.
        let _ = shutdown_tx.send(true);
        drain_shutdown.cancel();
        let _ = drain_handle.await;

        latencies_ns
    });

    if measurements.is_empty() {
        eprintln!("No samples collected — bench failed");
        std::process::exit(1);
    }
    print_stats(
        "PointWatch end-to-end (set_direct → event_rx.recv)",
        &measurements,
    );
}
