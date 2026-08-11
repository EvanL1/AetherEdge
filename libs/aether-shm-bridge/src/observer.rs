//! Read-only observability for the committed point and channel-health planes.

use std::path::{Path, PathBuf};

use aether_dataplane::{HeaderSnapshot, SlotIo, SlotReader};
use aether_ports::{PortError, PortErrorKind, PortResult};

use crate::topology_commit::read_topology_publication_commit;
use crate::{channel_health_path_from_shm, timestamp_ms};

const DEFAULT_HEALTHY_AFTER_MS: u64 = 3_000;
const DEFAULT_STALE_AFTER_MS: u64 = 30_000;

/// Aggregate health of one observed committed SHM topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmObservationStatus {
    /// Both planes are current and match their durable commit witness.
    Healthy,
    /// The topology is coherent but delayed, contended, or changing.
    Degraded,
    /// Authority, physical layout, commit identity, or writer liveness failed.
    Unhealthy,
}

impl ShmObservationStatus {
    /// Returns the stable lowercase representation used by operator surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// Severity of one observer finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmObservationSeverity {
    /// The observer completed but could not obtain a fully current snapshot.
    Degraded,
    /// The observed topology cannot be trusted as current SHM authority.
    Unhealthy,
}

impl ShmObservationSeverity {
    /// Returns the stable lowercase representation used by operator surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// One typed, stable-code diagnostic emitted by [`ShmObserver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmObservationFinding {
    /// Finding severity.
    pub severity: ShmObservationSeverity,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Human-readable evidence.
    pub message: String,
}

/// Aggregate slot contents observed in one best-effort read-only scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShmSlotObservation {
    /// Slots declared by the physical header.
    pub total: usize,
    /// Slots containing a value rather than the unwritten sentinel.
    pub present: usize,
    /// Slots still carrying the unwritten sentinel.
    pub unwritten: usize,
    /// Slots that changed during the observer's single seqlock attempt.
    pub contended: usize,
    /// Present slots with good quality.
    pub good: usize,
    /// Present slots with uncertain quality.
    pub uncertain: usize,
    /// Present slots with bad quality.
    pub bad: usize,
    /// Present slots with unavailable quality.
    pub unavailable: usize,
    /// Present slots with a quality code outside the current ABI.
    pub unknown_quality: usize,
    /// Online channel-health slots. Zero for a point-plane scan.
    pub online: usize,
    /// Offline channel-health slots. Zero for a point-plane scan.
    pub offline: usize,
    /// Present slots whose value/raw shape violates the plane contract.
    pub invalid_values: usize,
}

/// One immutable physical plane observed from a read-only mmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmPlaneObservation {
    /// Canonical path opened by the observer.
    pub path: PathBuf,
    /// Live mmap ABI version.
    pub version: u32,
    /// Exact physical slot count.
    pub slot_count: usize,
    /// Exact mapped file size.
    pub file_size: u64,
    /// Physical layout fingerprint.
    pub layout_hash: u64,
    /// Stable writer generation.
    pub writer_generation: u64,
    /// Cross-plane publication identity.
    pub publication_epoch: u64,
    /// Latest dedicated writer heartbeat.
    pub writer_heartbeat_ms: u64,
    /// Heartbeat age at observation time. Future heartbeats report zero here
    /// and emit a separate unhealthy finding.
    pub heartbeat_age_ms: u64,
    /// Optional aggregate scan. Disabled observers leave this as `None`.
    pub slots: Option<ShmSlotObservation>,
}

/// Complete read-only observation of the committed point/health topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmTopologyObservation {
    /// Observer wall-clock timestamp.
    pub observed_at_ms: u64,
    /// Highest finding severity.
    pub status: ShmObservationStatus,
    /// Point-plane observation when its physical file could be opened.
    pub point: Option<ShmPlaneObservation>,
    /// Channel-health-plane observation when its physical file could be opened.
    pub health: Option<ShmPlaneObservation>,
    /// Durable epoch when a readable commit witness was present.
    pub publication_epoch: Option<u64>,
    /// Stable-code diagnostics explaining every non-healthy result.
    pub findings: Vec<ShmObservationFinding>,
}

impl ShmTopologyObservation {
    fn empty(observed_at_ms: u64) -> Self {
        Self {
            observed_at_ms,
            status: ShmObservationStatus::Healthy,
            point: None,
            health: None,
            publication_epoch: None,
            findings: Vec::new(),
        }
    }

    fn finding(
        &mut self,
        severity: ShmObservationSeverity,
        code: &'static str,
        message: impl Into<String>,
    ) {
        self.status = match (self.status, severity) {
            (_, ShmObservationSeverity::Unhealthy) => ShmObservationStatus::Unhealthy,
            (ShmObservationStatus::Healthy, ShmObservationSeverity::Degraded) => {
                ShmObservationStatus::Degraded
            },
            (status, ShmObservationSeverity::Degraded) => status,
        };
        self.findings.push(ShmObservationFinding {
            severity,
            code,
            message: message.into(),
        });
    }
}

