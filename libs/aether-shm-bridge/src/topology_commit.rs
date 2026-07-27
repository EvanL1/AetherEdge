//! Durable witness for one coordinated point/health SHM publication.

#[cfg(unix)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use aether_dataplane::{AuthorityReadGuard, AuthorityWriteGuard, HeaderSnapshot, SlotReader};
use aether_ports::{PortError, PortErrorKind, PortResult};

use crate::managed::map_dataplane_error;
use crate::{
    ChannelHealthManifest, ChannelPointManifest, ShmChannelHealthWriterHandle, ShmWriterHandle,
};

const COMMIT_MAGIC: [u8; 8] = *b"AETHTP01";
const COMMIT_VERSION: u32 = 1;
const COMMIT_BYTES: usize = 80;
const CHECKSUM_OFFSET: usize = 72;

pub(crate) fn validate_topology_publication_epoch(publication_epoch: u64) -> PortResult<()> {
    if publication_epoch == 0 || publication_epoch == u64::MAX {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "coordinated SHM topology publication requires a non-zero, non-reserved epoch",
        ));
    }
    Ok(())
}

/// Immutable identities proving that two canonical SHM files were published
/// by one completed IO topology transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopologyPublicationCommit {
    publication_epoch: u64,
    point_layout_hash: u64,
    point_slot_count: u64,
    point_writer_generation: u64,
    health_layout_hash: u64,
    health_slot_count: u64,
    health_writer_generation: u64,
}

impl TopologyPublicationCommit {
    fn from_readers(
        publication_epoch: u64,
        point_reader: &SlotReader,
        health_reader: &SlotReader,
    ) -> PortResult<Self> {
        let point = point_reader.header();
        let health = health_reader.header();
        validate_header_epoch(
            "point",
            point,
            point_reader.publication_epoch(),
            publication_epoch,
        )?;
        validate_header_epoch(
            "channel-health",
            health,
            health_reader.publication_epoch(),
            publication_epoch,
        )?;
        Ok(Self {
            publication_epoch,
            point_layout_hash: point.routing_hash,
            point_slot_count: u64::from(point.slot_count),
            point_writer_generation: point.writer_generation,
            health_layout_hash: health.routing_hash,
            health_slot_count: u64::from(health.slot_count),
            health_writer_generation: health.writer_generation,
        })
    }

    /// Returns the common physical publication identity.
    #[must_use]
    pub const fn publication_epoch(self) -> u64 {
        self.publication_epoch
    }

    /// Returns the stable point-plane writer generation authorized by this
    /// publication.
    #[must_use]
    pub const fn point_writer_generation(self) -> u64 {
        self.point_writer_generation
    }

    /// Returns the stable health-plane writer generation authorized by this
    /// publication.
    #[must_use]
    pub const fn health_writer_generation(self) -> u64 {
        self.health_writer_generation
    }

    fn matches_readers(self, point_reader: &SlotReader, health_reader: &SlotReader) -> bool {
        let point = point_reader.header();
        let health = health_reader.header();
        self.publication_epoch != 0
            && point_reader.publication_epoch() == self.publication_epoch
            && health_reader.publication_epoch() == self.publication_epoch
            && point.routing_hash == self.point_layout_hash
            && u64::from(point.slot_count) == self.point_slot_count
            && point.writer_generation == self.point_writer_generation
            && health.routing_hash == self.health_layout_hash
            && u64::from(health.slot_count) == self.health_slot_count
            && health.writer_generation == self.health_writer_generation
    }

