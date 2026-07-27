//! Acquisition-owned lifecycle for atomically published SHM generations.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aether_dataplane::core::config::{commit_generation_swap_locked, generation_file_path};
use aether_dataplane::{AuthorityWriteGuard, SlotIo, SlotWriter};
use aether_ports::{PortError, PortErrorKind, PortResult};
use arc_swap::ArcSwapOption;

use crate::topology_commit::validate_topology_publication_epoch;
use crate::{AcquisitionCommitObserver, ChannelPointManifest, ShmAcquisitionStateWriter};

/// Physical writer configuration selected by the IO composition root.
#[derive(Debug, Clone)]
pub struct ShmRuntimeConfig {
    path: PathBuf,
    max_slots: u32,
}

impl ShmRuntimeConfig {
    /// Creates a runtime configuration for one canonical segment.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, max_slots: u32) -> Self {
        Self {
            path: path.into(),
            max_slots,
        }
    }

    /// Returns the canonical SHM path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the physical slot capacity.
    #[must_use]
    pub const fn max_slots(&self) -> u32 {
        self.max_slots
    }
}

/// One coherent writer/manifest generation published by [`ShmWriterHandle`].
pub struct ShmWriterGeneration {
    writer: Arc<SlotWriter>,
    manifest: Arc<ChannelPointManifest>,
    acquisition_writer: Arc<ShmAcquisitionStateWriter>,
    authority_gate: Arc<RwLock<()>>,
}

impl ShmWriterGeneration {
    fn compose(
        writer: Arc<SlotWriter>,
        manifest: Arc<ChannelPointManifest>,
        authority_gate: Arc<RwLock<()>>,
        observer: Option<Arc<dyn AcquisitionCommitObserver>>,
    ) -> PortResult<Self> {
        let mut acquisition_writer =
            ShmAcquisitionStateWriter::new(Arc::clone(&writer), Arc::clone(&manifest))
                .with_local_authority_gate(Arc::clone(&authority_gate));
        if let Some(observer) = observer {
            acquisition_writer = acquisition_writer.with_observer(observer);
        }
        acquisition_writer.validate_generation()?;
        Ok(Self {
            writer,
            manifest,
            acquisition_writer: Arc::new(acquisition_writer),
            authority_gate,
        })
    }

    /// Returns the acquisition-only T/S writer for this generation.
    #[must_use]
    pub fn acquisition_writer(&self) -> &Arc<ShmAcquisitionStateWriter> {
        &self.acquisition_writer
    }

    /// Returns the immutable physical channel manifest for this generation.
    #[must_use]
    pub fn manifest(&self) -> &Arc<ChannelPointManifest> {
        &self.manifest
    }

    /// Returns the stable writer generation number.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.writer.generation()
    }

    /// Returns the cross-plane publication identity stored in this segment.
    #[must_use]
    pub fn publication_epoch(&self) -> u64 {
        self.writer.header().publication_epoch()
    }

    /// Returns the number of live slots, including alignment padding.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.writer.slot_count()
    }

    /// Reads one physical slot for diagnostics and mirror workers.
    #[must_use]
    pub fn read_slot(&self, slot: usize) -> Option<aether_dataplane::SlotRead> {
        SlotIo::read_slot(self.writer.as_ref(), slot)
    }

    /// Drains the process-local dirty-slot set for optional mirrors.
    pub fn take_dirty_slots(&self) -> Vec<usize> {
        self.writer.take_dirty_slots()
    }

    /// Saves a tear-resistant snapshot of this exact generation.
    pub fn save_snapshot(&self, path: &Path) -> PortResult<()> {
        let _local_authority = self.authority_gate.read().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "local SHM authority gate was poisoned",
            )
        })?;
        let _cross_process_authority = self
            .writer
            .acquire_authority_read()
            .map_err(map_dataplane_error)?;
        self.writer
            .validate_authoritative_path()
            .map_err(map_dataplane_error)?;
        self.writer.save_snapshot(path).map_err(map_dataplane_error)
    }
}

/// Runtime-swappable acquisition writer whose layout changes are published by
/// staging-file rename under the shared authority lock.
pub struct ShmWriterHandle {
    current: ArcSwapOption<ShmWriterGeneration>,
    config: ShmRuntimeConfig,
    authority_gate: Arc<RwLock<()>>,
    observer: Option<Arc<dyn AcquisitionCommitObserver>>,
}