/// Direct, non-repairing observer for one canonical point plane and its
/// derived channel-health plane.
#[derive(Debug, Clone)]
pub struct ShmObserver {
    point_path: PathBuf,
    health_path: PathBuf,
    healthy_after_ms: u64,
    stale_after_ms: u64,
    scan_slots: bool,
}

impl ShmObserver {
    /// Creates an observer using the canonical derived health path and the
    /// default 3-second healthy / 30-second stale thresholds.
    #[must_use]
    pub fn new(point_path: impl Into<PathBuf>) -> Self {
        let point_path = point_path.into();
        let health_path = channel_health_path_from_shm(&point_path);
        Self {
            point_path,
            health_path,
            healthy_after_ms: DEFAULT_HEALTHY_AFTER_MS,
            stale_after_ms: DEFAULT_STALE_AFTER_MS,
            scan_slots: true,
        }
    }

    /// Selects a non-derived health-plane path.
    #[must_use]
    pub fn with_health_path(mut self, health_path: impl Into<PathBuf>) -> Self {
        self.health_path = health_path.into();
        self
    }

    /// Sets the healthy and terminal stale heartbeat thresholds.
    pub fn with_liveness_thresholds(
        mut self,
        healthy_after_ms: u64,
        stale_after_ms: u64,
    ) -> PortResult<Self> {
        if stale_after_ms == 0 || healthy_after_ms >= stale_after_ms {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "SHM observer requires healthy_after_ms < non-zero stale_after_ms",
            ));
        }
        self.healthy_after_ms = healthy_after_ms;
        self.stale_after_ms = stale_after_ms;
        Ok(self)
    }

    /// Enables or disables the O(N) aggregate slot scan. Header and commit
    /// validation always remain enabled.
    #[must_use]
    pub const fn with_slot_scan(mut self, enabled: bool) -> Self {
        self.scan_slots = enabled;
        self
    }

    /// Observes both planes without acquiring write authority, refreshing a
    /// heartbeat, repairing a file, or publishing a generation.
    #[must_use]
    pub fn inspect(&self) -> ShmTopologyObservation {
        let observed_at_ms = timestamp_ms();
        let mut observation = ShmTopologyObservation::empty(observed_at_ms);
        let point = SlotReader::open(&self.point_path);
        let health = SlotReader::open(&self.health_path);

        let point_reader = match point.as_ref() {
            Ok(reader) => Some(reader),
            Err(error) => {
                observation.finding(
                    ShmObservationSeverity::Unhealthy,
                    "point_plane_unavailable",
                    format!("cannot inspect {}: {error}", self.point_path.display()),
                );
                None
            },
        };
        let health_reader = match health.as_ref() {
            Ok(reader) => Some(reader),
            Err(error) => {
                observation.finding(
                    ShmObservationSeverity::Unhealthy,
                    "health_plane_unavailable",
                    format!("cannot inspect {}: {error}", self.health_path.display()),
                );
                None
            },
        };

        if let Some(reader) = point_reader {
            observation.point = Some(self.observe_plane(
                "point",
                &self.point_path,
                reader,
                observed_at_ms,
                false,
                &mut observation.findings,
            ));
        }
        if let Some(reader) = health_reader {
            observation.health = Some(self.observe_plane(
                "health",
                &self.health_path,
                reader,
                observed_at_ms,
                true,
                &mut observation.findings,
            ));
        }
        observation.status = status_from_findings(&observation.findings);

        let (Some(point_reader), Some(health_reader)) = (point_reader, health_reader) else {
            return observation;
        };
        let point_identity_before = point_reader.header();
        let health_identity_before = health_reader.header();
        let commit_before = match read_topology_publication_commit(&self.point_path) {
            Ok(commit) => commit,
            Err(error) => {
                observation.finding(
                    ShmObservationSeverity::Unhealthy,
                    "topology_commit_unavailable",
                    error.to_string(),
                );
                return observation;
            },
        };
        observation.publication_epoch = Some(commit_before.publication_epoch());

        let point_identity_after = point_reader.header();
        let health_identity_after = health_reader.header();
        let commit_after = match read_topology_publication_commit(&self.point_path) {
            Ok(commit) => commit,
            Err(error) => {
                observation.finding(
                    ShmObservationSeverity::Degraded,
                    "topology_publication_changed",
                    format!("commit witness changed during observation: {error}"),
                );
                return observation;
            },
        };
        if !same_plane_identity(point_identity_before, point_identity_after)
            || !same_plane_identity(health_identity_before, health_identity_after)
            || commit_before != commit_after
        {
            observation.finding(
                ShmObservationSeverity::Degraded,
                "topology_publication_changed",
                "SHM topology publication changed during observation",
            );
        } else if !commit_after.matches_readers(point_reader, health_reader) {
            observation.finding(
                ShmObservationSeverity::Unhealthy,
                "topology_commit_mismatch",
                "point and health planes do not match the durable commit witness",
            );
        }
        observation
    }

    fn observe_plane(
        &self,
        label: &'static str,
        path: &Path,
        reader: &SlotReader,
        observed_at_ms: u64,
        health_plane: bool,
        findings: &mut Vec<ShmObservationFinding>,
    ) -> ShmPlaneObservation {
        let header = reader.header();
        let file_size = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
        let heartbeat_age_ms = observed_at_ms.saturating_sub(header.writer_heartbeat);
        if header.writer_generation == 0 || header.writer_generation & 1 != 0 {
            findings.push(unhealthy_finding(
                "writer_generation_invalid",
                format!(
                    "{label} writer generation {} is not stable",
                    header.writer_generation
                ),
            ));
        }
        if header.writer_heartbeat == 0 {
            findings.push(unhealthy_finding(
                "writer_heartbeat_missing",
                format!("{label} writer heartbeat is zero"),
            ));
        } else if header.writer_heartbeat > observed_at_ms {
            findings.push(unhealthy_finding(
                "writer_heartbeat_future",
                format!("{label} writer heartbeat is in the future"),
            ));
        } else if heartbeat_age_ms > self.stale_after_ms {
            findings.push(unhealthy_finding(
                "writer_heartbeat_stale",
                format!("{label} writer heartbeat is {heartbeat_age_ms}ms old"),
            ));
        } else if heartbeat_age_ms > self.healthy_after_ms {
            findings.push(degraded_finding(
                "writer_heartbeat_delayed",
                format!("{label} writer heartbeat is {heartbeat_age_ms}ms old"),
            ));
        }

        let slots = self
            .scan_slots
            .then(|| scan_slots(label, reader, health_plane, findings));
        ShmPlaneObservation {
            path: path.to_path_buf(),
            version: header.version,
            slot_count: reader.slot_count(),
            file_size,
            layout_hash: header.layout_hash,
            writer_generation: header.writer_generation,
            publication_epoch: header.publication_epoch,
            writer_heartbeat_ms: header.writer_heartbeat,
            heartbeat_age_ms,
            slots,
        }
    }
}

