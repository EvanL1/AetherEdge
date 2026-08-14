//! PointWatch wire contract and isolated consumer-side UDS listener.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const POINT_WATCH_FRAME_MAGIC_START: u8 = 0xA5;
const POINT_WATCH_FRAME_MAGIC_END: u8 = 0x5A;
const INITIAL_FRAME_TIMEOUT: Duration = Duration::from_secs(1);
const POINT_WATCH_SOCKET_MODE: u32 = 0o600;

/// Fixed-size PointWatch hint sent after a SHM slot write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointWatchEvent {
    channel_id: u32,
    point_id: u32,
    slot_index: u32,
    point_type: u8,
}

impl PointWatchEvent {
    /// Wire frame size in bytes.
    pub const SIZE: usize = 16;

    /// Creates one wake-up hint after validating the physical slot width.
    ///
    /// SHM remains authoritative for value, raw value, timestamp, and quality.
    pub fn new(
        channel_id: u32,
        kind: PointKind,
        point_id: u32,
        slot_index: usize,
    ) -> PortResult<Self> {
        let slot_index = u32::try_from(slot_index).map_err(|_| {
            PortError::new(
                PortErrorKind::InvalidData,
                format!("PointWatch slot {slot_index} exceeds the u32 wire range"),
            )
        })?;
        Ok(Self {
            channel_id,
            point_id,
            slot_index,
            point_type: point_kind_code(kind),
        })
    }

    /// Decodes the one accepted little-endian wire representation.
    ///
    /// The trailing magic and zero reserved byte prevent another payload from
    /// being interpreted as a valid compact hint.
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> PortResult<Self> {
        if bytes[13] != POINT_WATCH_FRAME_MAGIC_START
            || bytes[14] != 0
            || bytes[15] != POINT_WATCH_FRAME_MAGIC_END
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "PointWatch frame magic or reserved byte is invalid",
            ));
        }
        let event = Self {
            channel_id: u32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4])),
            point_id: u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4])),
            slot_index: u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4])),
            point_type: bytes[12],
        };
        if event.point_kind().is_none() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("PointWatch point kind {} is invalid", event.point_type),
            ));
        }
        Ok(event)
    }

    /// Encodes the stable little-endian wire representation.
    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0_u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.channel_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.point_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.slot_index.to_le_bytes());
        bytes[12] = self.point_type;
        bytes[13] = POINT_WATCH_FRAME_MAGIC_START;
        bytes[15] = POINT_WATCH_FRAME_MAGIC_END;
        bytes
    }

    /// Returns the physical channel id.
    #[must_use]
    pub const fn channel_id(self) -> u32 {
        self.channel_id
    }

    /// Returns the physical point id.
    #[must_use]
    pub const fn point_id(self) -> u32 {
        self.point_id
    }

    /// Returns the point kind, or `None` for a future/unknown wire code.
    #[must_use]
    pub const fn point_kind(self) -> Option<PointKind> {
        match self.point_type {
            0 => Some(PointKind::Telemetry),
            1 => Some(PointKind::Status),
            2 => Some(PointKind::Command),
            3 => Some(PointKind::Action),
            _ => None,
        }
    }

    /// Returns the authoritative SHM slot to re-read.
    #[must_use]
    pub const fn slot_index(self) -> u32 {
        self.slot_index
    }

    /// Returns whether this hint still names the same typed slot in a
    /// consumer's current physical manifest.
    ///
    /// Event payload values are never authoritative. Consumers must call this
    /// before using the slot as a wake-up hint, then re-read SHM from the
    /// pinned current topology generation.
    #[must_use]
    pub fn matches_manifest(self, manifest: &crate::ChannelPointManifest) -> bool {
        let Some(kind) = self.point_kind() else {
            return false;
        };
        manifest
            .slot_for(crate::PhysicalPointAddress::from_raw_ids(
                self.channel_id,
                kind,
                self.point_id,
            ))
            .and_then(|slot| u32::try_from(slot).ok())
            == Some(self.slot_index)
    }
}

const fn point_kind_code(kind: PointKind) -> u8 {
    match kind {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    }
}

