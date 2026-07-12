//! Acquisition-owned writes into the authoritative SHM data plane.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, RwLock};

use aether_dataplane::{DataplaneError, SlotIo, SlotWriter};
use aether_domain::AcquiredPointSample;
use aether_ports::{AcquisitionStateWriter, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;

use crate::{ChannelPointManifest, PhysicalPointAddress};

/// Post-commit notification for acquisition writes.
///
/// Observers run only after the seqlock write completes and cannot mutate SHM.
/// The production PointWatch publisher implements this as a bounded fanout.
pub trait AcquisitionCommitObserver: Send + Sync + 'static {
    /// Observes the boundary after physical writes but before the adapter
    /// confirms that the mapped file is still authoritative.
    ///
    /// Production observers normally leave this hook untouched. It exists so
    /// conformance tests can deterministically exercise an atomic replacement
    /// at the otherwise tiny post-write identity-check boundary.
    fn before_authority_confirmation(&self) {}

    /// Observes one committed physical slot without influencing commit success.
    fn point_committed(&self, slot: usize, sample: AcquiredPointSample);
}

/// Typed acquisition writer over one immutable SHM generation.
///
/// The only data mutation exposed by this adapter is a validated batch of
/// domain [`AcquiredPointSample`] values. Slot-indexed writes remain private;
/// lifecycle operations expose only heartbeat, dirty-drain, and snapshot
/// capabilities needed by the owning io composition root.
pub struct ShmAcquisitionStateWriter {
    writer: Arc<SlotWriter>,
    manifest: Arc<ChannelPointManifest>,
    expected_generation: u64,
    local_authority_gate: Option<Arc<RwLock<()>>>,
    observer: Option<Arc<dyn AcquisitionCommitObserver>>,
}

impl ShmAcquisitionStateWriter {
    /// Composes one physical writer with the manifest for the same generation.
    #[must_use]
    pub fn new(writer: Arc<SlotWriter>, manifest: Arc<ChannelPointManifest>) -> Self {
        let expected_generation = writer.generation();
        Self {
            writer,
            manifest,
            expected_generation,
            local_authority_gate: None,
            observer: None,
        }
    }

    /// Adds the io process's in-memory replacement gate.
    ///
    /// Acquisition commits hold a read lease while `ShmWriterHandle` rebuilds hold
    /// the matching write lease from staging through local publication. The
    /// cross-process sidecar lock remains the source of serialization with
    /// other services.
    #[must_use]
    pub fn with_local_authority_gate(mut self, gate: Arc<RwLock<()>>) -> Self {
        self.local_authority_gate = Some(gate);
        self
    }

