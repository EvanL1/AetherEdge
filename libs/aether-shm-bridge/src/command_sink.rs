//! Generation-checked C/A command mirroring and IO notification.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_dataplane::{AuthorityReadGuard, DataplaneError, SlotWriter};
use aether_domain::{PhysicalDeviceCommand, PointKind, TimestampMs};
use aether_ports::{CommandReceipt, DeviceCommandSink, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};

use crate::{ChannelPointManifest, PhysicalPointAddress};

/// Existing IO-side UDS endpoint for M2C command event frames.
pub const DEFAULT_COMMAND_UDS_PATH: &str = "/tmp/aether-m2c.sock";

const NOTIFIER_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const UDS_CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const UDS_WRITE_TIMEOUT: Duration = Duration::from_millis(100);
const UDS_NOTIFY_TIMEOUT: Duration = Duration::from_millis(350);
const AUTHORITY_READ_TIMEOUT: Duration = Duration::from_millis(350);
const AUTHORITY_RETRY_DELAY: Duration = Duration::from_millis(2);

/// Synchronous observation hook after SHM mirroring and before the generation
/// post-check.
///
/// The default implementation is a no-op. The hook also makes the TOCTOU
/// boundary deterministic in conformance tests; it cannot approve delivery or
/// bypass any validation.
pub trait CommandMirrorObserver: Send + Sync + 'static {
    /// Observes a completed SHM mirror before transport notification.
    fn after_shm_write(&self, command: PhysicalDeviceCommand, slot: usize);

    /// Observes a complete transport write before authority is confirmed for
    /// the acceptance receipt.
    ///
    /// Production observers normally leave this hook untouched. Conformance
    /// tests use it to place an atomic replacement exactly at the final
    /// canonical-identity boundary.
    fn after_transport_write(&self, _command: PhysicalDeviceCommand) {}
}

struct NoopCommandMirrorObserver;

impl CommandMirrorObserver for NoopCommandMirrorObserver {
    fn after_shm_write(&self, _command: PhysicalDeviceCommand, _slot: usize) {}
}

struct CommandGeneration {
    writer: Arc<SlotWriter>,
    manifest: Arc<ChannelPointManifest>,
    expected_generation: u64,
}

struct CommandGenerationState {
    current: RwLock<Option<Arc<CommandGeneration>>>,
}

/// Reloadable view of the manifest published with the current command writer.
///
/// Every load reads the same generation cell used by command dispatch, so a
/// PointWatch rebuild cannot retain a stale one-time manifest snapshot after
/// IO atomically replaces the canonical SHM layout.
#[derive(Clone)]
pub struct ChannelPointManifestSource {
    state: Arc<CommandGenerationState>,
}

impl ChannelPointManifestSource {
    /// Loads the manifest from the currently published writer generation.
    #[must_use]
    pub fn load(&self) -> Option<Arc<ChannelPointManifest>> {
        self.state
            .current
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|value| Arc::clone(&value.manifest)))
    }
}

/// Physical command sink that mirrors C/A state into authoritative SHM and
/// writes the unchanged 56-byte command event to IO's UDS listener.
///
/// A successful receipt means the local IO transport accepted the complete
/// event frame. It is not a physical-device acknowledgement.
pub struct ShmDeviceCommandSink {
    generations: Arc<CommandGenerationState>,
    notifier: OnceLock<Arc<Mutex<CommandNotifier>>>,
    rebuild_trigger: Arc<Notify>,
    observer: Arc<dyn CommandMirrorObserver>,
}

impl Default for ShmDeviceCommandSink {
    fn default() -> Self {
        Self::new()
    }
}

impl ShmDeviceCommandSink {
    /// Creates an unconfigured sink. Each command submitted before a writer is
    /// published requests another rebuild attempt and fails closed.
    #[must_use]
    pub fn new() -> Self {
        Self::with_observer(Arc::new(NoopCommandMirrorObserver))
    }

    /// Creates a sink with a post-mirror observer.
    #[must_use]
    pub fn with_observer(observer: Arc<dyn CommandMirrorObserver>) -> Self {
        Self {
            generations: Arc::new(CommandGenerationState {
                current: RwLock::new(None),
            }),
            notifier: OnceLock::new(),
            rebuild_trigger: Arc::new(Notify::new()),
            observer,
        }
    }