    fn encode(self) -> [u8; COMMIT_BYTES] {
        let mut bytes = [0_u8; COMMIT_BYTES];
        bytes[0..8].copy_from_slice(&COMMIT_MAGIC);
        bytes[8..12].copy_from_slice(&COMMIT_VERSION.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.publication_epoch.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.point_layout_hash.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.point_slot_count.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.point_writer_generation.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.health_layout_hash.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.health_slot_count.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.health_writer_generation.to_le_bytes());
        let checksum = checksum(&bytes[..CHECKSUM_OFFSET]);
        bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> PortResult<Self> {
        if bytes.len() != COMMIT_BYTES {
            return Err(transition_error(format!(
                "SHM topology commit has invalid length {}; expected {COMMIT_BYTES}",
                bytes.len()
            )));
        }
        if bytes[0..8] != COMMIT_MAGIC {
            return Err(transition_error("SHM topology commit magic is invalid"));
        }
        let version = read_u32(bytes, 8)?;
        if version != COMMIT_VERSION {
            return Err(transition_error(format!(
                "SHM topology commit version {version} is unsupported"
            )));
        }
        let stored_checksum = read_u32(bytes, CHECKSUM_OFFSET)?;
        let observed_checksum = checksum(&bytes[..CHECKSUM_OFFSET]);
        if stored_checksum != observed_checksum {
            return Err(transition_error("SHM topology commit checksum is invalid"));
        }
        let commit = Self {
            publication_epoch: read_u64(bytes, 16)?,
            point_layout_hash: read_u64(bytes, 24)?,
            point_slot_count: read_u64(bytes, 32)?,
            point_writer_generation: read_u64(bytes, 40)?,
            health_layout_hash: read_u64(bytes, 48)?,
            health_slot_count: read_u64(bytes, 56)?,
            health_writer_generation: read_u64(bytes, 64)?,
        };
        if commit.publication_epoch == 0 || commit.publication_epoch == u64::MAX {
            return Err(transition_error(
                "SHM topology commit cannot authorize a reserved publication epoch",
            ));
        }
        Ok(commit)
    }
}

/// Returns the deterministic witness path paired with a canonical point SHM
/// path.
#[must_use]
pub fn topology_commit_path_from_shm(point_path: &Path) -> PathBuf {
    let file_name = point_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("aether-live-state");
    point_path.with_file_name(format!("{file_name}.topology-commit"))
}

/// Exclusive composition-level lease retained across both plane replacements
/// and the final witness publication.
pub struct TopologyPublicationGuard {
    point_path: PathBuf,
    allocated: Option<(PathBuf, u64)>,
    _authority: AuthorityWriteGuard,
}

impl TopologyPublicationGuard {
    /// Allocates the next epoch while retaining the cross-plane publication
    /// authority.
    ///
    /// The floor includes the durable witness and both canonical plane
    /// headers. A restart after a partial publication therefore advances past
    /// every epoch that another process could still observe.
    pub fn next_publication_epoch(&mut self, health_path: &Path) -> PortResult<u64> {
        validate_plane_paths(&self.point_path, health_path)?;
        if let Some((allocated_health_path, epoch)) = &self.allocated {
            if allocated_health_path == health_path {
                return Ok(*epoch);
            }
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "one SHM topology publication guard cannot bind two health-plane paths",
            ));
        }
        let (_first, _second) = acquire_authority_pair(&self.point_path, health_path)?;
        let mut floor = observed_plane_epoch(&self.point_path)?
            .unwrap_or_default()
            .max(observed_plane_epoch(health_path)?.unwrap_or_default());
        if let Ok(commit) = read_topology_publication_commit(&self.point_path) {
            floor = floor.max(commit.publication_epoch());
        }
        let next = floor.checked_add(1).ok_or_else(|| {
            PortError::new(
                PortErrorKind::InvalidData,
                "coordinated SHM topology publication epoch space is exhausted",
            )
        })?;
        validate_topology_publication_epoch(next)?;
        self.allocated = Some((health_path.to_path_buf(), next));
        Ok(next)
    }

    /// Commits the witness for the exact point path guarded by this lease.
    pub fn commit(
        self,
        health_path: &Path,
        publication_epoch: u64,
    ) -> PortResult<TopologyPublicationCommit> {
        validate_plane_paths(&self.point_path, health_path)?;
        if let Some((allocated_health_path, allocated_epoch)) = &self.allocated
            && (allocated_health_path != health_path || *allocated_epoch != publication_epoch)
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "SHM topology commit does not match the path and epoch allocated by its guard",
            ));
        }
        commit_topology_publication_under_guard(&self.point_path, health_path, publication_epoch)
    }
}

