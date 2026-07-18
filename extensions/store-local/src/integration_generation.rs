//! Crash-safe local topology-generation reservations.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aether_domain::{GatewayIdentity, IntegrationId, SnapshotDigest, TopologyGeneration};
use aether_ports::{IntegrationTopologyGenerationStore, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const FILE_SCHEMA: &str = "aether.edge.integration-generations.v1";
const MAX_FILE_BYTES: u64 = 4 * 1_024 * 1_024;
const MAX_ENTRIES: usize = 65_536;

/// Process-exclusive, atomically rewritten topology-generation store.
///
/// The JSON state is edge-local operational metadata, not an AetherContracts
/// public payload. A reservation is returned only after the replacement file
/// and parent directory have been synchronized.
pub struct FileIntegrationTopologyGenerationStore {
    path: PathBuf,
    _lock_file: File,
    entries: Mutex<BTreeMap<ScopeKey, GenerationEntry>>,
}

impl std::fmt::Debug for FileIntegrationTopologyGenerationStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileIntegrationTopologyGenerationStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileIntegrationTopologyGenerationStore {
    /// Opens or creates a process-exclusive generation store.
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|source| storage_error("create generation-store directory", source))?;
        }

        let lock_path = sibling_path(&path, ".lock");
        let lock_file = writable_file(&lock_path, true)?;
        FileExt::try_lock_exclusive(&lock_file).map_err(|source| {
            if source.kind() == std::io::ErrorKind::WouldBlock {
                PortError::new(
                    PortErrorKind::Conflict,
                    "integration generation store is already open",
                )
            } else {
                storage_error("lock integration generation store", source)
            }
        })?;

        let temporary_path = sibling_path(&path, ".replace.tmp");
        match std::fs::remove_file(&temporary_path) {
            Ok(()) => {},
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {},
            Err(source) => {
                return Err(storage_error(
                    "remove stale generation-store replacement",
                    source,
                ));
            },
        }

        let entries = load(&path)?;
        Ok(Self {
            path,
            _lock_file: lock_file,
            entries: Mutex::new(entries),
        })
    }
}

#[async_trait]
impl IntegrationTopologyGenerationStore for FileIntegrationTopologyGenerationStore {
    async fn reserve_generation(
        &self,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
        snapshot_digest: &SnapshotDigest,
    ) -> PortResult<TopologyGeneration> {
        let key = ScopeKey {
            gateway_id: gateway_id.as_str().to_string(),
            integration_id: integration_id.as_str().to_string(),
        };
        let mut current = self
            .entries
            .lock()
            .map_err(|_| crate::lock_error("integration generation store"))?;
        if let Some(entry) = current.get(&key)
            && entry.snapshot_digest == snapshot_digest.as_str()
        {
            return generation(entry.generation);
        }

        let next_generation = current
            .get(&key)
            .map_or(Some(1), |entry| entry.generation.checked_add(1))
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "integration topology generation is exhausted",
                )
            })?;
        let mut next = current.clone();
        next.insert(
            key,
            GenerationEntry {
                generation: next_generation,
                snapshot_digest: snapshot_digest.as_str().to_string(),
            },
        );
        persist(&self.path, &next)?;
        *current = next;
        generation(next_generation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopeKey {
    gateway_id: String,
    integration_id: String,
}

#[derive(Debug, Clone)]
struct GenerationEntry {
    generation: u64,
    snapshot_digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDocument {
    schema: String,
    entries: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    gateway_id: String,
    integration_id: String,
    generation: u64,
    snapshot_digest: String,
}

fn load(path: &Path) -> PortResult<BTreeMap<ScopeKey, GenerationEntry>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        },
        Err(source) => return Err(storage_error("open integration generation state", source)),
    };
    let length = file
        .metadata()
        .map_err(|source| storage_error("inspect integration generation state", source))?
        .len();
    if length == 0 || length > MAX_FILE_BYTES {
        return Err(corrupt("integration generation state has an invalid size"));
    }
    let capacity = usize::try_from(length)
        .map_err(|_| corrupt("integration generation state size cannot be represented"))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|source| storage_error("read integration generation state", source))?;
    let document: FileDocument = serde_json::from_slice(&bytes)
        .map_err(|_source| corrupt("integration generation state is not closed valid JSON"))?;
    if document.schema != FILE_SCHEMA || document.entries.len() > MAX_ENTRIES {
        return Err(corrupt(
            "integration generation state has an unsupported schema or entry count",
        ));
    }

    let mut entries = BTreeMap::new();
    for entry in document.entries {
        GatewayIdentity::new(&entry.gateway_id)
            .map_err(|_source| corrupt("generation state contains an invalid gateway identity"))?;
        IntegrationId::new(&entry.integration_id).map_err(|_source| {
            corrupt("generation state contains an invalid integration identity")
        })?;
        SnapshotDigest::new(&entry.snapshot_digest)
            .map_err(|_source| corrupt("generation state contains an invalid topology digest"))?;
        generation(entry.generation)?;
        let key = ScopeKey {
            gateway_id: entry.gateway_id,
            integration_id: entry.integration_id,
        };
        if entries
            .insert(
                key,
                GenerationEntry {
                    generation: entry.generation,
                    snapshot_digest: entry.snapshot_digest,
                },
            )
            .is_some()
        {
            return Err(corrupt(
                "generation state contains a duplicate integration scope",
            ));
        }
    }
    Ok(entries)
}

