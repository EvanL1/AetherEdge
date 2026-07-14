//! Per-channel connectivity state on a dedicated SHM segment.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aether_dataplane::core::config::{commit_generation_swap_locked, generation_file_path};
use aether_dataplane::{AuthorityWriteGuard, SlotIo, SlotIoWrite, SlotWriter};
use aether_domain::{ChannelId, TimestampMs};
use aether_ports::{
    ChannelHealthObservation, ChannelHealthSource, PortError, PortErrorKind, PortResult,
};
use arc_swap::ArcSwapOption;

use crate::managed::map_dataplane_error;
use crate::topology_commit::validate_topology_publication_epoch;
use crate::{ReconnectingSlotSource, ShmClientConfig, SlotSource};

const CHANNEL_HEALTH_MANIFEST_DOMAIN: &str = "aether.channel-health.v1";

/// Immutable set of configured channel identifiers for the health segment.
///
/// The physical slot index is the channel id. Sparse ids intentionally leave
/// NaN slots between configured channels; this keeps lookup O(1) and the file
/// format independent from process-local hash maps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// Single-writer channel-health SHM adapter used by acquisition/io.
pub struct ShmChannelHealthWriter {
    writer: Arc<SlotWriter>,
    manifest: Arc<ChannelHealthManifest>,
}

impl ShmChannelHealthWriter {
    /// Creates a fresh health segment with every configured channel unknown.
    pub fn create(
        path: impl AsRef<Path>,
        manifest: Arc<ChannelHealthManifest>,
    ) -> PortResult<Self> {
        Self::create_internal(path.as_ref(), manifest, 0)
    }

    /// Creates a fresh health segment carrying the coordinated publication
    /// identity selected by the IO composition root.
    pub fn create_at_epoch(
        path: impl AsRef<Path>,
        manifest: Arc<ChannelHealthManifest>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        validate_topology_publication_epoch(publication_epoch)?;
        Self::create_internal(path.as_ref(), manifest, publication_epoch)
    }

    fn create_internal(
        canonical_path: &Path,
        manifest: Arc<ChannelHealthManifest>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        let authority =
            AuthorityWriteGuard::acquire(canonical_path).map_err(map_dataplane_error)?;
        publish_health_writer(
            canonical_path,
            manifest,
            None,
            &authority,
            publication_epoch,
        )
    }

    /// Publishes one online/offline transition and refreshes writer heartbeat.
    pub fn set_online(&self, channel_id: u32, online: bool, timestamp_ms: u64) -> PortResult<()> {
        if !self.manifest.contains(channel_id) {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                format!("channel {channel_id} is absent from the health manifest"),
            ));
        }
        let _authority = self
            .writer
            .acquire_authority_read()
            .map_err(map_dataplane_error)?;
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)?;
        self.set_online_unchecked(channel_id, online, timestamp_ms)?;
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)
    }

    /// Refreshes liveness even when no channel changes state.
    pub fn update_heartbeat(&self, timestamp_ms: u64) {
        let _ = self.try_update_heartbeat(timestamp_ms);
    }

    fn set_online_unchecked(
        &self,
        channel_id: u32,
        online: bool,
        timestamp_ms: u64,
    ) -> PortResult<()> {
        let value = if online { 1.0 } else { 0.0 };
        if self
            .writer
            .write_slot(channel_id as usize, value, value, timestamp_ms)
        {
            return Ok(());
        }
        Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("channel {channel_id} resolved outside the health segment"),
        ))
    }

    fn try_update_heartbeat(&self, timestamp_ms: u64) -> PortResult<()> {
        let _authority = self
            .writer
            .acquire_authority_read()
            .map_err(map_dataplane_error)?;
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)?;
        self.writer.update_heartbeat(timestamp_ms);
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)
    }

    fn validate_authoritative_path(&self) -> PortResult<()> {
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)
    }

    fn generation(&self) -> u64 {
        self.writer.generation()
    }

    fn publication_epoch(&self) -> u64 {
        self.writer.header().publication_epoch()
    }

    fn slot_count(&self) -> usize {
        self.writer.slot_count()
    }

    fn writer_heartbeat(&self) -> u64 {
        self.writer.writer_heartbeat()
    }
}

/// Runtime-swappable writer for the acquisition-owned channel-health plane.
///
/// Every mutation takes a shared local lease and a shared cross-process
/// authority lease. Rebuilds take both exclusive leases from staging through
/// canonical reopen and local publication, so a retained generation can never
/// write after it stops being authoritative.
pub struct ShmChannelHealthWriterHandle {
    current: ArcSwapOption<ShmChannelHealthWriter>,
    path: PathBuf,
    authority_gate: RwLock<()>,
}

impl std::fmt::Debug for ShmChannelHealthWriterHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShmChannelHealthWriterHandle")
            .field("path", &self.path)
            .field("available", &self.current.load().is_some())
            .finish()
    }
}

