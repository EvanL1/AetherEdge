//! Self-healing read-only SHM file client.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_dataplane::{DataplaneError, SlotIo, SlotReader};
use aether_ports::{PortError, PortErrorKind, PortResult};

use crate::{SlotSnapshot, SlotSource};

/// Recovery and validation policy for a read-only SHM client.
#[derive(Debug, Clone)]
pub struct ShmClientConfig {
    path: PathBuf,
    expected_layout_hash: u64,
    identity_check_interval: Duration,
    writer_stale_after: Duration,
    reconnect_interval: Duration,
}

impl ShmClientConfig {
    /// Creates a client policy with mandatory manifest validation.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, expected_layout_hash: u64) -> Self {
        Self {
            path: path.into(),
            expected_layout_hash,
            identity_check_interval: Duration::from_millis(250),
            writer_stale_after: Duration::from_secs(30),
            reconnect_interval: Duration::from_millis(250),
        }
    }

    /// Changes how often the canonical path is checked for an inode swap.
    #[must_use]
    pub const fn with_identity_check_interval(mut self, interval: Duration) -> Self {
        self.identity_check_interval = interval;
        self
    }

    /// Changes the maximum accepted writer-heartbeat age.
    #[must_use]
    pub const fn with_writer_stale_after(mut self, timeout: Duration) -> Self {
        self.writer_stale_after = timeout;
        self
    }

    /// Changes the retry delay after the writer or its file is unavailable.
    #[must_use]
    pub const fn with_reconnect_interval(mut self, interval: Duration) -> Self {
        self.reconnect_interval = interval;
        self
    }

    /// Returns the canonical SHM path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
}

impl FileIdentity {
    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
            })
        }
        #[cfg(not(unix))]
        {
            let modified = metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            Ok(Self {
                device: 0,
                inode: modified,
                length: metadata.len(),
            })
        }
    }
}

struct OpenReader {
    reader: SlotReader,
    identity: FileIdentity,
    generation: u64,
}

#[derive(Default)]
struct ClientState {
    opened: Option<OpenReader>,
    next_identity_check: Option<Instant>,
    next_open_attempt: Option<Instant>,
    last_open_error: Option<PortError>,
}

/// Read-only slot source that lazily connects and automatically reopens after
/// writer restart, generation change, or atomic canonical-path replacement.
pub struct ReconnectingSlotSource {
    config: ShmClientConfig,
    state: RwLock<ClientState>,
}

impl ReconnectingSlotSource {
    /// Creates a lazy client. A missing writer does not prevent service startup;
    /// reads return a retryable unavailable error until the file appears.
    #[must_use]
    pub fn new(config: ShmClientConfig) -> Self {
        Self {
            config,
            state: RwLock::new(ClientState::default()),
        }
    }

    fn ensure_current(&self) -> PortResult<()> {
        let now = Instant::now();
        {
            let state = self.read_state()?;
            if let Some(opened) = &state.opened {
                let generation = opened.reader.generation();
                let identity_due = state
                    .next_identity_check
                    .is_none_or(|deadline| now >= deadline);
                if generation == opened.generation && generation & 1 == 0 && !identity_due {
                    return Ok(());
                }
            } else if state
                .next_open_attempt
                .is_some_and(|deadline| now < deadline)
            {
                return Err(state.last_open_error.clone().unwrap_or_else(|| {
                    PortError::new(PortErrorKind::Unavailable, "SHM writer is unavailable")
                }));
            }
        }

        let mut state = self.write_state()?;
        if let Some(opened) = &state.opened {
            let generation = opened.reader.generation();
            let identity_due = state
                .next_identity_check
                .is_none_or(|deadline| now >= deadline);
            if generation == opened.generation && generation & 1 == 0 && !identity_due {
                return Ok(());
            }

            if generation == opened.generation && generation & 1 == 0 {
                match FileIdentity::read(&self.config.path) {
                    Ok(identity) if identity == opened.identity => {
                        state.next_identity_check = Some(now + self.config.identity_check_interval);
                        return Ok(());
                    },
                    Ok(_) => {},
                    Err(error) => {
                        let mapped = map_io_error("stat canonical SHM path", error);
                        state.last_open_error = Some(mapped.clone());
                        state.next_identity_check = Some(now + self.config.reconnect_interval);
                        return Err(mapped);
                    },
                }
            }
        } else if state
            .next_open_attempt
            .is_some_and(|deadline| now < deadline)
        {
            return Err(state.last_open_error.clone().unwrap_or_else(|| {
                PortError::new(PortErrorKind::Unavailable, "SHM writer is unavailable")
            }));
        }

        match self.open_current() {
            Ok(opened) => {
                state.opened = Some(opened);
                state.next_identity_check = Some(now + self.config.identity_check_interval);
                state.next_open_attempt = None;
                state.last_open_error = None;
                Ok(())
            },
            Err(error) => {
                state.opened = None;
                state.next_open_attempt = Some(now + self.config.reconnect_interval);
                state.last_open_error = Some(error.clone());
                Err(error)
            },
        }
    }

