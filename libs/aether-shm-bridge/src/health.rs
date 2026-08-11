//! Per-channel connectivity state on a dedicated SHM segment.

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

const CHANNEL_HEALTH_MANIFEST_DOMAIN: &str = "aether.channel-health.layout.v5";
const HEALTH_QUALITY_GOOD: u32 = 0;

/// Immutable dense mapping from configured channel identifiers to health slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelHealthManifest {
    channel_ids: Vec<u32>,
    layout_hash: u64,
}

impl ChannelHealthManifest {
    /// Compiles a canonical dense manifest under an explicit slot capacity.
    ///
    /// Duplicate channel identifiers are invalid rather than silently
    /// collapsed.
    pub fn compile(
        channel_ids: impl IntoIterator<Item = u32>,
        max_slots: usize,
    ) -> PortResult<Self> {
        let mut configured = Vec::new();
        for channel_id in channel_ids {
            if configured.len() >= max_slots {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("channel-health manifest exceeds configured capacity {max_slots}"),
                ));
            }
            configured.push(channel_id);
        }
        configured.sort_unstable();
        if let Some(duplicate) = configured
            .windows(2)
            .find_map(|pair| (pair[0] == pair[1]).then_some(pair[0]))
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("duplicate channel {duplicate} in channel-health manifest"),
            ));
        }
        let layout_hash = calculate_health_layout_hash(&configured);
        Ok(Self {
            channel_ids: configured,
            layout_hash,
        })
    }

    /// Builds a canonical channel set for synthetic test fixtures.
    #[doc(hidden)]
    #[must_use]
    pub fn test_fixture(channel_ids: impl IntoIterator<Item = u32>) -> Self {
        let mut channel_ids = channel_ids.into_iter().collect::<Vec<_>>();
        channel_ids.sort_unstable();
        channel_ids.dedup();
        let layout_hash = calculate_health_layout_hash(&channel_ids);
        Self {
            channel_ids,
            layout_hash,
        }
    }

    /// Returns whether the channel belongs to this configuration snapshot.
    #[must_use]
    pub fn contains(&self, channel_id: u32) -> bool {
        self.slot_for(channel_id).is_some()
    }

    /// Resolves a configured channel to its dense health slot.
    #[must_use]
    pub fn slot_for(&self, channel_id: u32) -> Option<usize> {
        self.channel_ids.binary_search(&channel_id).ok()
    }

    /// Returns the number of configured health slots.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.channel_ids.len()
    }

    /// Computes the cross-process manifest fingerprint.
    #[must_use]
    pub const fn layout_hash(&self) -> u64 {
        self.layout_hash
    }

    /// Iterates the configured ids in deterministic order.
    pub fn channel_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.channel_ids.iter().copied()
    }
}

impl Default for ChannelHealthManifest {
    fn default() -> Self {
        Self {
            channel_ids: Vec::new(),
            layout_hash: calculate_health_layout_hash(&[]),
        }
    }
}

fn calculate_health_layout_hash(channel_ids: &[u32]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    CHANNEL_HEALTH_MANIFEST_DOMAIN.hash(&mut hasher);
    (channel_ids.len() as u64).hash(&mut hasher);
    for (slot, channel_id) in channel_ids.iter().enumerate() {
        (slot as u64).hash(&mut hasher);
        channel_id.hash(&mut hasher);
    }
    hasher.finish()
}

/// Current single-writer channel-health generation owned by the handle.
struct ShmChannelHealthWriter {
    writer: Arc<SlotWriter>,
    manifest: Arc<ChannelHealthManifest>,
}

impl ShmChannelHealthWriter {
    /// Publishes one online/offline transition.
    ///
    /// The health-plane owner refreshes liveness through the dedicated
    /// heartbeat task; ordinary value writes never impersonate that owner.
    fn set_online(&self, channel_id: u32, online: bool, timestamp_ms: u64) -> PortResult<()> {
        let slot = self.manifest.slot_for(channel_id).ok_or_else(|| {
            PortError::new(
                PortErrorKind::Permanent,
                format!("channel {channel_id} is absent from the health manifest"),
            )
        })?;
        let _authority = self
            .writer
            .acquire_authority_read()
            .map_err(map_dataplane_error)?;
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)?;
        self.set_online_unchecked(channel_id, slot, online, timestamp_ms)?;
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)
    }

    fn set_online_unchecked(
        &self,
        channel_id: u32,
        slot: usize,
        online: bool,
        timestamp_ms: u64,
    ) -> PortResult<()> {
        let value = if online { 1.0 } else { 0.0 };
        if self
            .writer
            .write_slot(slot, value, value, timestamp_ms, HEALTH_QUALITY_GOOD)
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
        self.writer.header().publication_epoch
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

    /// Creates and atomically publishes the initial coordinated generation.
    pub fn create(
        path: impl Into<PathBuf>,
        manifest: Arc<ChannelHealthManifest>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        validate_topology_publication_epoch(publication_epoch)?;
        let handle = Self::empty(path);
        handle.rebuild(manifest, publication_epoch)?;
        Ok(handle)
    }

    /// Publishes a fresh coordinated health plane while preserving
    /// observations for channel ids present in both manifests.
    pub fn rebuild(
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

    /// Returns the current dense health-segment slot count.
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
    let sequence = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let staging_path = generation_file_path(canonical_path, sequence.max(1));
    let mut cleanup = HealthStagingCleanup(Some(staging_path.clone()));
    let staging_writer = SlotWriter::create(
        &staging_path,
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
        let previous_slot = previous.manifest.slot_for(channel_id).ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                format!("channel {channel_id} disappeared during health-state migration"),
            )
        })?;
        let target_slot = manifest.slot_for(channel_id).ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                format!("channel {channel_id} disappeared from the replacement health manifest"),
            )
        })?;
        let sample =
            SlotIo::read_slot(previous.writer.as_ref(), previous_slot).ok_or_else(|| {
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
            target_slot,
            sample.value,
            sample.raw,
            sample.timestamp_ms,
            sample.quality_code,
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
        let Some(slot) = self.manifest.slot_for(channel_id_value) else {
            return Ok(None);
        };
        let Some(sample) = self.source.read_slot(slot)? else {
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
