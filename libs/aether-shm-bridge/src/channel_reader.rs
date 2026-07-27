//! Typed read-only channel view over one validated physical SHM generation.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aether_dataplane::{SlotIo, SlotReader};
use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use arc_swap::ArcSwap;

use crate::{ChannelPointManifest, PhysicalPointAddress, SlotSnapshot};

/// Read-only SHM mapping paired with the exact physical channel manifest.
pub struct ShmChannelReader {
    reader: SlotReader,
    manifest: Option<Arc<ChannelPointManifest>>,
    expected_generation: u64,
}

impl ShmChannelReader {
    /// Opens a physical segment and validates its layout identity.
    pub fn open(path: impl AsRef<Path>, manifest: Arc<ChannelPointManifest>) -> PortResult<Self> {
        let reader = SlotReader::open(path).map_err(map_dataplane_error)?;
        let header = reader.header();
        validate_stable_generation(header.writer_generation)?;
        if header.slot_count as usize != manifest.slot_count()
            || header.routing_hash != manifest.layout_hash()
        {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                format!(
                    "SHM layout does not match channel manifest: SHM slots={} hash=0x{:016x}, manifest slots={} hash=0x{:016x}",
                    header.slot_count,
                    header.routing_hash,
                    manifest.slot_count(),
                    manifest.layout_hash()
                ),
            ));
        }
        Ok(Self {
            reader,
            manifest: Some(manifest),
            expected_generation: header.writer_generation,
        })
    }

    /// Opens the physical segment without channel metadata for diagnostics.
    /// Channel-addressed reads are unavailable on this view.
    pub fn open_raw(path: impl AsRef<Path>) -> PortResult<Self> {
        let reader = SlotReader::open(path).map_err(map_dataplane_error)?;
        let generation = SlotIo::generation(&reader);
        validate_stable_generation(generation)?;
        Ok(Self {
            reader,
            manifest: None,
            expected_generation: generation,
        })
    }

    /// Reads one typed physical channel point.
    pub fn read_channel(
        &self,
        channel_id: u32,
        kind: PointKind,
        point_id: u32,
    ) -> PortResult<Option<SlotSnapshot>> {
        let Some(manifest) = &self.manifest else {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "raw SHM reader has no channel manifest",
            ));
        };
        let Some(slot) = manifest.slot_for(PhysicalPointAddress::from_legacy_raw(
            channel_id, kind, point_id,
        )) else {
            return Ok(None);
        };
        self.read_physical_slot(slot)
    }

    /// Reads one address from the typed physical manifest.
    pub fn read_physical(&self, address: PhysicalPointAddress) -> PortResult<Option<SlotSnapshot>> {
        let Some(manifest) = &self.manifest else {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "raw SHM reader has no channel manifest",
            ));
        };
        let Some(slot) = manifest.slot_for(address) else {
            return Ok(None);
        };
        self.read_physical_slot(slot)
    }

    /// Reads a slot for diagnostic/mirror paths without granting mutation.
    pub fn read_physical_slot(&self, slot: usize) -> PortResult<Option<SlotSnapshot>> {
        self.validate_generation()?;
        if slot >= self.reader.slot_count() {
            return Ok(None);
        }
        let value = SlotIo::read_slot(&self.reader, slot);
        self.validate_generation()?;
        let value = value.ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                format!("slot {slot} was being updated during the read"),
            )
        })?;
        if value.value.is_nan() {
            return Ok(None);
        }
        if !value.value.is_finite() || !value.raw.is_finite() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("slot {slot} contains non-finite live data"),
            ));
        }
        Ok(Some(SlotSnapshot::new_with_raw(
            value.value,
            value.raw,
            value.timestamp_ms,
        )))
    }

    /// Returns the paired manifest, if this is not a raw diagnostic view.
    #[must_use]
    pub fn manifest(&self) -> Option<&Arc<ChannelPointManifest>> {
        self.manifest.as_ref()
    }

    /// Returns physical channel ids in deterministic order.
    pub fn channel_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.manifest
            .iter()
            .flat_map(|manifest| manifest.counts().keys().copied())
    }

    /// Returns the number of live physical slots.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.reader.slot_count()
    }

    /// Returns the mapping capacity.
    #[must_use]
    pub fn max_slots(&self) -> u32 {
        self.reader.max_slots()
    }

    /// Returns the writer generation captured by this mapping.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.expected_generation
    }

    /// Returns the latest writer heartbeat.
    #[must_use]
    pub fn writer_heartbeat(&self) -> u64 {
        self.reader.writer_heartbeat()
    }

    /// Checks writer liveness using the caller's freshness policy.
    #[must_use]
    pub fn is_writer_alive(&self, timeout: Duration) -> bool {
        let timeout_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        self.validate_generation().is_ok() && self.reader.is_writer_alive(timeout_ms)
    }

    fn validate_generation(&self) -> PortResult<()> {
        let observed = SlotIo::generation(&self.reader);
        if observed == self.expected_generation && observed & 1 == 0 {
            return Ok(());
        }
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "SHM reader generation changed from {} to {observed}; reopen the canonical segment",
                self.expected_generation
            ),
        ))
    }
}

/// Atomically replaceable read mapping for long-lived consumers.
pub struct ShmChannelReaderHandle {
    current: ArcSwap<ShmChannelReader>,
}

impl ShmChannelReaderHandle {
    /// Creates a handle over an initially validated reader.
    #[must_use]
    pub fn new(reader: Arc<ShmChannelReader>) -> Self {
        Self {
            current: ArcSwap::new(reader),
        }
    }

    /// Publishes a newly validated mapping.
    pub fn replace(&self, reader: Arc<ShmChannelReader>) {
        self.current.store(reader);
    }

    /// Reads one typed physical channel point from the current generation.
    pub fn read_channel(
        &self,
        channel_id: u32,
        kind: PointKind,
        point_id: u32,
    ) -> PortResult<Option<SlotSnapshot>> {
        self.current.load().read_channel(channel_id, kind, point_id)
    }

    /// Returns the current reader generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.current.load().generation()
    }
}

fn map_dataplane_error(error: aether_dataplane::DataplaneError) -> PortError {
    let kind = match error {
        aether_dataplane::DataplaneError::Io { .. } => PortErrorKind::Unavailable,
        aether_dataplane::DataplaneError::InvalidLayout(_)
        | aether_dataplane::DataplaneError::InvalidPath(_) => PortErrorKind::Conflict,
    };
    PortError::new(kind, error.to_string())
}

fn validate_stable_generation(generation: u64) -> PortResult<()> {
    if generation != 0 && generation & 1 == 0 {
        return Ok(());
    }
    Err(PortError::new(
        PortErrorKind::Conflict,
        format!("SHM writer generation {generation} is not stably published"),
    ))
}