    /// Atomically publishes one coherent writer/manifest generation.
    pub fn publish_generation(
        &self,
        writer: Arc<SlotWriter>,
        manifest: Arc<ChannelPointManifest>,
    ) -> PortResult<()> {
        writer
            .validate_authoritative_path()
            .map_err(dataplane_port_error)?;
        let header = writer.header().snapshot();
        if writer.slot_count() != manifest.slot_count()
            || header.slot_count as usize != manifest.slot_count()
            || header.routing_hash != manifest.layout_hash()
        {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "command writer and channel manifest describe different SHM layouts",
            ));
        }
        if header.writer_generation == 0 || header.writer_generation & 1 != 0 {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                format!(
                    "SHM generation {} is not stably published",
                    header.writer_generation
                ),
            ));
        }

        let published = Arc::new(CommandGeneration {
            writer,
            manifest,
            expected_generation: header.writer_generation,
        });
        let mut guard = self.generations.current.write().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "command generation lock was poisoned",
            )
        })?;
        *guard = Some(published);
        Ok(())
    }

    /// Opens and validates the canonical segment against a manifest, then
    /// publishes both as one coherent generation.
    pub fn open_generation(
        &self,
        path: impl AsRef<Path>,
        manifest: Arc<ChannelPointManifest>,
    ) -> PortResult<()> {
        let writer = SlotWriter::open_existing(path, manifest.slot_count(), manifest.layout_hash())
            .map_err(dataplane_port_error)?;
        self.publish_generation(Arc::new(writer), manifest)
    }

    /// Configures the self-healing UDS notifier exactly once.
    ///
    /// Initial connection failure is not a configuration error: the notifier
    /// retains the path and retries with bounded backoff on later commands.
    pub async fn configure_notifier(&self, path: impl AsRef<Path>) -> PortResult<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "command UDS path must not be empty",
            ));
        }
        let notifier = Arc::new(Mutex::new(CommandNotifier::connect(path).await));
        self.notifier.set(notifier).map_err(|_| {
            PortError::new(
                PortErrorKind::Conflict,
                "command UDS notifier is already configured",
            )
        })
    }

    /// Returns the rebuild signal used by the composition root.
    #[must_use]
    pub fn rebuild_trigger(&self) -> Arc<Notify> {
        Arc::clone(&self.rebuild_trigger)
    }

    /// Invalidates the currently mapped generation and requests a reopen.
    ///
    /// The canonical-path inode watcher calls this after an atomic rename,
    /// because the old mmap's header generation cannot reveal that its path
    /// now names a different file. Commands fail closed until publication of
    /// the replacement writer/manifest pair.
    pub fn invalidate_and_rebuild(&self) {
        if let Ok(mut guard) = self.generations.current.write() {
            *guard = None;
        }
        self.rebuild_trigger.notify_one();
    }

    /// Returns the currently published typed manifest snapshot.
    #[must_use]
    pub fn manifest(&self) -> Option<Arc<ChannelPointManifest>> {
        self.manifest_source().load()
    }

    /// Returns a reloadable manifest source tied to the sink's generation
    /// cell. Long-lived consumers should retain this handle instead of one
    /// manifest snapshot.
    #[must_use]
    pub fn manifest_source(&self) -> ChannelPointManifestSource {
        ChannelPointManifestSource {
            state: Arc::clone(&self.generations),
        }
    }

    /// Returns whether a coherent SHM generation is currently available.
    #[must_use]
    pub fn is_writer_available(&self) -> bool {
        self.generations
            .current
            .read()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// Returns whether the UDS path has been configured.
    #[must_use]
    pub fn is_notifier_configured(&self) -> bool {
        self.notifier.get().is_some()
    }

    fn current_generation(&self) -> PortResult<Arc<CommandGeneration>> {
        let guard = self.generations.current.read().map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "command generation lock was poisoned",
            )
        })?;
        if let Some(generation) = guard.as_ref() {
            return Ok(Arc::clone(generation));
        }
        drop(guard);
        // A previous rebuild may have exhausted its retries. Every later
        // command grants the composition root a fresh self-healing attempt.
        self.rebuild_trigger.notify_one();
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "authoritative command SHM writer is unavailable",
        ))
    }

    fn invalidate_stale_generation(&self, stale: &Arc<CommandGeneration>) {
        if let Ok(mut guard) = self.generations.current.write()
            && guard
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, stale))
        {
            *guard = None;
        }
        self.rebuild_trigger.notify_one();
    }

    fn validate_authority(
        &self,
        generation: &Arc<CommandGeneration>,
        phase: &'static str,
    ) -> PortResult<()> {
        let is_current = self
            .generations
            .current
            .read()
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "command generation lock was poisoned",
                )
            })?
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, generation));
        if !is_current {
            self.invalidate_stale_generation(generation);
            return Err(PortError::new(
                PortErrorKind::Conflict,
                format!("command SHM authority changed {phase}"),
            ));
        }

        if let Err(error) = generation.writer.validate_authoritative_path() {
            self.invalidate_stale_generation(generation);
            let kind = match error {
                DataplaneError::Io { .. } => PortErrorKind::Unavailable,
                DataplaneError::InvalidLayout(_) | DataplaneError::InvalidPath(_) => {
                    PortErrorKind::Conflict
                },
            };
            return Err(PortError::new(
                kind,
                format!("command SHM authority lost {phase}: {error}"),
            ));
        }

        let actual_generation = generation.writer.generation();
        if actual_generation == generation.expected_generation
            && actual_generation != 0
            && actual_generation & 1 == 0
        {
            return Ok(());
        }
        self.invalidate_stale_generation(generation);
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "SHM generation changed {phase}: expected {}, got {actual_generation}",
                generation.expected_generation
            ),
        ))
    }

    async fn acquire_authority(
        &self,
        generation: &Arc<CommandGeneration>,
        command: PhysicalDeviceCommand,
    ) -> PortResult<AuthorityReadGuard> {
        let started = Instant::now();
        loop {
            match generation.writer.try_acquire_authority_read() {
                Ok(Some(guard)) => return Ok(guard),
                Ok(None) => {},
                Err(error) => return Err(dataplane_port_error(error)),
            }
            if system_time_ms() >= command.expires_at().get() {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    "command expired while waiting for the SHM authority lease",
                ));
            }
            if started.elapsed() >= AUTHORITY_READ_TIMEOUT {
                return Err(PortError::new(
                    PortErrorKind::Timeout,
                    "timed out waiting for canonical SHM replacement to finish",
                ));
            }
            tokio::time::sleep(AUTHORITY_RETRY_DELAY).await;
        }
    }
}