impl std::fmt::Debug for ShmWriterHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShmWriterHandle")
            .field("path", &self.config.path)
            .field("max_slots", &self.config.max_slots)
            .field("available", &self.current.load().is_some())
            .finish()
    }
}

impl ShmWriterHandle {
    /// Creates an unavailable handle for fail-closed composition tests and
    /// delayed IO startup. Production acquisition must not begin until a
    /// generation is published.
    #[must_use]
    pub fn empty(config: ShmRuntimeConfig) -> Self {
        Self {
            current: ArcSwapOption::empty(),
            config,
            authority_gate: Arc::new(RwLock::new(())),
            observer: None,
        }
    }

    /// Creates and atomically publishes the initial generation.
    ///
    /// When `snapshot` is present, restoration is accepted only when the
    /// snapshot carries the exact manifest hash and slot count. A topology
    /// change never reinterprets old slot positions as new physical points.
    pub fn create_published(
        config: ShmRuntimeConfig,
        manifest: Arc<ChannelPointManifest>,
        snapshot: Option<&Path>,
    ) -> PortResult<Self> {
        Self::create_published_internal(config, manifest, snapshot, None, 0)
    }

    /// Creates the initial generation as one member of a coordinated physical
    /// topology publication.
    pub fn create_published_at_epoch(
        config: ShmRuntimeConfig,
        manifest: Arc<ChannelPointManifest>,
        snapshot: Option<&Path>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        validate_topology_publication_epoch(publication_epoch)?;
        Self::create_published_internal(config, manifest, snapshot, None, publication_epoch)
    }

    /// Creates the initial generation with a post-commit observer such as the
    /// PointWatch publisher.
    pub fn create_published_with_observer(
        config: ShmRuntimeConfig,
        manifest: Arc<ChannelPointManifest>,
        snapshot: Option<&Path>,
        observer: Option<Arc<dyn AcquisitionCommitObserver>>,
    ) -> PortResult<Self> {
        Self::create_published_internal(config, manifest, snapshot, observer, 0)
    }

    /// Creates the initial generation with both a post-commit observer and a
    /// cross-plane publication identity.
    pub fn create_published_with_observer_at_epoch(
        config: ShmRuntimeConfig,
        manifest: Arc<ChannelPointManifest>,
        snapshot: Option<&Path>,
        observer: Option<Arc<dyn AcquisitionCommitObserver>>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        validate_topology_publication_epoch(publication_epoch)?;
        Self::create_published_internal(config, manifest, snapshot, observer, publication_epoch)
    }

    fn create_published_internal(
        config: ShmRuntimeConfig,
        manifest: Arc<ChannelPointManifest>,
        snapshot: Option<&Path>,
        observer: Option<Arc<dyn AcquisitionCommitObserver>>,
        publication_epoch: u64,
    ) -> PortResult<Self> {
        validate_capacity(&config, &manifest)?;
        let authority_gate = Arc::new(RwLock::new(()));
        let generation = publish_generation(
            &config,
            manifest,
            snapshot,
            Arc::clone(&authority_gate),
            observer.clone(),
            None,
            publication_epoch,
        )?;
        Ok(Self {
            current: ArcSwapOption::new(Some(Arc::new(generation))),
            config,
            authority_gate,
            observer,
        })
    }

    /// Loads the current coherent generation.
    #[must_use]
    pub fn generation(&self) -> Option<Arc<ShmWriterGeneration>> {
        self.current.load_full()
    }

    /// Atomically replaces the canonical segment with a fresh manifest.
    pub fn rebuild(&self, manifest: Arc<ChannelPointManifest>) -> PortResult<()> {
        self.rebuild_internal(manifest, 0)
    }

    /// Replaces the canonical point plane as part of one coordinated physical
    /// topology publication.
    pub fn rebuild_for_publication(
        &self,
        manifest: Arc<ChannelPointManifest>,
        publication_epoch: u64,
    ) -> PortResult<()> {
        validate_topology_publication_epoch(publication_epoch)?;
        if self
            .generation()
            .is_some_and(|generation| generation.publication_epoch() == publication_epoch)
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("point SHM publication epoch {publication_epoch} was already used"),
            ));
        }
        self.rebuild_internal(manifest, publication_epoch)
    }

    fn rebuild_internal(
        &self,
        manifest: Arc<ChannelPointManifest>,
        publication_epoch: u64,
    ) -> PortResult<()> {
        validate_capacity(&self.config, &manifest)?;
        let previous = self.current.load_full();
        let generation = publish_generation(
            &self.config,
            manifest,
            None,
            Arc::clone(&self.authority_gate),
            self.observer.clone(),
            previous.as_deref(),
            publication_epoch,
        )?;
        self.current.store(Some(Arc::new(generation)));
        Ok(())
    }

    /// Returns the physical writer configuration.
    #[must_use]
    pub const fn config(&self) -> &ShmRuntimeConfig {
        &self.config
    }

    /// Returns whether a coherent generation is currently published locally.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.current.load().is_some()
    }
}

