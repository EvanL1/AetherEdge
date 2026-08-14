//! Generation-checked C/A command mirroring and IO notification.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_dataplane::{AuthorityReadGuard, DataplaneError, SlotWriter};
use aether_domain::{PhysicalDeviceCommand, TimestampMs};
use aether_ports::{CommandReceipt, DeviceCommandSink, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};

use crate::{
    ChannelPointManifest, CommandAckStatus, CommandHello, DeviceCommandAck, DeviceCommandFrame,
    PhysicalPointAddress,
};

/// IO-side UDS endpoint for the acknowledged durable command protocol.
pub const DEFAULT_COMMAND_UDS_PATH: &str = "/tmp/aether-m2c.sock";

const NOTIFIER_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const UDS_CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const UDS_HELLO_TIMEOUT: Duration = Duration::from_millis(250);
const UDS_ACK_TIMEOUT: Duration = Duration::from_millis(350);
const UDS_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);
const AUTHORITY_READ_TIMEOUT: Duration = Duration::from_millis(350);
const AUTHORITY_RETRY_DELAY: Duration = Duration::from_millis(2);

/// Snapshot of the self-healing local command transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandNotifierStatus {
    configured: bool,
    connected: bool,
    last_failure_at_ms: Option<u64>,
}

impl CommandNotifierStatus {
    #[must_use]
    pub const fn configured(self) -> bool {
        self.configured
    }

    #[must_use]
    pub const fn connected(self) -> bool {
        self.connected
    }

    #[must_use]
    pub const fn last_failure_at_ms(self) -> Option<u64> {
        self.last_failure_at_ms
    }
}

#[derive(Default)]
struct CommandNotifierHealth {
    connected: AtomicBool,
    last_failure_at_ms: AtomicU64,
}

impl CommandNotifierHealth {
    fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}

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
/// submits it through the acknowledged IO command transport.
///
/// A receipt means IO durably admitted the exact `CommandId` and payload; it is
/// not a physical-device completion acknowledgement.
pub struct ShmDeviceCommandSink {
    generations: Arc<CommandGenerationState>,
    notifier: OnceLock<Arc<Mutex<CommandNotifier>>>,
    rebuild_trigger: Arc<Notify>,
    observer: Arc<dyn CommandMirrorObserver>,
    notifier_health: Arc<CommandNotifierHealth>,
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
            notifier_health: Arc::new(CommandNotifierHealth::default()),
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
        let header = writer.header();
        if writer.slot_count() != manifest.slot_count()
            || header.slot_count as usize != manifest.slot_count()
            || header.layout_hash != manifest.layout_hash()
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
        let notifier = Arc::new(Mutex::new(
            CommandNotifier::connect(path, Arc::clone(&self.notifier_health)).await,
        ));
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

    /// Returns actual UDS connection state, not merely whether a path exists.
    #[must_use]
    pub fn notifier_status(&self) -> CommandNotifierStatus {
        let last_failure_at_ms = self
            .notifier_health
            .last_failure_at_ms
            .load(Ordering::Acquire);
        CommandNotifierStatus {
            configured: self.is_notifier_configured(),
            connected: self.notifier_health.connected(),
            last_failure_at_ms: (last_failure_at_ms != 0).then_some(last_failure_at_ms),
        }
    }