#[async_trait]
impl DeviceCommandSink for ShmDeviceCommandSink {
    async fn send(&self, command: PhysicalDeviceCommand) -> PortResult<CommandReceipt> {
        let now = TimestampMs::new(system_time_ms());
        command
            .validate_at(now)
            .map_err(|error| PortError::new(PortErrorKind::Rejected, error.to_string()))?;

        let generation = self.current_generation()?;
        let authority = self.acquire_authority(&generation, command).await?;
        self.validate_authority(&generation, "before command mirror")?;

        let target = command.target();
        let physical =
            PhysicalPointAddress::new(target.channel_id(), target.kind(), target.point_id());
        let slot = generation.manifest.slot_for(physical).ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotFound,
                format!("physical command target {target:?} has no SHM slot"),
            )
        })?;

        generation.writer.set_direct(
            slot,
            command.value(),
            command.value(),
            command.issued_at().get(),
        );
        self.observer.after_shm_write(command, slot);

        self.validate_authority(&generation, "after command mirror")?;

        let notifier = self.notifier.get().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Unavailable,
                "command UDS notifier is not configured; SHM was mirrored but IO was not notified",
            )
        })?;
        let mut notifier = tokio::time::timeout(NOTIFIER_LOCK_TIMEOUT, notifier.lock())
            .await
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Timeout,
                    "command UDS notifier lock timed out after SHM mirror",
                )
            })?;
        command
            .validate_at(TimestampMs::new(system_time_ms()))
            .map_err(|error| PortError::new(PortErrorKind::Rejected, error.to_string()))?;
        self.validate_authority(&generation, "before command transport")?;
        match tokio::time::timeout(UDS_NOTIFY_TIMEOUT, notifier.notify(command)).await {
            Err(_) => {
                // Cancellation may interrupt `write_all` after a partial fixed
                // frame. Drop the stream so a later command cannot append to
                // that prefix and corrupt IO's 56-byte frame boundary.
                notifier.disconnect(false);
                return Err(PortError::new(
                    PortErrorKind::Timeout,
                    "command UDS notification exceeded its bounded transport deadline",
                ));
            },
            Ok(Err(CommandNotifyError::Expired)) => {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    "command expired immediately before UDS transport write",
                ));
            },
            Ok(Err(CommandNotifyError::Timeout(context))) => {
                return Err(PortError::new(PortErrorKind::Timeout, context));
            },
            Ok(Err(CommandNotifyError::Io(error))) => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    format!("command UDS notification failed after SHM mirror: {error}"),
                ));
            },
            Ok(Ok(())) => {},
        }
        self.observer.after_transport_write(command);
        self.validate_authority(&generation, "after command transport")?;

        let receipt = CommandReceipt::new(command.id(), TimestampMs::new(system_time_ms()));
        drop(authority);
        Ok(receipt)
    }
}

