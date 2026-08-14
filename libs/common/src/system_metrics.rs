//! System metrics collection for health endpoints
//!
//! Provides CPU and memory usage information using the sysinfo crate.

use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};

const METRICS_CACHE_TTL: Duration = Duration::from_secs(5);
static PROCESS_METRICS: OnceLock<Mutex<MetricsSampler>> = OnceLock::new();

/// System resource metrics
#[derive(Debug, Clone, Serialize)]
pub struct SystemMetrics {
    /// Number of CPU cores
    pub cpu_count: usize,
    /// Current process CPU usage percentage (can exceed 100% on multi-core)
    pub process_cpu_percent: f32,
    /// Current process memory usage (MB)
    pub process_memory_mb: u64,
    /// Total system memory (MB)
    pub memory_total_mb: u64,
}

impl SystemMetrics {
    /// Collect current process metrics
    ///
    /// Returns CPU and memory usage for the current process.
    /// Note: `process_cpu_percent` can exceed 100% on multi-core systems
    /// (e.g., 200% means using 2 full cores).
    pub fn collect() -> Self {
        let sampler = PROCESS_METRICS.get_or_init(|| Mutex::new(MetricsSampler::new()));
        let mut sampler = sampler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sampler.collect()
    }
}

struct MetricsSampler {
    system: System,
    pid: Pid,
    sampled_at: Instant,
    cached: SystemMetrics,
}

impl MetricsSampler {
    fn new() -> Self {
        let mut sys = System::new();
        let pid = Pid::from_u32(std::process::id());
        let cached = refresh(&mut sys, pid);
        Self {
            system: sys,
            pid,
            sampled_at: Instant::now(),
            cached,
        }
    }

    fn collect(&mut self) -> SystemMetrics {
        if self.sampled_at.elapsed() >= METRICS_CACHE_TTL {
            self.cached = refresh(&mut self.system, self.pid);
            self.sampled_at = Instant::now();
        }
        self.cached.clone()
    }
}

fn refresh(system: &mut System, pid: Pid) -> SystemMetrics {
    system.refresh_memory();
    // A persistent System instance gives CPU usage a previous sample to compare
    // with; constructing one in every health request usually reported zero.
    system.refresh_cpu_usage();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::new().with_cpu().with_memory(),
    );

    let (process_cpu, process_mem) = system
        .process(pid)
        .map(|process| (process.cpu_usage(), process.memory() / 1024 / 1024))
        .unwrap_or((0.0, 0));

    let cpu_count = system.cpus().len();
    let memory_total = system.total_memory() / 1024 / 1024;

    SystemMetrics {
        cpu_count,
        process_cpu_percent: process_cpu,
        process_memory_mb: process_mem,
        memory_total_mb: memory_total,
    }
}

impl Default for SystemMetrics {
    fn default() -> Self {
        Self::collect()
    }
}