impl ShmChannelHealthWriterHandle {
    /// Creates an unavailable handle for delayed acquisition startup.
    #[must_use]
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            current: ArcSwapOption::empty(),
            path: path.into(),
            authority_gate: RwLock::new(()),
        }
    }

    /// Creates and atomically publishes the initial health generation.
    pub fn create(
        path: impl Into<PathBuf>,
        manifest: Arc<ChannelHealthManifest>,
    ) -> PortResult<Self> {
        let handle = Self::empty(path);
        handle.rebuild(manifest)?;
        Ok(handle)
    }

    /// Creates and atomically publishes the initial coordinated health
    /// generation.
    pub fn create_at_epoch(
        path: impl Into<PathBuf>,
        manifest: Arc<ChannelHealthManifest>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        validate_topology_publication_epoch(publication_epoch)?;
        let handle = Self::empty(path);
        handle.rebuild_for_publication(manifest, publication_epoch)?;
        Ok(handle)
    }

    /// Publishes a fresh health manifest while preserving observations for
    /// channel ids present in both the old and new manifests.
    ///
    /// Rebuilding an identical canonical manifest is a true no-op: it does not
    /// replace the inode, advance the writer generation, or refresh heartbeat.
    pub fn rebuild(&self, manifest: Arc<ChannelHealthManifest>) -> PortResult<()> {
        self.rebuild_internal(manifest, 0, false)
    }

    /// Publishes a fresh health plane for a coordinated topology epoch.
    /// Unlike the compatibility rebuild path, a new epoch always replaces an
    /// identical manifest so both physical planes carry the same identity.
    pub fn rebuild_for_publication(
        &self,
        manifest: Arc<ChannelHealthManifest>,
        publication_epoch: u64,
    ) -> PortResult<()> {
        validate_topology_publication_epoch(publication_epoch)?;
        if self.publication_epoch() == Some(publication_epoch) {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "channel-health SHM publication epoch {publication_epoch} was already used"
                ),
            ));
        }
        self.rebuild_internal(manifest, publication_epoch, true)
    }

    fn rebuild_internal(
        &self,
        manifest: Arc<ChannelHealthManifest>,
        publication_epoch: u64,
        force_publication: bool,
    ) -> PortResult<()> {
        let _local_authority = self.authority_gate.write().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "local channel-health authority gate was poisoned",
            )
        })?;
        let cross_process_authority =
            AuthorityWriteGuard::acquire(&self.path).map_err(map_dataplane_error)?;
        let previous = self.current.load_full();

        if let Some(previous) = previous.as_ref() {
            previous.validate_authoritative_path()?;
            if !force_publication && previous.manifest.as_ref() == manifest.as_ref() {
                return Ok(());
            }
        }

        let replacement = publish_health_writer(
            &self.path,
            manifest,
            previous.as_deref(),
            &cross_process_authority,
            publication_epoch,
        )?;
        self.current.store(Some(Arc::new(replacement)));
        Ok(())
    }

    /// Publishes one online/offline observation to the current generation.
    pub fn set_online(&self, channel_id: u32, online: bool, timestamp_ms: u64) -> PortResult<()> {
        let _local_authority = self.authority_gate.read().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "local channel-health authority gate was poisoned",
            )
        })?;
        self.current_writer()?
            .set_online(channel_id, online, timestamp_ms)
    }

    /// Refreshes current writer liveness without changing channel state.
    pub fn update_heartbeat(&self, timestamp_ms: u64) -> PortResult<()> {
        let _local_authority = self.authority_gate.read().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "local channel-health authority gate was poisoned",
            )
        })?;
        self.current_writer()?.try_update_heartbeat(timestamp_ms)
    }

    /// Returns the canonical health-segment path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns whether a canonical writer is currently published locally.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.current.load().is_some()
    }

    /// Returns the immutable manifest for the current coherent generation.
    #[must_use]
    pub fn manifest(&self) -> Option<Arc<ChannelHealthManifest>> {
        self.current
            .load_full()
            .map(|current| Arc::clone(&current.manifest))
    }

    /// Returns the current physical writer generation.
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        self.current.load_full().map(|current| current.generation())
    }

    /// Returns the current cross-plane publication identity.
    #[must_use]
    pub fn publication_epoch(&self) -> Option<u64> {
        self.current
            .load_full()
            .map(|current| current.publication_epoch())
    }

    /// Returns the current sparse health-segment slot count.
    #[must_use]
    pub fn slot_count(&self) -> Option<usize> {
        self.current.load_full().map(|current| current.slot_count())
    }

    /// Returns the last heartbeat published by the current writer.
    #[must_use]
    pub fn writer_heartbeat(&self) -> Option<u64> {
        self.current
            .load_full()
            .map(|current| current.writer_heartbeat())
    }

    fn current_writer(&self) -> PortResult<Arc<ShmChannelHealthWriter>> {
        self.current.load_full().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Unavailable,
                "channel-health SHM writer is unavailable",
            )
        })
    }
}