    /// Performs one bounded background reconnect attempt without submitting a
    /// device command. Composition roots use this for late IO startup so
    /// readiness can recover before any operator risks a control action.
    pub async fn probe_notifier(&self) -> PortResult<CommandNotifierStatus> {
        let notifier = self.notifier.get().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Unavailable,
                "command UDS notifier is not configured",
            )
        })?;
        let mut notifier = tokio::time::timeout(NOTIFIER_LOCK_TIMEOUT, notifier.lock())
            .await
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Timeout,
                    "command UDS notifier probe lock timed out",
                )
            })?;
        notifier.verify_idle_stream();
        if !notifier.is_connected() {
            // A supervisor already bounds probe frequency. Bypass command-path
            // exponential backoff here so a newly started IO listener becomes
            // ready within one probe interval.
            notifier.last_connect_attempt = None;
            notifier
                .try_reconnect()
                .await
                .map_err(|error| match error {
                    CommandNotifyError::Timeout(context) => {
                        PortError::new(PortErrorKind::Timeout, context)
                    },
                    CommandNotifyError::Io(error) => PortError::new(
                        PortErrorKind::Unavailable,
                        format!("command UDS notifier probe failed: {error}"),
                    ),
                    CommandNotifyError::Expired => PortError::new(
                        PortErrorKind::Permanent,
                        "command notifier probe cannot expire",
                    ),
                    CommandNotifyError::Nack { .. } => PortError::new(
                        PortErrorKind::Permanent,
                        "command notifier probe received an impossible protocol response",
                    ),
                })?;
        }
        drop(notifier);
        Ok(self.notifier_status())
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
        let physical = PhysicalPointAddress::from(target);
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
            crate::encode_point_quality(aether_domain::PointQuality::Good),
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
        let accepted_at_ms =
            match tokio::time::timeout(UDS_NOTIFY_TIMEOUT, notifier.notify(command)).await {
                Err(_) => {
                    // Cancellation may interrupt `write_all` after a partial fixed
                    // frame. Drop the stream so a later command cannot append to
                    // that prefix and corrupt IO's fixed frame boundary.
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
                Ok(Err(CommandNotifyError::Nack { status, state })) => {
                    let kind = match status {
                        CommandAckStatus::Conflict => PortErrorKind::Conflict,
                        CommandAckStatus::Expired | CommandAckStatus::Invalid => {
                            PortErrorKind::Rejected
                        },
                        CommandAckStatus::Backpressure | CommandAckStatus::Busy => {
                            PortErrorKind::Unavailable
                        },
                        CommandAckStatus::Unavailable | CommandAckStatus::Internal => {
                            PortErrorKind::Unavailable
                        },
                        CommandAckStatus::Accepted | CommandAckStatus::Duplicate => {
                            PortErrorKind::Permanent
                        },
                    };
                    return Err(PortError::new(
                        kind,
                        format!("IO command admission returned {status:?} in {state:?}"),
                    ));
                },
                Ok(Err(CommandNotifyError::Io(error))) => {
                    return Err(PortError::new(
                        PortErrorKind::Unavailable,
                        format!("command UDS notification failed after SHM mirror: {error}"),
                    ));
                },
                Ok(Ok(accepted_at_ms)) => accepted_at_ms,
            };
        self.observer.after_transport_write(command);
        self.validate_authority(&generation, "after command transport")?;

        let receipt = CommandReceipt::new(command.id(), TimestampMs::new(accepted_at_ms));
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
    path: std::path::PathBuf,
    last_connect_attempt: Option<Instant>,
    backoff: Duration,
    health: Arc<CommandNotifierHealth>,
}

impl CommandNotifier {
    const MIN_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(5);
    const SEND_RETRIES: usize = 3;
    const RETRY_DELAY: Duration = Duration::from_millis(10);

