//! Per-channel connectivity state on a dedicated SHM segment.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aether_dataplane::{SlotIoWrite, SlotWriter};
use aether_ports::{PortError, PortErrorKind, PortResult};

use crate::managed::map_dataplane_error;
use crate::{ReconnectingSlotSource, ShmClientConfig, SlotSource};

const CHANNEL_HEALTH_MANIFEST_DOMAIN: &str = "aether.channel-health.v1";

/// Immutable set of configured channel identifiers for the health segment.
///
/// The physical slot index is the channel id. Sparse ids intentionally leave
/// NaN slots between configured channels; this keeps lookup O(1) and the file
/// format independent from process-local hash maps.
#[derive(Debug, Clone, Default)]
pub struct ChannelHealthManifest {
    channel_ids: BTreeSet<u32>,
    slot_count: usize,
}

impl ChannelHealthManifest {
    /// Builds a canonical manifest from configured channel ids.
    #[must_use]
    pub fn from_channel_ids(channel_ids: impl IntoIterator<Item = u32>) -> Self {
        let channel_ids: BTreeSet<u32> = channel_ids.into_iter().collect();
        let slot_count = channel_ids
            .last()
            .map_or(0, |channel_id| *channel_id as usize + 1);
        Self {
            channel_ids,
            slot_count,
        }
    }

    /// Returns whether the channel belongs to this configuration snapshot.
    #[must_use]
    pub fn contains(&self, channel_id: u32) -> bool {
        self.channel_ids.contains(&channel_id)
    }

    /// Returns the physical slot count including sparse gaps.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Computes the cross-process manifest fingerprint.
    #[must_use]
    pub fn layout_hash(&self) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        CHANNEL_HEALTH_MANIFEST_DOMAIN.hash(&mut hasher);
        for channel_id in &self.channel_ids {
            channel_id.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Iterates the configured ids in deterministic order.
    pub fn channel_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.channel_ids.iter().copied()
    }
}

/// One connectivity sample read from the channel-health SHM plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelHealthSample {
    online: bool,
    timestamp_ms: u64,
}

impl ChannelHealthSample {
    /// Returns whether the channel was online at the sample timestamp.
    #[must_use]
    pub const fn online(self) -> bool {
        self.online
    }

    /// Returns when the connectivity state was observed.
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        self.timestamp_ms
    }
}

/// Single-writer channel-health SHM adapter used by acquisition/io.
pub struct ShmChannelHealthWriter {
    writer: SlotWriter,
    manifest: Arc<ChannelHealthManifest>,
}

impl ShmChannelHealthWriter {
    /// Creates a fresh health segment with every configured channel unknown.
    pub fn create(
        path: impl AsRef<Path>,
        manifest: Arc<ChannelHealthManifest>,
    ) -> PortResult<Self> {
        let canonical_path = path.as_ref();
        let max_slots = u32::try_from(manifest.slot_count()).map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                format!(
                    "channel health slot count {} exceeds u32 capacity",
                    manifest.slot_count()
                ),
            )
        })?;
        let _ = aether_dataplane::core::config::cleanup_orphan_generation_files(canonical_path);
        let sequence = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(1);
        let staging_path =
            aether_dataplane::core::config::generation_file_path(canonical_path, sequence);
        let writer = match SlotWriter::create(
            &staging_path,
            max_slots,
            manifest.slot_count(),
            manifest.layout_hash(),
        ) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = std::fs::remove_file(&staging_path);
                return Err(map_dataplane_error(error));
            },
        };
        if let Err(error) = writer.flush() {
            let _ = std::fs::remove_file(&staging_path);
            return Err(map_dataplane_error(error));
        }
        if let Err(error) =
            aether_dataplane::core::config::commit_generation_swap(&staging_path, canonical_path)
        {
            let _ = std::fs::remove_file(&staging_path);
            return Err(map_dataplane_error(error));
        }
        Ok(Self { writer, manifest })
    }

    /// Publishes one online/offline transition and refreshes writer heartbeat.
    pub fn set_online(&self, channel_id: u32, online: bool, timestamp_ms: u64) -> PortResult<()> {
        if !self.manifest.contains(channel_id) {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                format!("channel {channel_id} is absent from the health manifest"),
            ));
        }
        let value = if online { 1.0 } else { 0.0 };
        if !self
            .writer
            .write_slot(channel_id as usize, value, value, timestamp_ms)
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("channel {channel_id} resolved outside the health segment"),
            ));
        }
        Ok(())
    }

    /// Refreshes liveness even when no channel changes state.
    pub fn update_heartbeat(&self, timestamp_ms: u64) {
        self.writer.update_heartbeat(timestamp_ms);
    }
}

/// Self-healing read adapter for the channel-health SHM segment.
pub struct ShmChannelHealthReader {
    source: ReconnectingSlotSource,
    manifest: Arc<ChannelHealthManifest>,
}

impl ShmChannelHealthReader {
    /// Creates a lazy reader with mandatory health-manifest validation.
    #[must_use]
    pub fn new(config: ShmClientConfig, manifest: Arc<ChannelHealthManifest>) -> Self {
        Self {
            source: ReconnectingSlotSource::new(config),
            manifest,
        }
    }

    /// Reads a channel state. `None` means unconfigured or not observed yet.
    pub fn read_channel(&self, channel_id: u32) -> PortResult<Option<ChannelHealthSample>> {
        if !self.manifest.contains(channel_id) {
            return Ok(None);
        }
        let Some(sample) = self.source.read_slot(channel_id as usize)? else {
            return Ok(None);
        };
        let online = match sample.value() {
            value if value.is_nan() => return Ok(None),
            0.0 => false,
            1.0 => true,
            value => {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("channel {channel_id} has invalid health value {value}"),
                ));
            },
        };
        Ok(Some(ChannelHealthSample {
            online,
            timestamp_ms: sample.timestamp_ms(),
        }))
    }
}

/// Derives the sibling channel-health path from the main live-state SHM path.
#[must_use]
pub fn channel_health_path_from_shm(shm_path: &Path) -> PathBuf {
    let stem = shm_path
        .file_stem()
        .or_else(|| shm_path.file_name())
        .unwrap_or_default();
    let mut file_name = OsString::from(stem);
    file_name.push("-health");
    if let Some(extension) = shm_path.extension() {
        file_name.push(".");
        file_name.push(extension);
    }
    shm_path.with_file_name(file_name)
}
