//! Coherent read-side publication of the point and channel-health SHM planes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aether_dataplane::AuthorityReadGuard;
use aether_ports::{PortError, PortErrorKind, PortResult};
use arc_swap::ArcSwap;

use crate::managed::map_dataplane_error;
use crate::topology_commit::{
    TopologyPublicationCommit, acquire_topology_authority, validate_topology_publication_locked,
};
use crate::{
    ChannelHealthManifest, ChannelPointManifest, ReconnectingSlotSource, ShmChannelHealthReader,
    ShmClientConfig,
};

/// One immutable point/health topology generation for read-only consumers.
///
/// The generation deliberately contains both planes. A consumer may retain
/// the returned `Arc` for a whole query or scheduler pass and cannot observe a
/// point manifest from one publication with a health manifest from another.
pub struct ShmReadTopologyGeneration {
    point_path: PathBuf,
    health_path: PathBuf,
    point_source: Arc<ReconnectingSlotSource>,
    point_manifest: Arc<ChannelPointManifest>,
    channel_health: Arc<ShmChannelHealthReader>,
    health_manifest: Arc<ChannelHealthManifest>,
    point_writer_generation: AtomicU64,
    health_writer_generation: AtomicU64,
    publication_epoch: AtomicU64,
}

impl std::fmt::Debug for ShmReadTopologyGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShmReadTopologyGeneration")
            .field("point_layout_hash", &self.point_manifest.layout_hash())
            .field("point_slot_count", &self.point_manifest.slot_count())
            .field("health_layout_hash", &self.health_manifest.layout_hash())
            .field("health_slot_count", &self.health_manifest.slot_count())
            .field(
                "publication_epoch",
                &self.publication_epoch.load(Ordering::Acquire),
            )
            .field(
                "point_writer_generation",
                &self.point_writer_generation.load(Ordering::Acquire),
            )
            .field(
                "health_writer_generation",
                &self.health_writer_generation.load(Ordering::Acquire),
            )
            .finish()
    }
}

