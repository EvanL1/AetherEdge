//! Capacity and resident-memory benchmark for the bounded DataEvent ingress.
//!
//! Run with:
//!
//! ```text
//! cargo bench -p aether-io --bench capacity_rss
//! ```
//!
//! `AETHER_BENCH_POINT_CAPACITY` changes the number of unique pending points
//! (default: 100,000). `AETHER_BENCH_OUTPUT_DIR` writes the same JSON emitted
//! on stdout to `<dir>/capacity-rss.json`.

use std::env;
use std::fs;
use std::hint::black_box;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use aether_io::protocols::core::data_event_ingress::data_event_channel_with_capacity;
use aether_io::protocols::core::{DataBatch, DataEvent, DataEventAdmission, DataPoint};
use serde::Serialize;

const DEFAULT_POINT_CAPACITY: usize = 100_000;
const DEFAULT_BATCH_SIZE: usize = 256;
const MAX_BENCHMARK_POINTS: usize = 2_000_000;

#[derive(Serialize)]
struct CapacityRssReport {
    benchmark: &'static str,
    environment: Environment,
    configured_point_capacity: usize,
    batch_size: usize,
    fill_batches: u64,
    fill_elapsed_ns: u128,
    fill_points_per_second: f64,
    coalesce_elapsed_ns: u128,
    coalesce_points_per_second: f64,
    drain_elapsed_ns: u128,
    rss_before_bytes: u64,
    rss_filled_bytes: u64,
    rss_after_coalesce_bytes: u64,
    rss_after_drain_bytes: u64,
    rss_fill_delta_bytes: i128,
    rss_bytes_per_pending_point: f64,
    ingress: IngressReport,
}

#[derive(Serialize)]
struct Environment {
    os: &'static str,
    arch: &'static str,
    build_mode: &'static str,
    pid: u32,
    logical_cpus: usize,
    rustc_version: Option<String>,
    git_head: Option<String>,
    git_dirty: Option<bool>,
}

#[derive(Serialize)]
struct IngressReport {
    accepted_events: u64,
    coalesced_events: u64,
    dropped_full: u64,
    dropped_closed: u64,
    dropped_contended: u64,
    oversized: u64,
    high_watermark: u64,
    pending_after_fill: u64,
    pending_after_drain: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let point_capacity = bounded_env_usize(
        "AETHER_BENCH_POINT_CAPACITY",
        DEFAULT_POINT_CAPACITY,
        1,
        MAX_BENCHMARK_POINTS,
    )?;
    let batch_size = bounded_env_usize(
        "AETHER_BENCH_BATCH_SIZE",
        DEFAULT_BATCH_SIZE.min(point_capacity),
        1,
        point_capacity,
    )?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(run(point_capacity, batch_size))?;
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    write_report("capacity-rss.json", &json)?;
    Ok(())
}

