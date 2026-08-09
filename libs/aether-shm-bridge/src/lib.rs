//! Typed bridge between domain live-state ports and physical SHM.

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

use aether_domain::PointQuality;
use aether_ports::{PortError, PortErrorKind, PortResult};

pub use acquisition_writer::{AcquisitionCommitObserver, ShmAcquisitionStateWriter};
pub use aether_dataplane::core::config::{
    cleanup_orphan_generation_files, default_shm_path, timestamp_ms,
};
pub use aether_dataplane::{SubscriptionBitmap, WATCH_SLOT_CAPACITY, bitmap_path_for_consumer};
pub use aether_ports::ChannelHealthObservation as ChannelHealthSample;
pub use channel_reader::ShmChannelReader;
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
    ChannelHealthManifest, ShmChannelHealthReader, ShmChannelHealthWriterHandle,
    channel_health_path_from_shm,
};
pub use managed::{ReconnectingSlotSource, ShmClientConfig};
pub use manifest::{ChannelPointManifest, PhysicalPointAddress};
#[cfg(unix)]
pub use point_watch::PointWatchPublisher;
pub use read_topology::ShmReadTopologyGeneration;
pub use runtime::{DEFAULT_MAX_SLOTS, ShmRuntimeConfig, ShmWriterGeneration, ShmWriterHandle};
pub use topology_commit::{
    TopologyPublicationCommit, TopologyPublicationGuard, begin_topology_publication,
    commit_topology_publication, publish_topology_generation, read_topology_publication_commit,
    topology_commit_path_from_shm, validate_topology_publication,
};

/// Business-neutral value read from one authoritative SHM slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotSnapshot {
    value: f64,
    raw: f64,
    timestamp_ms: u64,
    quality: PointQuality,
}

impl SlotSnapshot {
    /// Creates a slot snapshot.
    #[must_use]
    pub const fn new(value: f64, timestamp_ms: u64, quality: PointQuality) -> Self {
        Self {
            value,
            raw: value,
            timestamp_ms,
            quality,
        }
    }

    /// Creates a slot snapshot retaining both engineering and raw values.
    #[must_use]
    pub const fn new_with_raw(
        value: f64,
        raw: f64,
        timestamp_ms: u64,
        quality: PointQuality,
    ) -> Self {
        Self {
            value,
            raw,
            timestamp_ms,
            quality,
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

    /// Returns the quality published by the acquisition source.
    #[must_use]
    pub const fn quality(self) -> PointQuality {
        self.quality
    }
}

pub(crate) const fn encode_point_quality(quality: PointQuality) -> u32 {
    match quality {
        PointQuality::Good => 0,
        PointQuality::Uncertain => 1,
        PointQuality::Bad => 2,
        PointQuality::Unavailable => 3,
    }
}

pub(crate) fn decode_point_quality(code: u32) -> PortResult<PointQuality> {
    match code {
        0 => Ok(PointQuality::Good),
        1 => Ok(PointQuality::Uncertain),
        2 => Ok(PointQuality::Bad),
        3 => Ok(PointQuality::Unavailable),
        _ => Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("SHM slot contains unknown point quality code {code}"),
        )),
    }
}

/// Minimal slot-indexed read contract used by the typed live-state bridge.
pub trait SlotSource: Send + Sync + 'static {
    /// Returns the number of readable slots.
    fn slot_count(&self) -> PortResult<usize>;

    /// Reads a seqlock-consistent slot. Transient contention and unavailable
    /// writers are reported with retryable port errors.
    fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>>;

    /// Reads several slots in request order.
    ///
    /// Implementations backed by one physical generation should override this
    /// method so generation and publication fencing can be amortised across
    /// the batch. The returned values are individually seqlock-consistent, but
    /// the batch is not an atomic snapshot across slots. Any slot or fencing
    /// error rejects the whole batch.
    fn read_slots(&self, indices: &[usize]) -> PortResult<Vec<Option<SlotSnapshot>>> {
        indices
            .iter()
            .copied()
            .map(|index| self.read_slot(index))
            .collect()
    }
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
            decode_point_quality(slot.quality_code)?,
        )))
    }
}