fn validate_capacity(config: &ShmRuntimeConfig, manifest: &ChannelPointManifest) -> PortResult<()> {
    if manifest.slot_count() <= config.max_slots as usize {
        return Ok(());
    }
    Err(PortError::new(
        PortErrorKind::InvalidData,
        format!(
            "manifest slot count {} exceeds configured max_slots {}",
            manifest.slot_count(),
            config.max_slots
        ),
    ))
}

fn publish_generation(
    config: &ShmRuntimeConfig,
    manifest: Arc<ChannelPointManifest>,
    snapshot: Option<&Path>,
    authority_gate: Arc<RwLock<()>>,
    observer: Option<Arc<dyn AcquisitionCommitObserver>>,
    previous: Option<&ShmWriterGeneration>,
    publication_epoch: u64,
) -> PortResult<ShmWriterGeneration> {
    let _local_authority = authority_gate.write().map_err(|_| {
        PortError::new(
            PortErrorKind::Permanent,
            "local SHM authority gate was poisoned",
        )
    })?;
    let _cross_process_authority =
        AuthorityWriteGuard::acquire(config.path()).map_err(map_dataplane_error)?;
    let staging_sequence = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let staging_path = generation_file_path(config.path(), staging_sequence.max(1));
    let mut cleanup = StagingCleanup(Some(staging_path.clone()));

    let writer = SlotWriter::create_at_epoch(
        &staging_path,
        config.max_slots,
        manifest.slot_count(),
        manifest.layout_hash(),
        publication_epoch,
    )
    .map_err(map_dataplane_error)?;
    if let Some(snapshot_path) = snapshot {
        restore_exact_snapshot(&writer, snapshot_path, &manifest)?;
    }
    writer.flush().map_err(map_dataplane_error)?;
    let discovered_previous = if previous.is_none() {
        SlotWriter::open_canonical_for_replacement(config.path(), &_cross_process_authority)
            .map_err(map_dataplane_error)?
    } else {
        None
    };
    let previous_writer = previous
        .map(|generation| generation.writer.as_ref())
        .or(discovered_previous.as_ref());
    let invalidation = previous_writer
        .map(|writer| {
            writer
                .begin_generation_swap(&_cross_process_authority)
                .map_err(map_dataplane_error)
        })
        .transpose()?;
    commit_generation_swap_locked(&staging_path, config.path(), &_cross_process_authority)
        .map_err(map_dataplane_error)?;
    if let Some(invalidation) = invalidation {
        invalidation.commit();
    }
    cleanup.0 = None;

    let writer = Arc::new(
        SlotWriter::open_existing(config.path(), manifest.slot_count(), manifest.layout_hash())
            .map_err(map_dataplane_error)?,
    );
    ShmWriterGeneration::compose(writer, manifest, Arc::clone(&authority_gate), observer)
}

fn restore_exact_snapshot(
    writer: &SlotWriter,
    snapshot_path: &Path,
    manifest: &ChannelPointManifest,
) -> PortResult<()> {
    let snapshot =
        aether_dataplane::SnapshotImage::load(snapshot_path).map_err(map_dataplane_error)?;
    let header = snapshot.header();
    if header.slot_count as usize != manifest.slot_count()
        || header.routing_hash != manifest.layout_hash()
    {
        return Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "snapshot layout does not match active manifest: snapshot slots={} hash=0x{:016x}, manifest slots={} hash=0x{:016x}",
                header.slot_count,
                header.routing_hash,
                manifest.slot_count(),
                manifest.layout_hash()
            ),
        ));
    }

    for (slot, value) in snapshot.slots().iter().enumerate() {
        if let Some(value) = value {
            writer.set_direct(slot, value.value, value.raw, value.timestamp_ms);
        }
    }
    Ok(())
}

struct StagingCleanup(Option<PathBuf>);

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
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