fn dataplane_port_error(error: DataplaneError) -> PortError {
    let kind = match error {
        DataplaneError::Io { .. } => PortErrorKind::Unavailable,
        DataplaneError::InvalidLayout(_) | DataplaneError::InvalidPath(_) => {
            PortErrorKind::Conflict
        },
    };
    PortError::new(kind, error.to_string())
}

fn system_time_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    match u64::try_from(millis) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}

struct CommandNotifier {
    stream: Option<UnixStream>,
    path: PathBuf,
    producer_id: u64,
    next_sequence: u64,
    last_connect_attempt: Option<Instant>,
    backoff: Duration,
}

impl CommandNotifier {
    const MIN_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(5);
    const SEND_RETRIES: usize = 3;
    const RETRY_DELAY: Duration = Duration::from_millis(10);

    async fn connect(path: &Path) -> Self {
        let stream = tokio::time::timeout(UDS_CONNECT_TIMEOUT, UnixStream::connect(path))
            .await
            .ok()
            .and_then(Result::ok);
        let last_connect_attempt = stream.is_none().then(Instant::now);
        Self {
            stream,
            path: path.to_path_buf(),
            producer_id: new_producer_id(),
            next_sequence: 1,
            last_connect_attempt,
            backoff: Self::MIN_BACKOFF,
        }
    }

    async fn notify(&mut self, command: PhysicalDeviceCommand) -> Result<(), CommandNotifyError> {
        self.try_reconnect().await?;
        if system_time_ms() >= command.expires_at().get() {
            return Err(CommandNotifyError::Expired);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        let frame = DeviceCommandFrame::new(command, self.producer_id, sequence)?.to_bytes();
        for attempt in 0..Self::SEND_RETRIES {
            if system_time_ms() >= command.expires_at().get() {
                return Err(CommandNotifyError::Expired);
            }
            self.try_reconnect().await?;
            let stream = self.stream.as_mut().ok_or_else(|| {
                CommandNotifyError::Io(io::Error::new(
                    io::ErrorKind::NotConnected,
                    format!("IO command listener {:?} is disconnected", self.path),
                ))
            })?;
            match tokio::time::timeout(UDS_WRITE_TIMEOUT, stream.write_all(&frame)).await {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(_error)) if attempt + 1 < Self::SEND_RETRIES => {
                    self.disconnect(true);
                    tokio::time::sleep(Self::RETRY_DELAY).await;
                },
                Ok(Err(error)) => {
                    self.disconnect(false);
                    return Err(CommandNotifyError::Io(error));
                },
                Err(_) if attempt + 1 < Self::SEND_RETRIES => {
                    self.disconnect(true);
                    tokio::time::sleep(Self::RETRY_DELAY).await;
                },
                Err(_) => {
                    self.disconnect(false);
                    return Err(CommandNotifyError::Timeout(
                        "command UDS write timed out after SHM mirror",
                    ));
                },
            }
        }
        Err(CommandNotifyError::Io(io::Error::other(
            "command frame retry loop exhausted",
        )))
    }

    fn disconnect(&mut self, retry_immediately: bool) {
        self.stream = None;
        self.last_connect_attempt = if retry_immediately {
            None
        } else {
            Some(Instant::now())
        };
    }

    async fn try_reconnect(&mut self) -> Result<(), CommandNotifyError> {
        if self.stream.is_some() {
            return Ok(());
        }
        if self
            .last_connect_attempt
            .is_some_and(|attempt| attempt.elapsed() < self.backoff)
        {
            return Ok(());
        }
        match tokio::time::timeout(UDS_CONNECT_TIMEOUT, UnixStream::connect(&self.path)).await {
            Ok(Ok(stream)) => {
                self.stream = Some(stream);
                self.last_connect_attempt = None;
                self.backoff = Self::MIN_BACKOFF;
            },
            Ok(Err(_)) => {
                self.last_connect_attempt = Some(Instant::now());
                self.backoff = self.backoff.saturating_mul(2).min(Self::MAX_BACKOFF);
            },
            Err(_) => {
                self.last_connect_attempt = Some(Instant::now());
                self.backoff = self.backoff.saturating_mul(2).min(Self::MAX_BACKOFF);
                return Err(CommandNotifyError::Timeout(
                    "command UDS reconnect timed out after SHM mirror",
                ));
            },
        }
        Ok(())
    }
}