fn publish_health_writer(
    canonical_path: &Path,
    manifest: Arc<ChannelHealthManifest>,
    previous: Option<&ShmChannelHealthWriter>,
    authority: &AuthorityWriteGuard,
    publication_epoch: u64,
) -> PortResult<ShmChannelHealthWriter> {
    let max_slots = u32::try_from(manifest.slot_count()).map_err(|_| {
        PortError::new(
            PortErrorKind::Permanent,
            format!(
                "channel health slot count {} exceeds u32 capacity",
                manifest.slot_count()
            ),
        )
    })?;
    let sequence = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let staging_path = generation_file_path(canonical_path, sequence.max(1));
    let mut cleanup = HealthStagingCleanup(Some(staging_path.clone()));
    let staging_writer = SlotWriter::create_at_epoch(
        &staging_path,
        max_slots,
        manifest.slot_count(),
        manifest.layout_hash(),
        publication_epoch,
    )
    .map_err(map_dataplane_error)?;

    if let Some(previous) = previous {
        migrate_intersection(&staging_writer, &manifest, previous)?;
        staging_writer.update_heartbeat(previous.writer_heartbeat());
    }
    staging_writer.flush().map_err(map_dataplane_error)?;
    let discovered_previous = if previous.is_none() {
        SlotWriter::open_canonical_for_replacement(canonical_path, authority)
            .map_err(map_dataplane_error)?
    } else {
        None
    };
    let previous_writer = previous
        .map(|previous| previous.writer.as_ref())
        .or(discovered_previous.as_ref());
    let invalidation = previous_writer
        .map(|writer| {
            writer
                .begin_generation_swap(authority)
                .map_err(map_dataplane_error)
        })
        .transpose()?;
    commit_generation_swap_locked(&staging_path, canonical_path, authority)
        .map_err(map_dataplane_error)?;
    if let Some(invalidation) = invalidation {
        invalidation.commit();
    }
    cleanup.0 = None;
    drop(staging_writer);

    let writer = SlotWriter::open_existing(
        canonical_path,
        manifest.slot_count(),
        manifest.layout_hash(),
    )
    .map_err(map_dataplane_error)?;
    Ok(ShmChannelHealthWriter {
        writer: Arc::new(writer),
        manifest,
    })
}

fn migrate_intersection(
    staging_writer: &SlotWriter,
    manifest: &ChannelHealthManifest,
    previous: &ShmChannelHealthWriter,
) -> PortResult<()> {
    for channel_id in manifest
        .channel_ids()
        .filter(|channel_id| previous.manifest.contains(*channel_id))
    {
        let sample =
            SlotIo::read_slot(previous.writer.as_ref(), channel_id as usize).ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Conflict,
                    format!("channel {channel_id} health state changed during migration"),
                )
            })?;
        if sample.value.is_nan() {
            continue;
        }
        let valid_offline = sample.value == 0.0 && sample.raw == 0.0;
        let valid_online = sample.value == 1.0 && sample.raw == 1.0;
        if !valid_offline && !valid_online {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("channel {channel_id} has invalid health state"),
            ));
        }
        staging_writer.set_direct(
            channel_id as usize,
            sample.value,
            sample.raw,
            sample.timestamp_ms,
        );
    }
    Ok(())
}

struct HealthStagingCleanup(Option<PathBuf>);

impl Drop for HealthStagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
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

    /// Eagerly validates the health-plane layout for topology publication.
    pub fn validate_layout(&self) -> PortResult<()> {
        self.source.validate_layout(self.manifest.slot_count())
    }

    pub(crate) fn require_coordinated_publication(&self) {
        self.source.require_coordinated_publication();
    }

    pub(crate) fn accept_publication_identity(
        &self,
        publication_epoch: u64,
        writer_generation: u64,
    ) -> PortResult<()> {
        self.source
            .accept_publication_identity(publication_epoch, writer_generation)
    }

    /// Returns the immutable health manifest paired with this reader.
    #[must_use]
    pub fn manifest(&self) -> &Arc<ChannelHealthManifest> {
        &self.manifest
    }

    /// Reads a channel state. `None` means unconfigured or not observed yet.
    pub fn read_channel(&self, channel_id: u32) -> PortResult<Option<ChannelHealthObservation>> {
        self.read_observation(ChannelId::new(channel_id))
    }

    fn read_observation(
        &self,
        channel_id: ChannelId,
    ) -> PortResult<Option<ChannelHealthObservation>> {
        let channel_id_value = channel_id.get();
        if !self.manifest.contains(channel_id_value) {
            return Ok(None);
        }
        let Some(sample) = self.source.read_slot(channel_id_value as usize)? else {
            return Ok(None);
        };
        let online = match sample.value() {
            value if value.is_nan() => return Ok(None),
            0.0 => false,
            1.0 => true,
            value => {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("channel {channel_id_value} has invalid health value {value}"),
                ));
            },
        };
        Ok(Some(ChannelHealthObservation::new(
            channel_id,
            online,
            TimestampMs::new(sample.timestamp_ms()),
        )))
    }
}

impl ChannelHealthSource for ShmChannelHealthReader {
    fn read_channel(&self, channel_id: ChannelId) -> PortResult<Option<ChannelHealthObservation>> {
        self.read_observation(channel_id)
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