fn scan_slots(
    label: &str,
    reader: &SlotReader,
    health_plane: bool,
    findings: &mut Vec<ShmObservationFinding>,
) -> ShmSlotObservation {
    let mut observation = ShmSlotObservation {
        total: reader.slot_count(),
        ..ShmSlotObservation::default()
    };
    for slot in 0..reader.slot_count() {
        let Some(sample) = SlotIo::read_slot(reader, slot) else {
            observation.contended += 1;
            continue;
        };
        if sample.value.is_nan() && sample.raw.is_nan() {
            observation.unwritten += 1;
            continue;
        }
        observation.present += 1;
        match sample.quality_code {
            0 => observation.good += 1,
            1 => observation.uncertain += 1,
            2 => observation.bad += 1,
            3 => observation.unavailable += 1,
            _ => observation.unknown_quality += 1,
        }
        if !sample.value.is_finite() || !sample.raw.is_finite() {
            observation.invalid_values += 1;
        } else if health_plane {
            if sample.value == 1.0 && sample.raw == 1.0 {
                observation.online += 1;
            } else if sample.value == 0.0 && sample.raw == 0.0 {
                observation.offline += 1;
            } else {
                observation.invalid_values += 1;
            }
        }
    }
    if observation.contended != 0 {
        findings.push(degraded_finding(
            "slot_scan_contended",
            format!(
                "{label} scan could not read {} changing slots",
                observation.contended
            ),
        ));
    }
    if observation.unknown_quality != 0 {
        findings.push(unhealthy_finding(
            "slot_quality_invalid",
            format!(
                "{label} scan found {} unknown quality codes",
                observation.unknown_quality
            ),
        ));
    }
    if observation.invalid_values != 0 {
        findings.push(unhealthy_finding(
            "slot_value_invalid",
            format!(
                "{label} scan found {} invalid slot values",
                observation.invalid_values
            ),
        ));
    }
    observation
}

fn same_plane_identity(first: HeaderSnapshot, second: HeaderSnapshot) -> bool {
    first.magic == second.magic
        && first.version == second.version
        && first.slot_count == second.slot_count
        && first.layout_hash == second.layout_hash
        && first.writer_generation == second.writer_generation
        && first.publication_epoch == second.publication_epoch
}

fn status_from_findings(findings: &[ShmObservationFinding]) -> ShmObservationStatus {
    if findings
        .iter()
        .any(|finding| finding.severity == ShmObservationSeverity::Unhealthy)
    {
        ShmObservationStatus::Unhealthy
    } else if findings
        .iter()
        .any(|finding| finding.severity == ShmObservationSeverity::Degraded)
    {
        ShmObservationStatus::Degraded
    } else {
        ShmObservationStatus::Healthy
    }
}

fn degraded_finding(code: &'static str, message: String) -> ShmObservationFinding {
    ShmObservationFinding {
        severity: ShmObservationSeverity::Degraded,
        code,
        message,
    }
}

fn unhealthy_finding(code: &'static str, message: String) -> ShmObservationFinding {
    ShmObservationFinding {
        severity: ShmObservationSeverity::Unhealthy,
        code,
        message,
    }
}