enum CommandNotifyError {
    Expired,
    Timeout(&'static str),
    Io(io::Error),
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCommandFrame {
    channel_id: u32,
    point_id: u32,
    point_type: u8,
    padding: [u8; 7],
    value_bits: u64,
    timestamp_ms: u64,
    expires_at_ms: u64,
    producer_id: u64,
    sequence: u64,
}

impl DeviceCommandFrame {
    /// Fixed native-endian command wire size retained for IO compatibility.
    pub const SIZE: usize = 56;

    fn new(
        command: PhysicalDeviceCommand,
        producer_id: u64,
        sequence: u64,
    ) -> Result<Self, CommandNotifyError> {
        let target = command.target();
        let point_type = match target.kind() {
            PointKind::Command => 2,
            PointKind::Action => 3,
            PointKind::Telemetry | PointKind::Status => {
                return Err(CommandNotifyError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "acquisition-owned point cannot enter the command wire",
                )));
            },
        };
        Ok(Self {
            channel_id: target.channel_id().get(),
            point_id: target.point_id().get(),
            point_type,
            padding: [0; 7],
            value_bits: command.value().to_bits(),
            timestamp_ms: command.issued_at().get(),
            expires_at_ms: command.expires_at().get(),
            producer_id,
            sequence,
        })
    }

    /// Decodes the existing fixed-size native-endian IO command frame.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        Self {
            channel_id: u32::from_ne_bytes(bytes[0..4].try_into().unwrap_or([0; 4])),
            point_id: u32::from_ne_bytes(bytes[4..8].try_into().unwrap_or([0; 4])),
            point_type: bytes[8],
            padding: bytes[9..16].try_into().unwrap_or([0; 7]),
            value_bits: u64::from_ne_bytes(bytes[16..24].try_into().unwrap_or([0; 8])),
            timestamp_ms: u64::from_ne_bytes(bytes[24..32].try_into().unwrap_or([0; 8])),
            expires_at_ms: u64::from_ne_bytes(bytes[32..40].try_into().unwrap_or([0; 8])),
            producer_id: u64::from_ne_bytes(bytes[40..48].try_into().unwrap_or([0; 8])),
            sequence: u64::from_ne_bytes(bytes[48..56].try_into().unwrap_or([0; 8])),
        }
    }

    /// Encodes the unchanged native-endian IO command frame.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0_u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.channel_id.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.point_id.to_ne_bytes());
        bytes[8] = self.point_type;
        bytes[9..16].copy_from_slice(&self.padding);
        bytes[16..24].copy_from_slice(&self.value_bits.to_ne_bytes());
        bytes[24..32].copy_from_slice(&self.timestamp_ms.to_ne_bytes());
        bytes[32..40].copy_from_slice(&self.expires_at_ms.to_ne_bytes());
        bytes[40..48].copy_from_slice(&self.producer_id.to_ne_bytes());
        bytes[48..56].copy_from_slice(&self.sequence.to_ne_bytes());
        bytes
    }

    /// Returns the target physical channel.
    #[must_use]
    pub const fn channel_id(self) -> u32 {
        self.channel_id
    }

    /// Returns the target physical point id.
    #[must_use]
    pub const fn point_id(self) -> u32 {
        self.point_id
    }

    /// Returns the raw point-kind code used by duplicate detection.
    #[must_use]
    pub const fn point_kind_code(self) -> u8 {
        self.point_type
    }

    /// Returns the typed command-owned point kind.
    #[must_use]
    pub const fn point_kind(self) -> Option<PointKind> {
        match self.point_type {
            2 => Some(PointKind::Command),
            3 => Some(PointKind::Action),
            _ => None,
        }
    }

    /// Returns the command engineering value.
    #[must_use]
    pub fn value(self) -> f64 {
        f64::from_bits(self.value_bits)
    }

    /// Returns the command issue timestamp.
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the exclusive command deadline.
    #[must_use]
    pub const fn expires_at_ms(self) -> u64 {
        self.expires_at_ms
    }

    /// Returns the producer incarnation id.
    #[must_use]
    pub const fn producer_id(self) -> u64 {
        self.producer_id
    }

    /// Returns the monotonic sequence within the producer incarnation.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

fn new_producer_id() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let bytes = nanos.to_ne_bytes();
    let time_bits = u64::from_ne_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let producer = time_bits ^ (u64::from(std::process::id()) << 32);
    producer.max(1)
}