/// Begins one cross-plane publication transaction before either canonical
/// SHM path is replaced.
pub fn begin_topology_publication(point_path: &Path) -> PortResult<TopologyPublicationGuard> {
    let authority = AuthorityWriteGuard::acquire(&topology_commit_path_from_shm(point_path))
        .map_err(map_dataplane_error)?;
    Ok(TopologyPublicationGuard {
        point_path: point_path.to_path_buf(),
        allocated: None,
        _authority: authority,
    })
}

/// Rebuilds both physical planes under one composition-level transaction and
/// publishes their commit witness as the final linearization point.
pub fn publish_topology_generation(
    point_writer: &ShmWriterHandle,
    health_writer: &ShmChannelHealthWriterHandle,
    point_manifest: std::sync::Arc<ChannelPointManifest>,
    health_manifest: std::sync::Arc<ChannelHealthManifest>,
) -> PortResult<TopologyPublicationCommit> {
    let mut publication = begin_topology_publication(point_writer.config().path())?;
    let publication_epoch = publication.next_publication_epoch(health_writer.path())?;
    point_writer.rebuild_for_publication(point_manifest, publication_epoch)?;
    health_writer.rebuild_for_publication(health_manifest, publication_epoch)?;
    publication.commit(health_writer.path(), publication_epoch)
}

/// Atomically publishes a witness after both canonical planes have been
/// renamed and reopened successfully.
pub fn commit_topology_publication(
    point_path: &Path,
    health_path: &Path,
    publication_epoch: u64,
) -> PortResult<TopologyPublicationCommit> {
    let publication = begin_topology_publication(point_path)?;
    publication.commit(health_path, publication_epoch)
}

fn commit_topology_publication_under_guard(
    point_path: &Path,
    health_path: &Path,
    publication_epoch: u64,
) -> PortResult<TopologyPublicationCommit> {
    validate_plane_paths(point_path, health_path)?;
    validate_topology_publication_epoch(publication_epoch)?;
    if let Ok(existing) = read_topology_publication_commit(point_path)
        && existing.publication_epoch() >= publication_epoch
    {
        return Err(transition_error(format!(
            "refusing to reuse or roll back durable SHM topology publication epoch from {} to {publication_epoch}",
            existing.publication_epoch()
        )));
    }
    let (_first, _second) = acquire_authority_pair(point_path, health_path)?;
    let point = SlotReader::open(point_path).map_err(map_dataplane_error)?;
    let health = SlotReader::open(health_path).map_err(map_dataplane_error)?;
    let commit = TopologyPublicationCommit::from_readers(publication_epoch, &point, &health)?;

    let canonical_path = topology_commit_path_from_shm(point_path);
    let parent = canonical_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        unavailable_error(format!("create SHM topology commit directory: {error}"))
    })?;
    let file_name = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("topology-commit");
    let staging_path = canonical_path.with_file_name(format!(".{file_name}.staging"));
    let mut cleanup = CommitStagingCleanup(Some(staging_path.clone()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging_path)
        .map_err(|error| unavailable_error(format!("create SHM topology commit: {error}")))?;
    file.write_all(&commit.encode())
        .and_then(|()| file.sync_all())
        .map_err(|error| unavailable_error(format!("flush SHM topology commit: {error}")))?;
    std::fs::rename(&staging_path, &canonical_path)
        .map_err(|error| unavailable_error(format!("publish SHM topology commit: {error}")))?;
    cleanup.0 = None;
    sync_parent_directory(parent)?;

    let observed = read_topology_publication_commit(point_path)?;
    if observed != commit {
        return Err(transition_error(
            "published SHM topology commit could not be revalidated",
        ));
    }
    Ok(commit)
}

fn validate_plane_paths(point_path: &Path, health_path: &Path) -> PortResult<()> {
    if point_path == health_path {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "point and channel-health SHM paths must be distinct",
        ));
    }
    if topology_commit_path_from_shm(point_path) == health_path {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "channel-health SHM path must not alias the topology commit witness",
        ));
    }
    Ok(())
}