async fn run(
    point_capacity: usize,
    batch_size: usize,
) -> Result<CapacityRssReport, Box<dyn std::error::Error>> {
    let rss_before = current_rss_bytes()?;
    let (sink, mut receiver) = data_event_channel_with_capacity(point_capacity);

    let fill_started = Instant::now();
    let mut fill_batches = 0_u64;
    for start in (0..point_capacity).step_by(batch_size) {
        let end = start.saturating_add(batch_size).min(point_capacity);
        let points = (start..end)
            .map(|id| DataPoint::telemetry(u32::try_from(id).unwrap_or(u32::MAX), id as f64))
            .collect();
        let admission = sink.publish(DataEvent::DataUpdate(DataBatch::from_points(points)));
        if admission != DataEventAdmission::Accepted {
            return Err(
                format!("unexpected fill admission at point {start}: {admission:?}").into(),
            );
        }
        fill_batches = fill_batches.saturating_add(1);
    }
    let fill_elapsed = fill_started.elapsed();
    let filled_stats = sink.stats();
    if filled_stats.data_pending != point_capacity as u64
        || filled_stats.dropped_full != 0
        || filled_stats.dropped_contended != 0
        || filled_stats.oversized != 0
    {
        return Err(format!("fill violated bounded-ingress invariants: {filled_stats:?}").into());
    }
    let rss_filled = current_rss_bytes()?;

    // Update every key once. The pending cardinality and high-water mark must
    // remain fixed while each event is classified as coalesced.
    let coalesce_started = Instant::now();
    for start in (0..point_capacity).step_by(batch_size) {
        let end = start.saturating_add(batch_size).min(point_capacity);
        let points = (start..end)
            .map(|id| {
                DataPoint::telemetry(u32::try_from(id).unwrap_or(u32::MAX), (id as f64) + 1.0)
            })
            .collect();
        let admission = sink.publish(DataEvent::DataUpdate(DataBatch::from_points(points)));
        if admission != DataEventAdmission::Coalesced {
            return Err(
                format!("unexpected coalesce admission at point {start}: {admission:?}").into(),
            );
        }
    }
    let coalesce_elapsed = coalesce_started.elapsed();
    let coalesced_stats = sink.stats();
    if coalesced_stats.data_pending != point_capacity as u64
        || coalesced_stats.high_watermark != filled_stats.high_watermark
    {
        return Err(format!("coalescing changed ingress cardinality: {coalesced_stats:?}").into());
    }
    let rss_after_coalesce = current_rss_bytes()?;

    let drain_started = Instant::now();
    while sink.stats().data_pending > 0 {
        black_box(receiver.recv().await.ok_or("ingress closed before drain")?);
    }
    let drain_elapsed = drain_started.elapsed();
    let drained_stats = sink.stats();
    if drained_stats.pending != 0 {
        return Err(format!("ingress did not drain completely: {drained_stats:?}").into());
    }
    let rss_after_drain = current_rss_bytes()?;

    let rss_fill_delta = i128::from(rss_filled) - i128::from(rss_before);
    Ok(CapacityRssReport {
        benchmark: "io.data_event.capacity_rss",
        environment: Environment {
            os: env::consts::OS,
            arch: env::consts::ARCH,
            build_mode: if cfg!(debug_assertions) {
                "debug"
            } else {
                "optimized"
            },
            pid: std::process::id(),
            logical_cpus: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1),
            rustc_version: command_stdout("rustc", &["--version"]),
            git_head: command_stdout("git", &["rev-parse", "HEAD"]),
            git_dirty: command_stdout("git", &["status", "--porcelain"])
                .map(|status| !status.is_empty()),
        },
        configured_point_capacity: point_capacity,
        batch_size,
        fill_batches,
        fill_elapsed_ns: fill_elapsed.as_nanos(),
        fill_points_per_second: rate(point_capacity, fill_elapsed.as_nanos()),
        coalesce_elapsed_ns: coalesce_elapsed.as_nanos(),
        coalesce_points_per_second: rate(point_capacity, coalesce_elapsed.as_nanos()),
        drain_elapsed_ns: drain_elapsed.as_nanos(),
        rss_before_bytes: rss_before,
        rss_filled_bytes: rss_filled,
        rss_after_coalesce_bytes: rss_after_coalesce,
        rss_after_drain_bytes: rss_after_drain,
        rss_fill_delta_bytes: rss_fill_delta,
        rss_bytes_per_pending_point: (rss_fill_delta.max(0) as f64) / point_capacity as f64,
        ingress: IngressReport {
            accepted_events: coalesced_stats.accepted,
            coalesced_events: coalesced_stats.coalesced,
            dropped_full: coalesced_stats.dropped_full,
            dropped_closed: coalesced_stats.dropped_closed,
            dropped_contended: coalesced_stats.dropped_contended,
            oversized: coalesced_stats.oversized,
            high_watermark: coalesced_stats.high_watermark,
            pending_after_fill: coalesced_stats.data_pending,
            pending_after_drain: drained_stats.pending,
        },
    })
}

fn rate(items: usize, elapsed_ns: u128) -> f64 {
    if elapsed_ns == 0 {
        return 0.0;
    }
    (items as f64) * 1_000_000_000.0 / elapsed_ns as f64
}

fn bounded_env_usize(
    key: &'static str,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    let value = match env::var(key) {
        Ok(raw) => raw
            .parse::<usize>()
            .map_err(|error| format!("{key} must be an integer: {error}"))?,
        Err(env::VarError::NotPresent) => default,
        Err(error) => return Err(format!("cannot read {key}: {error}").into()),
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("{key} must be between {minimum} and {maximum}, got {value}").into());
    }
    Ok(value)
}

fn current_rss_bytes() -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status")?;
        if let Some(kib) = status.lines().find_map(|line| {
            line.strip_prefix("VmRSS:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
        }) {
            return kib
                .checked_mul(1_024)
                .ok_or_else(|| io::Error::other("RSS byte count overflow"));
        }
    }

    // POSIX `ps` reports RSS in KiB on both supported Linux and macOS hosts.
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ps failed while reading RSS: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let kib = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .trim()
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    kib.checked_mul(1_024)
        .ok_or_else(|| io::Error::other("RSS byte count overflow"))
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
}

fn write_report(file_name: &str, json: &str) -> io::Result<()> {
    let Some(output_dir) = env::var_os("AETHER_BENCH_OUTPUT_DIR") else {
        return Ok(());
    };
    let output_dir = PathBuf::from(output_dir);
    fs::create_dir_all(&output_dir)?;
    fs::write(output_dir.join(file_name), json)
}