    fn open_current(&self) -> PortResult<OpenReader> {
        let reader = SlotReader::open(&self.config.path).map_err(map_dataplane_error)?;
        let header = reader.header();
        if header.routing_hash != self.config.expected_layout_hash {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "SHM manifest mismatch at {:?}: expected 0x{:016X}, got 0x{:016X}",
                    self.config.path, self.config.expected_layout_hash, header.routing_hash
                ),
            ));
        }
        if header.writer_generation == 0 || header.writer_generation & 1 != 0 {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                format!(
                    "SHM writer generation {} is not stable",
                    header.writer_generation
                ),
            ));
        }
        let identity = FileIdentity::read(&self.config.path)
            .map_err(|error| map_io_error("stat opened SHM path", error))?;
        Ok(OpenReader {
            reader,
            identity,
            generation: header.writer_generation,
        })
    }

    fn with_reader<T>(&self, read: impl FnOnce(&SlotReader) -> PortResult<T>) -> PortResult<T> {
        self.ensure_current()?;
        let state = self.read_state()?;
        let opened = state.opened.as_ref().ok_or_else(|| {
            PortError::new(PortErrorKind::Unavailable, "SHM reader is not connected")
        })?;
        validate_writer_freshness(&opened.reader, self.config.writer_stale_after)?;
        read(&opened.reader)
    }

    fn read_state(&self) -> PortResult<std::sync::RwLockReadGuard<'_, ClientState>> {
        self.state.read().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "SHM client read lock was poisoned",
            )
        })
    }

    fn write_state(&self) -> PortResult<std::sync::RwLockWriteGuard<'_, ClientState>> {
        self.state.write().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "SHM client write lock was poisoned",
            )
        })
    }
}

impl SlotSource for ReconnectingSlotSource {
    fn slot_count(&self) -> PortResult<usize> {
        self.with_reader(|reader| Ok(reader.slot_count()))
    }

    fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>> {
        self.with_reader(|reader| {
            if index >= reader.slot_count() {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!(
                        "slot {index} is outside live slot_count {}",
                        reader.slot_count()
                    ),
                ));
            }
            let slot = SlotIo::read_slot(reader, index).ok_or_else(|| {
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
        })
    }
}

fn validate_writer_freshness(reader: &SlotReader, timeout: Duration) -> PortResult<()> {
    let heartbeat = reader.writer_heartbeat();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let timeout_ms = timeout.as_millis() as u64;
    if heartbeat == 0 || now_ms.saturating_sub(heartbeat) > timeout_ms {
        return Err(PortError::new(
            PortErrorKind::Unavailable,
            format!(
                "SHM writer heartbeat is stale: heartbeat={heartbeat}, timeout_ms={timeout_ms}"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn map_dataplane_error(error: DataplaneError) -> PortError {
    match error {
        DataplaneError::InvalidLayout(message) => {
            PortError::new(PortErrorKind::InvalidData, message)
        },
        DataplaneError::InvalidPath(path) => PortError::new(
            PortErrorKind::Permanent,
            format!("invalid SHM path: {path:?}"),
        ),
        DataplaneError::Io { context, source } => map_io_error(&context, source),
    }
}

fn map_io_error(context: &str, error: std::io::Error) -> PortError {
    let kind = match error.kind() {
        ErrorKind::PermissionDenied | ErrorKind::InvalidInput => PortErrorKind::Permanent,
        ErrorKind::NotFound
        | ErrorKind::Interrupted
        | ErrorKind::WouldBlock
        | ErrorKind::TimedOut => PortErrorKind::Unavailable,
        _ => PortErrorKind::Unavailable,
    };
    PortError::new(kind, format!("{context}: {error}"))
}