    /// Attaches a non-blocking observer invoked after every committed point.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn AcquisitionCommitObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Validates that the writer and manifest describe the same generation.
    pub fn validate_generation(&self) -> PortResult<()> {
        self.writer
            .validate_authoritative_path()
            .map_err(dataplane_port_error)?;
        let header = SlotIo::header(self.writer.as_ref());
        let expected_hash = self.manifest.layout_hash();
        let expected_slots = self.manifest.slot_count();
        if header.writer_generation == self.expected_generation
            && self.expected_generation != 0
            && self.expected_generation & 1 == 0
            && header.routing_hash == expected_hash
            && header.slot_count as usize == expected_slots
        {
            return Ok(());
        }
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "SHM generation mismatch: expected generation={} hash=0x{expected_hash:016x} slots={expected_slots}, found generation={} hash=0x{:016x} slots={}",
                self.expected_generation,
                header.writer_generation,
                header.routing_hash,
                header.slot_count
            ),
        ))
    }

    /// Validates and commits a complete batch synchronously.
    ///
    /// Prefer the [`AcquisitionStateWriter`] port from async application code.
    /// This inherent operation exists for synchronous composition shims while
    /// they migrate to the port.
    pub fn commit_batch(&self, samples: &[AcquiredPointSample]) -> PortResult<usize> {
        let _local_authority = self
            .local_authority_gate
            .as_ref()
            .map(|gate| {
                gate.read().map_err(|_| {
                    PortError::new(
                        PortErrorKind::Permanent,
                        "local SHM authority gate was poisoned",
                    )
                })
            })
            .transpose()?;
        let _cross_process_authority = self
            .writer
            .acquire_authority_read()
            .map_err(dataplane_port_error)?;
        self.validate_generation()?;
        let header = SlotIo::header(self.writer.as_ref());

        let mut seen = HashSet::with_capacity(samples.len());
        let mut resolved = Vec::with_capacity(samples.len());
        for sample in samples {
            let address = sample.address();
            if !address.kind().is_acquisition_owned() {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    format!(
                        "point kind {:?} is not owned by acquisition",
                        address.kind()
                    ),
                ));
            }
            let physical =
                PhysicalPointAddress::new(address.channel_id(), address.kind(), address.point_id());
            if !seen.insert(physical) {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("duplicate acquired point address {physical:?}"),
                ));
            }
            let slot = self.manifest.slot_for(physical).ok_or_else(|| {
                PortError::new(
                    PortErrorKind::NotFound,
                    format!("unknown acquired point address {physical:?}"),
                )
            })?;
            if slot >= header.slot_count as usize {
                return Err(PortError::new(
                    PortErrorKind::Conflict,
                    format!(
                        "manifest resolved {physical:?} to slot {slot}, outside live slot count {}",
                        header.slot_count
                    ),
                ));
            }
            resolved.push((slot, *sample));
        }

        // `SlotWriter` fixes slot_count for the lifetime of this generation.
        // Every slot was bounds-checked above, so these writes cannot fail and
        // no recoverable error can arise after the first mutation.
        for &(slot, sample) in &resolved {
            self.writer
                .set_direct(slot, sample.value(), sample.raw(), sample.timestamp().get());
        }
        if let Some(observer) = &self.observer {
            observer.before_authority_confirmation();
        }
        self.validate_generation()?;
        if let Some(observer) = &self.observer {
            for (slot, sample) in resolved {
                observer.point_committed(slot, sample);
            }
        }
        Ok(samples.len())
    }

    /// Refreshes writer liveness without mutating point state.
    pub fn update_heartbeat(&self, timestamp_ms: u64) {
        self.writer.update_heartbeat(timestamp_ms);
    }

    /// Returns the latest writer heartbeat.
    #[must_use]
    pub fn writer_heartbeat(&self) -> u64 {
        self.writer.writer_heartbeat()
    }

    /// Returns the immutable generation identifier.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.writer.generation()
    }

    /// Returns the number of live slots without exposing slot mutation.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.writer.slot_count()
    }

    /// Drains the process-local dirty-slot notification set.
    pub fn take_dirty_slots(&self) -> Vec<usize> {
        self.writer.take_dirty_slots()
    }

    /// Saves a tear-resistant snapshot of this writer generation.
    pub fn save_snapshot(&self, path: &Path) -> PortResult<()> {
        self.writer.save_snapshot(path).map_err(|error| {
            PortError::new(
                PortErrorKind::Permanent,
                format!("failed to save SHM snapshot: {error}"),
            )
        })
    }
}

fn dataplane_port_error(error: DataplaneError) -> PortError {
    let kind = match error {
        DataplaneError::Io { .. } => PortErrorKind::Unavailable,
        DataplaneError::InvalidLayout(_) | DataplaneError::InvalidPath(_) => {
            PortErrorKind::Conflict
        },
    };
    PortError::new(kind, error.to_string())
}

#[async_trait]
impl AcquisitionStateWriter for ShmAcquisitionStateWriter {
    async fn write_batch(&self, samples: &[AcquiredPointSample]) -> PortResult<usize> {
        self.commit_batch(samples)
    }
}