/// Derives the default isolated UDS path for an event consumer.
#[must_use]
pub fn point_watch_socket_for_consumer(consumer: &str) -> PathBuf {
    point_watch_socket_from_shm(&crate::default_shm_path(), consumer)
}

/// Derives an isolated UDS path beside a specific SHM segment.
#[must_use]
pub fn point_watch_socket_from_shm(shm_path: &Path, consumer: &str) -> PathBuf {
    shm_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!("aether-point-watch-{consumer}.sock"))
}

/// Bounded UDS listener that turns PointWatch frames into in-process hints.
pub struct PointWatchEventListener {
    socket_path: PathBuf,
    event_tx: mpsc::Sender<PointWatchEvent>,
    shutdown: CancellationToken,
    dropped_count: Arc<AtomicU64>,
    rejected_peers: Arc<AtomicU64>,
    initial_frame_timeouts: Arc<AtomicU64>,
}

/// Listener whose path ownership and permissions were established during
/// fail-fast service composition.
pub struct PreparedPointWatchEventListener {
    listener: UnixListener,
    socket_path: PathBuf,
    event_tx: mpsc::Sender<PointWatchEvent>,
    shutdown: CancellationToken,
    dropped_count: Arc<AtomicU64>,
    rejected_peers: Arc<AtomicU64>,
    initial_frame_timeouts: Arc<AtomicU64>,
    owner_uid: u32,
}

impl Drop for PreparedPointWatchEventListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl PointWatchEventListener {
    /// Creates a listener and its bounded event receiver.
    #[must_use]
    pub fn new(
        socket_path: impl Into<PathBuf>,
        shutdown: CancellationToken,
    ) -> (Self, mpsc::Receiver<PointWatchEvent>) {
        let (event_tx, event_rx) = mpsc::channel(1_024);
        (
            Self {
                socket_path: socket_path.into(),
                event_tx,
                shutdown,
                dropped_count: Arc::new(AtomicU64::new(0)),
                rejected_peers: Arc::new(AtomicU64::new(0)),
                initial_frame_timeouts: Arc::new(AtomicU64::new(0)),
            },
            event_rx,
        )
    }

    /// Returns events dropped due to bounded in-process backpressure.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Returns local producer connections rejected by the peer-UID fence.
    #[must_use]
    pub fn rejected_peer_count(&self) -> u64 {
        self.rejected_peers.load(Ordering::Relaxed)
    }

    /// Returns connections closed before their first complete frame arrived.
    #[must_use]
    pub fn initial_frame_timeout_count(&self) -> u64 {
        self.initial_frame_timeouts.load(Ordering::Relaxed)
    }

    /// Removes a stale socket and binds/secures the listener synchronously.
    /// An occupied live socket fails before a scheduler or other runtime task
    /// can be activated.
    pub fn prepare(self) -> std::io::Result<PreparedPointWatchEventListener> {
        prepare_socket_path(&self.socket_path)?;
        let listener = UnixListener::bind(&self.socket_path)?;
        let owner_uid = match secure_socket(&self.socket_path) {
            Ok(owner_uid) => owner_uid,
            Err(error) => {
                drop(listener);
                let _ = std::fs::remove_file(&self.socket_path);
                return Err(error);
            },
        };
        Ok(PreparedPointWatchEventListener {
            listener,
            socket_path: self.socket_path,
            event_tx: self.event_tx,
            shutdown: self.shutdown,
            dropped_count: self.dropped_count,
            rejected_peers: self.rejected_peers,
            initial_frame_timeouts: self.initial_frame_timeouts,
            owner_uid,
        })
    }

    /// Runs until cancellation, accepting producer reconnects serially.
    pub async fn run(self) -> std::io::Result<()> {
        self.prepare()?.run().await
    }
}