fn observed_plane_epoch(path: &Path) -> PortResult<Option<u64>> {
    match std::fs::metadata(path) {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(unavailable_error(format!(
                "inspect SHM plane before epoch allocation at {path:?}: {error}"
            )));
        },
    }
    let reader = SlotReader::open(path).map_err(map_dataplane_error)?;
    let epoch = reader.publication_epoch();
    if epoch == 0 {
        return Ok(None);
    }
    validate_topology_publication_epoch(epoch)?;
    Ok(Some(epoch))
}

/// Reads and validates the versioned witness without interpreting desired
/// topology. Callers validate it against locked canonical SHM headers.
pub fn read_topology_publication_commit(
    point_path: &Path,
) -> PortResult<TopologyPublicationCommit> {
    let path = topology_commit_path_from_shm(point_path);
    let bytes = std::fs::read(&path).map_err(|error| {
        transition_error(format!("read SHM topology commit at {path:?}: {error}"))
    })?;
    TopologyPublicationCommit::decode(&bytes)
}

/// Proves that the locked canonical planes match their latest commit witness
/// and the composition-provided manifests.
pub fn validate_topology_publication(
    point_path: &Path,
    health_path: &Path,
    expected_point_hash: u64,
    expected_point_slots: usize,
    expected_health_hash: u64,
    expected_health_slots: usize,
) -> PortResult<TopologyPublicationCommit> {
    let _topology = acquire_topology_authority(point_path)?;
    let (_first, _second) = acquire_authority_pair(point_path, health_path)?;
    validate_topology_publication_locked(
        point_path,
        health_path,
        expected_point_hash,
        expected_point_slots,
        expected_health_hash,
        expected_health_slots,
    )
}

pub(crate) fn acquire_topology_authority(point_path: &Path) -> PortResult<AuthorityReadGuard> {
    AuthorityReadGuard::acquire(&topology_commit_path_from_shm(point_path))
        .map_err(map_dataplane_error)
}

pub(crate) fn validate_topology_publication_locked(
    point_path: &Path,
    health_path: &Path,
    expected_point_hash: u64,
    expected_point_slots: usize,
    expected_health_hash: u64,
    expected_health_slots: usize,
) -> PortResult<TopologyPublicationCommit> {
    let point = SlotReader::open(point_path).map_err(map_dataplane_error)?;
    let health = SlotReader::open(health_path).map_err(map_dataplane_error)?;
    let point_header = point.header();
    let health_header = health.header();
    if point_header.routing_hash != expected_point_hash
        || point_header.slot_count as usize != expected_point_slots
        || health_header.routing_hash != expected_health_hash
        || health_header.slot_count as usize != expected_health_slots
    {
        return Err(transition_error(
            "canonical SHM planes do not match the expected topology manifests",
        ));
    }
    let commit = read_topology_publication_commit(point_path)?;
    if !commit.matches_readers(&point, &health) {
        return Err(transition_error(
            "canonical SHM planes do not match the committed publication witness",
        ));
    }
    Ok(commit)
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

fn validate_header_epoch(
    label: &str,
    header: HeaderSnapshot,
    observed_epoch: u64,
    publication_epoch: u64,
) -> PortResult<()> {
    if observed_epoch != publication_epoch {
        return Err(transition_error(format!(
            "{label} SHM publication epoch {} does not match requested epoch {publication_epoch}",
            observed_epoch
        )));
    }
    if header.writer_generation == 0 || header.writer_generation & 1 != 0 {
        return Err(transition_error(format!(
            "{label} SHM writer generation {} is not stable",
            header.writer_generation
        )));
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> PortResult<u32> {
    let raw: [u8; 4] = bytes[offset..offset + 4]
        .try_into()
        .map_err(|_| transition_error("SHM topology commit contains a malformed u32"))?;
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> PortResult<u64> {
    let raw: [u8; 8] = bytes[offset..offset + 8]
        .try_into()
        .map_err(|_| transition_error("SHM topology commit contains a malformed u64"))?;
    Ok(u64::from_le_bytes(raw))
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> PortResult<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| unavailable_error(format!("flush SHM topology commit directory: {error}")))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> PortResult<()> {
    Ok(())
}

fn transition_error(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Conflict, message)
}

fn unavailable_error(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

struct CommitStagingCleanup(Option<PathBuf>);

impl Drop for CommitStagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
