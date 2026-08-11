//! PointWatch wire contract and isolated consumer-side UDS listener.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const POINT_WATCH_FRAME_MARKER: [u8; 3] = [0xA5, 1, 0x5A];

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

    /// Decodes the strict v1 little-endian wire representation.
    ///
    /// The trailing marker prevents an older, longer PointWatch payload from
    /// being interpreted as a valid compact hint by a new listener.
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> PortResult<Self> {
        if bytes[13..16] != POINT_WATCH_FRAME_MARKER {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "PointWatch frame marker or version is invalid",
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
        bytes[13..16].copy_from_slice(&POINT_WATCH_FRAME_MARKER);
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
            },
            event_rx,
        )
    }

    /// Returns events dropped due to bounded in-process backpressure.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Runs until cancellation, accepting producer reconnects serially.
    pub async fn run(self) -> std::io::Result<()> {
        prepare_socket_path(&self.socket_path)?;
        let listener = UnixListener::bind(&self.socket_path)?;
        let _cleanup = SocketCleanup(self.socket_path.clone());

        loop {
            let stream = tokio::select! {
                _ = self.shutdown.cancelled() => break,
                accepted = listener.accept() => accepted?.0,
            };
            if !consume_connection(stream, &self.event_tx, &self.dropped_count, &self.shutdown)
                .await?
            {
                break;
            }
        }
        Ok(())
    }
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

async fn consume_connection(
    mut stream: UnixStream,
    event_tx: &mpsc::Sender<PointWatchEvent>,
    dropped_count: &AtomicU64,
    shutdown: &CancellationToken,
) -> std::io::Result<bool> {
    loop {
        let mut bytes = [0_u8; PointWatchEvent::SIZE];
        let read = tokio::select! {
            _ = shutdown.cancelled() => return Ok(false),
            read = stream.read_exact(&mut bytes) => read,
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