impl PreparedPointWatchEventListener {
    /// Runs an already-bound listener until cancellation.
    pub async fn run(self) -> std::io::Result<()> {
        let _cleanup = SocketCleanup(self.socket_path.clone());

        loop {
            let stream = tokio::select! {
                _ = self.shutdown.cancelled() => break,
                accepted = self.listener.accept() => accepted?.0,
            };
            let peer_uid = match stream.peer_cred() {
                Ok(credentials) => credentials.uid(),
                Err(_) => {
                    self.rejected_peers.fetch_add(1, Ordering::Relaxed);
                    continue;
                },
            };
            if peer_uid != self.owner_uid {
                self.rejected_peers.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if !consume_connection(
                stream,
                &self.event_tx,
                &self.dropped_count,
                &self.initial_frame_timeouts,
                &self.shutdown,
            )
            .await?
            {
                break;
            }
        }
        Ok(())
    }
}

fn secure_socket(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(POINT_WATCH_SOCKET_MODE),
    )?;
    let metadata = std::fs::symlink_metadata(path)?;
    let actual_mode = metadata.permissions().mode() & 0o777;
    if actual_mode != POINT_WATCH_SOCKET_MODE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "PointWatch socket {path:?} mode is {actual_mode:o}, expected {POINT_WATCH_SOCKET_MODE:o}"
            ),
        ));
    }
    Ok(metadata.uid())
}

fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("PointWatch listener already active at {path:?}"),
        ));
    }
    std::fs::remove_file(path)
}

#[cfg(test)]
mod listener_tests {
    use super::*;

    #[tokio::test]
    async fn occupied_socket_is_rejected_during_prepare_not_runtime() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let path = directory.path().join("point-watch.sock");
        let _occupied = UnixListener::bind(&path).expect("occupy PointWatch socket");
        let (listener, _events) = PointWatchEventListener::new(&path, CancellationToken::new());

        let error = match listener.prepare() {
            Ok(_) => panic!("active listener must fail before runtime task activation"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    }
}

async fn consume_connection(
    mut stream: UnixStream,
    event_tx: &mpsc::Sender<PointWatchEvent>,
    dropped_count: &AtomicU64,
    initial_frame_timeouts: &AtomicU64,
    shutdown: &CancellationToken,
) -> std::io::Result<bool> {
    let mut received_frame = false;
    loop {
        let mut bytes = [0_u8; PointWatchEvent::SIZE];
        let read = if received_frame {
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(false),
                read = stream.read_exact(&mut bytes) => read,
            }
        } else {
            let timed_read = tokio::select! {
                _ = shutdown.cancelled() => return Ok(false),
                read = tokio::time::timeout(
                    INITIAL_FRAME_TIMEOUT,
                    stream.read_exact(&mut bytes),
                ) => read,
            };
            match timed_read {
                Ok(read) => read,
                Err(_) => {
                    initial_frame_timeouts.fetch_add(1, Ordering::Relaxed);
                    return Ok(true);
                },
            }
        };
        match read {
            Ok(_) => {
                let event = match PointWatchEvent::from_bytes(&bytes) {
                    Ok(event) => event,
                    Err(_) => {
                        dropped_count.fetch_add(1, Ordering::Relaxed);
                        return Ok(true);
                    },
                };
                received_frame = true;
                match event_tx.try_send(event) {
                    Ok(()) => {},
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        dropped_count.fetch_add(1, Ordering::Relaxed);
                    },
                    Err(mpsc::error::TrySendError::Closed(_)) => return Ok(false),
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(true),
            Err(error) => return Err(error),
        }
    }
}

struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connection_without_a_first_frame_cannot_hold_the_listener_forever() {
        let (listener_side, _idle_peer) = UnixStream::pair().expect("PointWatch UDS pair");
        let (event_tx, _event_rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let initial_timeouts = Arc::new(AtomicU64::new(0));
        let shutdown = CancellationToken::new();
        let observed_timeouts = Arc::clone(&initial_timeouts);
        let task = tokio::spawn(async move {
            consume_connection(
                listener_side,
                &event_tx,
                &dropped,
                &initial_timeouts,
                &shutdown,
            )
            .await
        });
        let keep_listening = tokio::time::timeout(INITIAL_FRAME_TIMEOUT * 2, task)
            .await
            .expect("initial PointWatch frame deadline")
            .expect("idle PointWatch task joins")
            .expect("idle connection closes cleanly");

        assert!(keep_listening);
        assert_eq!(observed_timeouts.load(Ordering::Relaxed), 1);
    }
}
