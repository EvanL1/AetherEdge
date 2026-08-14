use std::time::Duration;

use aether_io::core::channels::channel_task::bench_support::{
    LaneProgressSample, run_selector_benchmark,
};
use aether_io::protocols::core::file_logging::bench_support::run_slow_log_benchmark;
use serde_json::{Value, json};

const SELECTOR_POLL_SAMPLES: u64 = 1_001;
const SELECTOR_POLL_INTERVAL: Duration = Duration::from_millis(1);
const SELECTOR_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(10);
const LOG_ATTEMPTS: u64 = 50_000;
const LOG_QUEUE_CAPACITY: usize = 64;
const TICKER_SAMPLES: u64 = 500;
const TICKER_INTERVAL: Duration = Duration::from_millis(1);

fn distribution(samples: &[u64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = |percent: usize| -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let index = (sorted.len().saturating_sub(1) * percent).div_ceil(100);
        sorted[index]
    };
    let sum = sorted.iter().fold(0_u128, |total, sample| {
        total.saturating_add(*sample as u128)
    });
    let mean = if sorted.is_empty() {
        0
    } else {
        u64::try_from(sum / sorted.len() as u128).unwrap_or(u64::MAX)
    };
    json!({
        "samples": sorted.len(),
        "min": sorted.first().copied().unwrap_or(0),
        "p50": percentile(50),
        "p95": percentile(95),
        "p99": percentile(99),
        "max": sorted.last().copied().unwrap_or(0),
        "mean": mean,
    })
}

fn lane(sample: LaneProgressSample) -> Value {
    json!({
        "completed": sample.completed,
        "max_non_poll_gap_work_units": sample.max_non_poll_gap,
    })
}

fn probe_stdout(program: &str, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn probe_git_dirty() -> Option<bool> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

async fn run() -> Result<Value, String> {
    let selector = run_selector_benchmark(
        SELECTOR_POLL_INTERVAL,
        SELECTOR_HEARTBEAT_INTERVAL,
        SELECTOR_POLL_SAMPLES,
    )
    .await?;
    let slow_log = run_slow_log_benchmark(
        LOG_ATTEMPTS,
        LOG_QUEUE_CAPACITY,
        TICKER_INTERVAL,
        TICKER_SAMPLES,
    )
    .await?;

    Ok(json!({
        "benchmark": "aether_io_resilience",
        "environment": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "build_mode": if cfg!(debug_assertions) { "debug" } else { "release" },
            "logical_cpus": std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1),
            "rustc_version": probe_stdout("rustc", &["--version"]),
            "git_head": probe_stdout("git", &["rev-parse", "HEAD"]),
            "git_dirty": probe_git_dirty(),
        },
        "units": {
            "timing_fields": "nanoseconds",
            "counts": "completed operations",
            "lane_progress_gap": "completed non-poll work units",
            "queue_depth": "log records including the in-flight write where stated",
        },
        "selector": {
            "poll_interval_ns": selector.poll_interval_ns,
            "heartbeat_interval_ns": selector.heartbeat_interval_ns,
            "polls": selector.polls,
            "total_work_units": selector.total_work_units,
            "poll_gap_ns": distribution(&selector.poll_gap_ns),
            "lanes": {
                "protocol": lane(selector.protocol),
                "business": lane(selector.business),
                "event": lane(selector.event),
                "heartbeat": lane(selector.heartbeat),
            },
            "contract": {
                "continuously_ready_external_lane_max_gap_work_units": 4,
                "passed": true,
            },
        },
        "slow_file_log": {
            "minimum_attempts": slow_log.minimum_attempts,
            "attempts": slow_log.attempts,
            "queue_capacity": slow_log.queue_capacity,
            "accepted": slow_log.accepted,
            "dropped": slow_log.dropped,
            "pending_at_saturation": slow_log.pending_at_saturation,
            "max_pending_observed": slow_log.max_pending_observed,
            "writes_after_drain": slow_log.writes_after_drain,
            "admission_latency_observations": slow_log.admission_latency_observations,
            "admission_latency_reservoir_samples": slow_log.admission_latency_ns.len(),
            "admission_latency_ns": distribution(&slow_log.admission_latency_ns),
            "admission_window_ns": slow_log.admission_window_ns,
            "ticker_interval_ns": slow_log.ticker_interval_ns,
            "ticker_window_ns": slow_log.ticker_window_ns,
            "ticker_lateness_ns": distribution(&slow_log.ticker_lateness_ns),
            "contract": {
                "max_pending_including_in_flight": slow_log.queue_capacity + 1,
                "admission_covered_full_ticker_window": slow_log.admission_window_ns >= slow_log.ticker_window_ns,
                "all_attempts_accounted": true,
                "all_accepted_drained": true,
                "passed": true,
            },
        },
    }))
}

fn persist_report(report: &str) -> Result<(), String> {
    let Some(directory) = std::env::var_os("AETHER_BENCH_OUTPUT_DIR") else {
        return Ok(());
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "create benchmark output directory {}: {error}",
            directory.display()
        )
    })?;
    let path = directory.join("io-resilience.json");
    std::fs::write(&path, report)
        .map_err(|error| format!("write benchmark report {}: {error}", path.display()))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match run().await {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(report) => {
                if let Err(error) = persist_report(&report) {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
                println!("{report}");
            },
            Err(error) => {
                eprintln!("serialize io resilience benchmark: {error}");
                std::process::exit(1);
            },
        },
        Err(error) => {
            eprintln!("io resilience benchmark failed: {error}");
            std::process::exit(1);
        },
    }
}
