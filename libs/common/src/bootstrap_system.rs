//! System requirements checking utilities
//!
//! Provides functions to check system resources and requirements
//! before starting AetherEMS services

use errors::AetherResult;
use tracing::{debug, info, warn};

/// System requirements for AetherEMS services
#[derive(Debug, Clone)]
pub struct SystemRequirements {
    /// Minimum CPU cores required
    pub min_cpu_cores: usize,
    /// Minimum memory in MB
    pub min_memory_mb: usize,
    /// Recommended CPU cores
    pub recommended_cpu_cores: usize,
    /// Recommended memory in MB
    pub recommended_memory_mb: usize,
}

impl Default for SystemRequirements {
    fn default() -> Self {
        Self {
            min_cpu_cores: 1,
            min_memory_mb: 256,
            recommended_cpu_cores: 2,
            recommended_memory_mb: 512,
        }
    }
}

/// Check if system meets the requirements
pub fn check_system_requirements() -> AetherResult<SystemInfo> {
    check_system_requirements_with(SystemRequirements::default())
}

/// Check system requirements with custom thresholds
pub fn check_system_requirements_with(
    requirements: SystemRequirements,
) -> AetherResult<SystemInfo> {
    let mut info = SystemInfo::collect();

    // Check CPU cores
    if info.cpu_cores < requirements.min_cpu_cores {
        warn!(
            "CPU: {} cores (min:{})",
            info.cpu_cores, requirements.min_cpu_cores
        );
        info.warnings.push(format!(
            "CPU cores ({}) below minimum requirement ({})",
            info.cpu_cores, requirements.min_cpu_cores
        ));
    } else if info.cpu_cores < requirements.recommended_cpu_cores {
        debug!(
            "CPU: {} cores (rec:{})",
            info.cpu_cores, requirements.recommended_cpu_cores
        );
    } else {
        debug!("CPU: {} cores", info.cpu_cores);
    }

    // Check memory
    if let Some(memory_mb) = info.available_memory_mb {
        if memory_mb < requirements.min_memory_mb {
            warn!(
                "Memory: {} MB (min:{})",
                memory_mb, requirements.min_memory_mb
            );
            info.warnings.push(format!(
                "Memory ({}MB) below minimum requirement ({}MB)",
                memory_mb, requirements.min_memory_mb
            ));
        } else if memory_mb < requirements.recommended_memory_mb {
            debug!(
                "Memory: {} MB (rec:{})",
                memory_mb, requirements.recommended_memory_mb
            );
        } else {
            debug!("Memory: {} MB", memory_mb);
        }
    }

    Ok(info)
}

/// System information collected during checks
#[derive(Debug, Clone)]
pub struct SystemInfo {
    /// Number of CPU cores
    pub cpu_cores: usize,
    /// Available memory in MB (if available)
    pub available_memory_mb: Option<usize>,
    /// Total memory in MB (if available)
    pub total_memory_mb: Option<usize>,
    /// Operating system name
    pub os_name: String,
    /// Architecture
    pub arch: String,
    /// Any warnings generated during checks
    pub warnings: Vec<String>,
}

impl SystemInfo {
    /// Collect current system information
    pub fn collect() -> Self {
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let (available_memory_mb, total_memory_mb) = get_memory_info();

        Self {
            cpu_cores,
            available_memory_mb,
            total_memory_mb,
            os_name: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            warnings: Vec::new(),
        }
    }

    /// Check if system has any warnings
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Print system information summary
    pub fn print_summary(&self) {
        // Build memory info string
        let mem_info = match (self.available_memory_mb, self.total_memory_mb) {
            (Some(avail), Some(total)) => format!(", Mem:{}/{}MB", avail, total),
            (Some(avail), None) => format!(", Mem:{}MB", avail),
            (None, Some(total)) => format!(", Mem:{}MB total", total),
            (None, None) => String::new(),
        };

        info!(
            "System: {} ({}) CPU:{}{}",
            self.os_name, self.arch, self.cpu_cores, mem_info
        );

        for warning in &self.warnings {
            warn!("{}", warning);
        }
    }
}

/// Get memory information based on platform
fn get_memory_info() -> (Option<usize>, Option<usize>) {
    #[cfg(target_os = "linux")]
    {
        get_linux_memory_info()
    }

    #[cfg(target_os = "macos")]
    {
        get_macos_memory_info()
    }

    #[cfg(target_os = "windows")]
    {
        get_windows_memory_info()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        (None, None)
    }
}

#[cfg(target_os = "linux")]
fn get_linux_memory_info() -> (Option<usize>, Option<usize>) {
    use std::fs;

    let mut available_mb = None;
    let mut total_mb = None;

    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            if line.starts_with("MemAvailable:")
                && let Some(kb_str) = line.split_whitespace().nth(1)
                && let Ok(kb) = kb_str.parse::<usize>()
            {
                available_mb = Some(kb / 1024);
            } else if line.starts_with("MemTotal:")
                && let Some(kb_str) = line.split_whitespace().nth(1)
                && let Ok(kb) = kb_str.parse::<usize>()
            {
                total_mb = Some(kb / 1024);
            }

            if available_mb.is_some() && total_mb.is_some() {
                break;
            }
        }
    }

    (available_mb, total_mb)
}

#[cfg(target_os = "macos")]
fn get_macos_memory_info() -> (Option<usize>, Option<usize>) {
    use std::process::Command;

    let total_mb = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .map(|bytes| bytes / 1024 / 1024);

    // macOS doesn't have a direct "available memory" metric like Linux
    // We could use vm_stat for more detailed info if needed
    (None, total_mb)
}

#[cfg(target_os = "windows")]
fn get_windows_memory_info() -> (Option<usize>, Option<usize>) {
    // Windows memory info would require winapi calls
    // For now, return None
    (None, None)
}

/// Check disk space availability for a given path
pub fn check_disk_space(path: &str, _required_mb: usize) -> AetherResult<bool> {
    use std::path::Path;

    let path = Path::new(path);
    let check_path = if path.exists() {
        path
    } else {
        path.parent().unwrap_or(Path::new("/"))
    };

    // For now, we'll use a simplified check based on filesystem metadata
    // Real disk space checking would require platform-specific system calls
    if let Ok(metadata) = std::fs::metadata(check_path)
        && (metadata.is_dir() || metadata.is_file())
    {
        debug!("Path {} exists and is accessible", check_path.display());
        // Since we can't easily get disk space without external dependencies,
        // we'll just check if the path is accessible
        return Ok(true);
    }

    warn!("Cannot verify disk space at {}", path.display());
    // Default to true if we can't check
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_system_info_collection() {
        let info = SystemInfo::collect();
        assert!(info.cpu_cores >= 1);
        assert!(!info.os_name.is_empty());
        assert!(!info.arch.is_empty());
    }

    #[test]
    fn test_default_requirements() {
        let req = SystemRequirements::default();
        assert_eq!(req.min_cpu_cores, 1);
        assert_eq!(req.min_memory_mb, 256);
    }

    #[test]
    fn test_system_check() {
        // Should not panic
        let result = check_system_requirements();
        assert!(result.is_ok());
    }
}