    async fn connect(path: &Path, health: Arc<CommandNotifierHealth>) -> Self {
        let stream = connect_command_stream(path).await.ok();
        let connected = stream.is_some();
        let last_connect_attempt = (!connected).then(Instant::now);
        health.set_connected(connected);
        if !connected {
            health
                .last_failure_at_ms
                .store(system_time_ms(), Ordering::Release);
        }
        Self {
            stream,
            path: path.to_path_buf(),
            last_connect_attempt,
            backoff: Self::MIN_BACKOFF,
            health,
        }
    }

    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    fn verify_idle_stream(&mut self) {
        let Some(stream) = self.stream.as_ref() else {
            return;
        };
        let mut unexpected = [0_u8; 1];
        match stream.try_read(&mut unexpected) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {},
            Ok(0) | Err(_) => {
                self.disconnect(true);
            },
            Ok(_) => {
                // No server frame is valid while this mutex proves that no
                // command/ACK exchange is in flight. Poison the desynchronised
                // stream and reconnect through a fresh hello.
                self.disconnect(true);
            },
        }
    }

    async fn notify(&mut self, command: PhysicalDeviceCommand) -> Result<u64, CommandNotifyError> {
        self.try_reconnect().await?;
        if system_time_ms() >= command.expires_at().get() {
            return Err(CommandNotifyError::Expired);
        }
        let frame = DeviceCommandFrame::new(command)
            .map_err(|error| {
                CommandNotifyError::Io(io::Error::new(io::ErrorKind::InvalidInput, error))
            })?
            .to_bytes();
        for attempt in 0..Self::SEND_RETRIES {
            if system_time_ms() >= command.expires_at().get() {
                return Err(CommandNotifyError::Expired);
            }
            if self.stream.is_none() {
                self.connect_now().await?;
            }
            let stream = self.stream.as_mut().ok_or_else(|| {
                CommandNotifyError::Io(io::Error::new(
                    io::ErrorKind::NotConnected,
                    format!("IO command listener {:?} is disconnected", self.path),
                ))
            })?;
            let exchange = async {
                stream.write_all(&frame).await?;
                let mut ack = [0_u8; DeviceCommandAck::SIZE];
                stream.read_exact(&mut ack).await?;
                Ok::<_, io::Error>(ack)
            };
            match tokio::time::timeout(UDS_ACK_TIMEOUT, exchange).await {
                Ok(Ok(bytes)) => {
                    let ack = match DeviceCommandAck::from_bytes(&bytes) {
                        Ok(ack) => ack,
                        Err(error) => {
                            self.disconnect(true);
                            return Err(CommandNotifyError::Io(io::Error::new(
                                io::ErrorKind::InvalidData,
                                error,
                            )));
                        },
                    };
                    if ack.command_id() != command.id() {
                        self.disconnect(true);
                        return Err(CommandNotifyError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "IO command acknowledgement id mismatch",
                        )));
                    }
                    self.health.set_connected(true);
                    if ack.status().is_accepted() {
                        return Ok(ack.recorded_at_ms());
                    }
                    return Err(CommandNotifyError::Nack {
                        status: ack.status(),
                        state: ack.state(),
                    });
                },
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
                        "command UDS admission acknowledgement timed out after SHM mirror",
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
        self.mark_disconnected();
        self.last_connect_attempt = if retry_immediately {
            None
        } else {
            Some(Instant::now())
        };
    }

    fn mark_disconnected(&self) {
        self.health.set_connected(false);
        self.health
            .last_failure_at_ms
            .store(system_time_ms(), Ordering::Release);
    }

    async fn try_reconnect(&mut self) -> Result<(), CommandNotifyError> {
        if self.is_connected() {
            return Ok(());
        }
        if self
            .last_connect_attempt
            .is_some_and(|attempt| attempt.elapsed() < self.backoff)
        {
            return Ok(());
        }
        self.connect_now().await
    }

    async fn connect_now(&mut self) -> Result<(), CommandNotifyError> {
        match connect_command_stream(&self.path).await {
            Ok(stream) => {
                self.stream = Some(stream);
                self.health.set_connected(true);
                self.last_connect_attempt = None;
                self.backoff = Self::MIN_BACKOFF;
                Ok(())
            },
            Err(error) => {
                self.mark_disconnected();
                self.last_connect_attempt = Some(Instant::now());
                self.backoff = self.backoff.saturating_mul(2).min(Self::MAX_BACKOFF);
                Err(CommandNotifyError::Io(error))
            },
        }
    }
}

enum CommandNotifyError {
    Expired,
    Timeout(&'static str),
    Nack {
        status: CommandAckStatus,
        state: crate::CommandLedgerStateCode,
    },
    Io(io::Error),
}

async fn connect_raw_stream(path: &Path) -> io::Result<UnixStream> {
    tokio::time::timeout(UDS_CONNECT_TIMEOUT, UnixStream::connect(path))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "command UDS connect timed out"))?
}

async fn connect_command_stream(path: &Path) -> io::Result<UnixStream> {
    let mut stream = connect_raw_stream(path).await?;
    let mut bytes = [0_u8; CommandHello::SIZE];
    tokio::time::timeout(UDS_HELLO_TIMEOUT, stream.read_exact(&mut bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "command hello timed out"))??;
    CommandHello::from_bytes(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(stream)
}
