/// Collect system metrics using the `sysinfo` crate.
use sysinfo::{Disks, Networks, System};
use tracing::warn;

use crate::models::SystemMetrics;

pub fn collect() -> SystemMetrics {
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpu = sys.global_cpu_usage();

    let mem_total = sys.total_memory();
    let mem_used = sys.used_memory();
    let mem_avail = sys.available_memory();
    let mem_pct = if mem_total > 0 {
        mem_used as f64 / mem_total as f64 * 100.0
    } else {
        0.0
    };

    let (disk_total, disk_used, disk_free, disk_pct) = collect_disk();
    let (net_sent, net_recv) = collect_network();

    let uptime_hours = System::uptime() as f64 / 3600.0;

    SystemMetrics {
        cpu_usage_percent: cpu,
        memory_total_gb: mem_total as f64 / 1024.0 / 1024.0 / 1024.0,
        memory_used_gb: mem_used as f64 / 1024.0 / 1024.0 / 1024.0,
        memory_available_gb: mem_avail as f64 / 1024.0 / 1024.0 / 1024.0,
        memory_usage_percent: mem_pct,
        disk_total_gb: disk_total,
        disk_used_gb: disk_used,
        disk_free_gb: disk_free,
        disk_usage_percent: disk_pct,
        network_bytes_sent: net_sent,
        network_bytes_recv: net_recv,
        system_uptime_hours: uptime_hours,
    }
}

fn collect_disk() -> (f64, f64, f64, f64) {
    let disks = Disks::new_with_refreshed_list();
    // Aggregate across all disks
    let (total, avail) = disks.iter().fold((0u64, 0u64), |(t, a), d| {
        (t + d.total_space(), a + d.available_space())
    });
    if total == 0 {
        warn!("No disk information available");
        return (0.0, 0.0, 0.0, 0.0);
    }
    let used = total - avail;
    let gb = |v: u64| v as f64 / 1024.0 / 1024.0 / 1024.0;
    let pct = used as f64 / total as f64 * 100.0;
    (gb(total), gb(used), gb(avail), pct)
}

fn collect_network() -> (u64, u64) {
    let mut net = Networks::new_with_refreshed_list();
    net.refresh();
    net.iter().fold((0u64, 0u64), |(s, r), (_, d)| {
        (s + d.total_transmitted(), r + d.total_received())
    })
}