impl ShmReadTopologyGeneration {
    /// Builds a lazy generation so a service can start before IO.
    ///
    /// This validates the composition-provided hashes but does not open either
    /// SHM path. Reads remain retryably unavailable until IO publishes them.
    pub fn new_lazy(
        point_config: ShmClientConfig,
        health_config: ShmClientConfig,
        point_manifest: Arc<ChannelPointManifest>,
        health_manifest: Arc<ChannelHealthManifest>,
    ) -> PortResult<Self> {
        if point_config.path() == health_config.path() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "point and channel-health SHM paths must be distinct",
            ));
        }
        validate_config_hash(
            "point",
            point_config.expected_layout_hash(),
            point_manifest.layout_hash(),
        )?;
        validate_config_hash(
            "channel-health",
            health_config.expected_layout_hash(),
            health_manifest.layout_hash(),
        )?;
        let point_path = point_config.path().to_path_buf();
        let health_path = health_config.path().to_path_buf();
        let point_source = Arc::new(ReconnectingSlotSource::new(point_config));
        point_source.require_coordinated_publication();
        let channel_health = Arc::new(ShmChannelHealthReader::new(
            health_config,
            Arc::clone(&health_manifest),
        ));
        channel_health.require_coordinated_publication();
        Ok(Self {
            point_path,
            health_path,
            point_source,
            point_manifest,
            channel_health,
            health_manifest,
            point_writer_generation: AtomicU64::new(0),
            health_writer_generation: AtomicU64::new(0),
            publication_epoch: AtomicU64::new(0),
        })
    }

    /// Opens and validates both physical planes before returning a candidate.
    pub fn open(
        point_config: ShmClientConfig,
        health_config: ShmClientConfig,
        point_manifest: Arc<ChannelPointManifest>,
        health_manifest: Arc<ChannelHealthManifest>,
    ) -> PortResult<Self> {
        let generation =
            Self::new_lazy(point_config, health_config, point_manifest, health_manifest)?;
        let publication = generation.validate_current_publication()?;
        generation.pin_publication(publication)?;
        Ok(generation)
    }

    /// Revalidates both layouts without requiring a fresh heartbeat.
    pub fn validate_layouts(&self) -> PortResult<()> {
        self.with_validated_authority(|| ())
    }

    /// Runs one publication while both canonical planes are validation-locked.
    ///
    /// IO needs exclusive leases to invalidate and replace either generation,
    /// so a service-local `ArcSwap` performed in this closure cannot race the
    /// final validation and publish a mapping that was already superseded.
    pub fn with_validated_authority<T>(&self, publish: impl FnOnce() -> T) -> PortResult<T> {
        let _topology = acquire_topology_authority(&self.point_path)?;
        let (_first, _second) = acquire_authority_pair(&self.point_path, &self.health_path)?;
        let observed = self.validate_locked_publication()?;
        let observed_epoch = observed.publication_epoch();
        let pinned_epoch = self.publication_epoch.load(Ordering::Acquire);
        let pinned_point_writer = self.point_writer_generation.load(Ordering::Acquire);
        let pinned_health_writer = self.health_writer_generation.load(Ordering::Acquire);
        if pinned_epoch != 0
            && (observed_epoch != pinned_epoch
                || observed.point_writer_generation() != pinned_point_writer
                || observed.health_writer_generation() != pinned_health_writer)
        {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                format!(
                    "SHM topology publication changed from epoch/writers \
                     {pinned_epoch}/{pinned_point_writer}/{pinned_health_writer} to \
                     {observed_epoch}/{}/{}",
                    observed.point_writer_generation(),
                    observed.health_writer_generation(),
                ),
            ));
        }
        self.pin_publication(observed)?;
        Ok(publish())
    }

    fn validate_current_publication(&self) -> PortResult<TopologyPublicationCommit> {
        let _topology = acquire_topology_authority(&self.point_path)?;
        let (_first, _second) = acquire_authority_pair(&self.point_path, &self.health_path)?;
        self.validate_locked_publication()
    }

    fn validate_locked_publication(&self) -> PortResult<TopologyPublicationCommit> {
        self.point_source
            .validate_layout(self.point_manifest.slot_count())?;
        self.channel_health.validate_layout()?;
        validate_topology_publication_locked(
            &self.point_path,
            &self.health_path,
            self.point_manifest.layout_hash(),
            self.point_manifest.slot_count(),
            self.health_manifest.layout_hash(),
            self.health_manifest.slot_count(),
        )
    }

    fn pin_publication(&self, publication: TopologyPublicationCommit) -> PortResult<()> {
        let publication_epoch = publication.publication_epoch();
        self.point_source.accept_publication_identity(
            publication_epoch,
            publication.point_writer_generation(),
        )?;
        self.channel_health.accept_publication_identity(
            publication_epoch,
            publication.health_writer_generation(),
        )?;
        self.point_writer_generation
            .store(publication.point_writer_generation(), Ordering::Release);
        self.health_writer_generation
            .store(publication.health_writer_generation(), Ordering::Release);
        self.publication_epoch
            .store(publication_epoch, Ordering::Release);
        Ok(())
    }

    /// Returns the point-plane source paired with this generation.
    #[must_use]
    pub fn point_source(&self) -> &Arc<ReconnectingSlotSource> {
        &self.point_source
    }

    /// Returns the point manifest paired with this generation.
    #[must_use]
    pub fn point_manifest(&self) -> &Arc<ChannelPointManifest> {
        &self.point_manifest
    }

    /// Returns the channel-health source paired with this generation.
    #[must_use]
    pub fn channel_health(&self) -> &Arc<ShmChannelHealthReader> {
        &self.channel_health
    }

    /// Returns the health manifest paired with this generation.
    #[must_use]
    pub fn health_manifest(&self) -> &Arc<ChannelHealthManifest> {
        &self.health_manifest
    }

    /// Returns the committed IO publication epoch pinned by this read view.
    /// Zero is reserved for compatibility-only uncoordinated fixtures.
    #[must_use]
    pub fn publication_epoch(&self) -> u64 {
        self.publication_epoch.load(Ordering::Acquire)
    }

    /// Returns the point-plane writer generation pinned with this view.
    #[must_use]
    pub fn point_writer_generation(&self) -> u64 {
        self.point_writer_generation.load(Ordering::Acquire)
    }

    /// Returns the channel-health writer generation pinned with this view.
    #[must_use]
    pub fn health_writer_generation(&self) -> u64 {
        self.health_writer_generation.load(Ordering::Acquire)
    }
}

/// Atomically replaceable read-side topology generation.
pub struct ShmReadTopologyHandle {
    current: ArcSwap<ShmReadTopologyGeneration>,
}

impl ShmReadTopologyHandle {
    /// Creates a handle, typically with a lazy startup generation.
    #[must_use]
    pub fn new(initial: Arc<ShmReadTopologyGeneration>) -> Self {
        Self {
            current: ArcSwap::new(initial),
        }
    }

    /// Pins one coherent generation for an entire logical read operation.
    #[must_use]
    pub fn load(&self) -> Arc<ShmReadTopologyGeneration> {
        self.current.load_full()
    }

    /// Validates and publishes a complete replacement generation.
    pub fn publish(&self, candidate: Arc<ShmReadTopologyGeneration>) -> PortResult<()> {
        candidate.with_validated_authority(|| self.current.store(candidate.clone()))
    }
}

fn acquire_authority_pair(
    point_path: &Path,
    health_path: &Path,
) -> PortResult<(AuthorityReadGuard, AuthorityReadGuard)> {
    let (first_path, second_path) = if point_path <= health_path {
        (point_path, health_path)
    } else {
        (health_path, point_path)
    };
    let first = AuthorityReadGuard::acquire(first_path).map_err(map_dataplane_error)?;
    let second = AuthorityReadGuard::acquire(second_path).map_err(map_dataplane_error)?;
    Ok((first, second))
}

fn validate_config_hash(label: &str, configured: u64, manifest: u64) -> PortResult<()> {
    if configured == manifest {
        return Ok(());
    }
    Err(PortError::new(
        PortErrorKind::InvalidData,
        format!(
            "{label} SHM client hash 0x{configured:016x} does not match manifest hash 0x{manifest:016x}"
        ),
    ))
}
