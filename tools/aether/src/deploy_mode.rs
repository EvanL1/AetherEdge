//! Detects whether this host is running AetherEMS via Docker Compose or
//! systemd, so `aether services`/`aether doctor` can speak the right
//! backend without a user-facing flag. See
//! docs/superpowers/specs/2026-07-10-baremetal-install-design.md.

use anyhow::{Result, bail};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployMode {
    Docker,
    Systemd,
}

impl DeployMode {
    /// Systemd mode requires both: the unit this installer creates exists,
    /// and `systemctl` itself is on PATH (belt-and-braces — the unit file
    /// existing without a working systemctl would be a broken half-install,
    /// and falling back to Docker semantics in that case is safer than
    /// erroring, since the Docker path degrades gracefully with its own
    /// "docker-compose.yml not found" error).
    pub(crate) fn detect(installed_mode: Option<&str>) -> Result<Self> {
        match installed_mode {
            Some("systemd") => Ok(DeployMode::Systemd),
            Some("docker-compose" | "docker") => Ok(DeployMode::Docker),
            Some(other) => bail!("unsupported install context mode: {other}"),
            None => Ok(Self::detect_with(
                Path::new("/etc/systemd/system/aether.target"),
                which_systemctl(),
            )),
        }
    }

    fn detect_with(unit_path: &Path, systemctl_available: bool) -> Self {
        if unit_path.exists() && systemctl_available {
            DeployMode::Systemd
        } else {
            DeployMode::Docker
        }
    }
}

fn which_systemctl() -> bool {
    std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_docker_when_unit_file_absent() {
        let mode = DeployMode::detect_with(Path::new("/nonexistent/aether.target"), true);
        assert_eq!(mode, DeployMode::Docker);
    }

    #[test]
    fn detects_docker_when_systemctl_unavailable_even_if_unit_present() {
        // Use this crate's own Cargo.toml as a stand-in "file that exists"
        // so the test doesn't depend on writing to /etc.
        let existing_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let mode = DeployMode::detect_with(&existing_file, false);
        assert_eq!(mode, DeployMode::Docker);
    }

    #[test]
    fn detects_systemd_when_both_present() {
        let existing_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let mode = DeployMode::detect_with(&existing_file, true);
        assert_eq!(mode, DeployMode::Systemd);
    }

    #[test]
    fn install_context_mode_overrides_host_heuristics() {
        assert_eq!(
            DeployMode::detect(Some("docker-compose")).unwrap(),
            DeployMode::Docker
        );
        assert_eq!(
            DeployMode::detect(Some("systemd")).unwrap(),
            DeployMode::Systemd
        );
        assert!(DeployMode::detect(Some("unknown")).is_err());
    }
}
