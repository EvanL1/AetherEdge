//! System metrics collection for health endpoints
//!
//! Provides CPU and memory usage information using the sysinfo crate.

use serde::Serialize;
use sysinfo::{Pid, System};

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
        let mut sys = System::new();
        sys.refresh_memory();
        sys.refresh_cpu_usage(); // Required for cpus() to return non-empty list

        let pid = Pid::from_u32(std::process::id());
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::new().with_cpu().with_memory(),
        );

        let (process_cpu, process_mem) = sys
            .process(pid)
            .map(|p| (p.cpu_usage(), p.memory() / 1024 / 1024))
            .unwrap_or((0.0, 0));

        let cpu_count = sys.cpus().len();
        let memory_total = sys.total_memory() / 1024 / 1024; // bytes -> MB

        Self {
            cpu_count,
            process_cpu_percent: process_cpu,
            process_memory_mb: process_mem,
            memory_total_mb: memory_total,
        }
    }
}

impl Default for SystemMetrics {
    fn default() -> Self {
        Self::collect()
    }
}