fn persist(path: &Path, entries: &BTreeMap<ScopeKey, GenerationEntry>) -> PortResult<()> {
    let document = FileDocument {
        schema: FILE_SCHEMA.to_string(),
        entries: entries
            .iter()
            .map(|(key, entry)| FileEntry {
                gateway_id: key.gateway_id.clone(),
                integration_id: key.integration_id.clone(),
                generation: entry.generation,
                snapshot_digest: entry.snapshot_digest.clone(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&document)
        .map_err(|_source| corrupt("cannot encode integration generation state"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(PortError::new(
            PortErrorKind::Permanent,
            "integration generation state exceeds its 4 MiB safety bound",
        ));
    }

    let temporary_path = sibling_path(path, ".replace.tmp");
    let mut temporary = create_new_file(&temporary_path)?;
    let write_result = temporary
        .write_all(&bytes)
        .and_then(|()| temporary.sync_all())
        .map_err(|source| storage_error("write integration generation replacement", source));
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(error);
    }
    if let Err(source) = std::fs::rename(&temporary_path, path) {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(storage_error(
            "commit integration generation replacement",
            source,
        ));
    }
    sync_parent_directory(path)
}

fn writable_file(path: &Path, create: bool) -> PortResult<File> {
    let mut options = OpenOptions::new();
    options
        .create(create)
        .truncate(false)
        .read(true)
        .write(true);
    set_private_mode(&mut options);
    options
        .open(path)
        .map_err(|source| storage_error("open integration generation lock", source))
}

fn create_new_file(path: &Path) -> PortResult<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    set_private_mode(&mut options);
    options
        .open(path)
        .map_err(|source| storage_error("create integration generation replacement", source))
}

#[cfg(unix)]
fn set_private_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
const fn set_private_mode(_options: &mut OpenOptions) {}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "integration-generations".into(),
        |name| name.to_string_lossy(),
    );
    path.with_file_name(format!("{name}{suffix}"))
}

fn sync_parent_directory(path: &Path) -> PortResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| storage_error("sync integration generation directory", source))
}

fn generation(value: u64) -> PortResult<TopologyGeneration> {
    TopologyGeneration::new(value).map_err(|_source| {
        PortError::new(
            PortErrorKind::Permanent,
            "integration generation state contains zero",
        )
    })
}

fn storage_error(operation: &str, source: std::io::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("{operation} failed: {source}"),
    )
}

fn corrupt(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}
