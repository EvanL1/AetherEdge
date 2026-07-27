//! Typed migration bridge between domain live-state ports and physical SHM.

mod acquisition_writer;
mod channel_reader;
#[cfg(unix)]
mod command_sink;
#[cfg(unix)]
mod events;
mod health;
mod managed;
mod manifest;
#[cfg(unix)]
mod point_watch;
mod read_topology;
mod runtime;
mod topology_commit;

use std::collections::HashMap;
use std::sync::Arc;

use aether_domain::{PointAddress, PointQuality, PointSample, TimestampMs};
use aether_ports::{LiveState, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;

pub use acquisition_writer::{AcquisitionCommitObserver, ShmAcquisitionStateWriter};
pub use aether_dataplane::core::config::{
    cleanup_orphan_generation_files, default_shm_path, timestamp_ms,
};
pub use aether_dataplane::{
    DEFAULT_MAX_SLOTS, SubscriptionBitmap, automation_bitmap_path_from_shm,
    bitmap_path_for_consumer,
};
pub use aether_ports::ChannelHealthObservation as ChannelHealthSample;
pub use channel_reader::{ShmChannelReader, ShmChannelReaderHandle};
#[cfg(unix)]
pub use command_sink::{
    ChannelPointManifestSource, CommandMirrorObserver, DEFAULT_COMMAND_UDS_PATH,
    DeviceCommandFrame, ShmDeviceCommandSink,
};
#[cfg(unix)]
pub use events::{
    PointWatchEvent, PointWatchEventListener, point_watch_socket_for_consumer,
    point_watch_socket_from_shm,
};
pub use health::{
    ChannelHealthManifest, ShmChannelHealthReader, ShmChannelHealthWriter,
    ShmChannelHealthWriterHandle, channel_health_path_from_shm,
};
pub use managed::{ReconnectingSlotSource, ShmClientConfig};
pub use manifest::{
    CHANNEL_POINT_KINDS, ChannelPointLayout, ChannelPointManifest, PhysicalPointAddress,
};
#[cfg(unix)]
pub use point_watch::PointWatchPublisher;
pub use read_topology::{ShmReadTopologyGeneration, ShmReadTopologyHandle};
pub use runtime::{ShmRuntimeConfig, ShmWriterGeneration, ShmWriterHandle};
pub use topology_commit::{
    TopologyPublicationCommit, TopologyPublicationGuard, begin_topology_publication,
    commit_topology_publication, publish_topology_generation, read_topology_publication_commit,
    topology_commit_path_from_shm, validate_topology_publication,
};

/// Business-neutral value read from one legacy SHM slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotSnapshot {
    value: f64,
    raw: f64,
    timestamp_ms: u64,
}

impl SlotSnapshot {
    /// Creates a slot snapshot.
    #[must_use]
    pub const fn new(value: f64, timestamp_ms: u64) -> Self {
        Self {
            value,
            raw: value,
            timestamp_ms,
        }
    }

    /// Creates a slot snapshot retaining both engineering and raw values.
    #[must_use]
    pub const fn new_with_raw(value: f64, raw: f64, timestamp_ms: u64) -> Self {
        Self {
            value,
            raw,
            timestamp_ms,
        }
    }

    /// Returns the engineering-unit value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }

    /// Returns the raw device value.
    #[must_use]
    pub const fn raw(self) -> f64 {
        self.raw
    }

    /// Returns the source timestamp in milliseconds since UNIX epoch.
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        self.timestamp_ms
    }
}

/// Minimal slot-indexed read contract used by the migration bridge.
pub trait SlotSource: Send + Sync + 'static {
    /// Returns the number of readable slots.
    fn slot_count(&self) -> PortResult<usize>;

    /// Reads a seqlock-consistent slot. Transient contention and unavailable
    /// writers are reported with retryable port errors.
    fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>>;
}

impl<T> SlotSource for T
where
    T: aether_dataplane::SlotIo + 'static,
{
    fn slot_count(&self) -> PortResult<usize> {
        Ok(aether_dataplane::SlotIo::slot_count(self))
    }

    fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>> {
        let slot_count = aether_dataplane::SlotIo::slot_count(self);
        if index >= slot_count {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("slot {index} is outside live slot_count {slot_count}"),
            ));
        }
        let slot = aether_dataplane::SlotIo::read_slot(self, index).ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                format!("slot {index} was being updated during the read"),
            )
        })?;
        Ok(Some(SlotSnapshot::new_with_raw(
            slot.value,
            slot.raw,
            slot.timestamp_ms,
        )))
    }
}

/// Resolves a domain point address to a physical SHM slot.
pub trait PointSlotResolver: Send + Sync + 'static {
    /// Returns the physical slot for a point address.
    fn resolve(&self, address: PointAddress) -> Option<usize>;
}

/// Immutable resolver useful for snapshots of routing configuration.
#[derive(Debug, Clone, Default)]
pub struct StaticSlotResolver {
    slots: HashMap<PointAddress, usize>,
}

impl StaticSlotResolver {
    /// Builds a resolver from domain-address/slot pairs.
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = (PointAddress, usize)>) -> Self {
        Self {
            slots: entries.into_iter().collect(),
        }
    }

    /// Builds a resolver from an existing map.
    #[must_use]
    pub const fn from_map(slots: HashMap<PointAddress, usize>) -> Self {
        Self { slots }
    }
}

impl PointSlotResolver for StaticSlotResolver {
    fn resolve(&self, address: PointAddress) -> Option<usize> {
        self.slots.get(&address).copied()
    }
}

/// Read-only [`LiveState`] adapter over the existing SHM slot contract.
pub struct ShmLiveState {
    source: Arc<dyn SlotSource>,
    resolver: Arc<dyn PointSlotResolver>,
}

impl ShmLiveState {
    /// Creates a read-only bridge. It intentionally accepts no writer.
    #[must_use]
    pub fn new<S, R>(source: Arc<S>, resolver: Arc<R>) -> Self
    where
        S: SlotSource,
        R: PointSlotResolver,
    {
        Self { source, resolver }
    }

    fn read_resolved(&self, address: PointAddress) -> PortResult<Option<PointSample>> {
        let Some(slot_index) = self.resolver.resolve(address) else {
            return Ok(None);
        };
        let slot_count = self.source.slot_count()?;
        if slot_index >= slot_count {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "point {address:?} resolved to slot {slot_index}, but slot_count is {slot_count}"
                ),
            ));
        }

        let slot = self.source.read_slot(slot_index)?.ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                format!("slot {slot_index} was being updated during the read"),
            )
        })?;
        if slot.value.is_nan() {
            return Ok(None);
        }
        if !slot.value.is_finite() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("slot {slot_index} contains a non-finite value"),
            ));
        }

        Ok(Some(PointSample::new(
            address,
            slot.value,
            TimestampMs::new(slot.timestamp_ms),
            PointQuality::Good,
        )))
    }
}

#[async_trait]
impl LiveState for ShmLiveState {
    async fn read(&self, address: PointAddress) -> PortResult<Option<PointSample>> {
        self.read_resolved(address)
    }

    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>> {
        addresses
            .iter()
            .copied()
            .map(|address| self.read_resolved(address))
            .collect()
    }
}
