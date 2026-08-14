//! Shared Memory Command Listener (Event-Driven)
//!
//! Listens for M2C commands via Unix Domain Socket notifications.
//! Replaces polling with event-driven architecture for lower latency.
//!
//! ## Architecture
//!
//! ```text
//! automation/rules: write SHM → send UDS command event ──►
//!                                                       │
//! io: listen UDS ← recv full command event → dispatch command
//! ```
//!
//! Replaced the former ShmCommandPoller (polling-based) with lower latency (~1-2ms vs 10-20ms avg)
//! and event-triggered CPU usage instead of continuous polling.

use aether_core::PointType;
use aether_domain::CommandConstraints;
use aether_shm_bridge::{
    CommandAckStatus, CommandHello, CommandLedgerStateCode, DEFAULT_COMMAND_UDS_PATH,
    DeviceCommandAck, DeviceCommandFrame,
};
use dashmap::DashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info, warn};

use crate::core::channels::command_ledger::{
    CommandLedger, CommandLedgerAcceptance, CommandLedgerAdmission, CommandLedgerRecord,
    CommandLedgerState, CommandLedgerTransition,
};
use crate::core::channels::command_outcome::{CommandLifecycleState, CommandOutcomeTracker};
use crate::core::channels::types::ChannelCommand;

const COMMAND_HANDLER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
const COMMAND_CONNECTION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const COMMAND_ACK_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
const MAX_COMMAND_CONNECTIONS: usize = 64;

/// Bounded command-listener resource and rejection telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShmListenerStats {
    pub running: bool,
    pub active_connections: usize,
    pub connection_capacity: usize,
    pub rejected_connections: u64,
    pub idle_timeouts: u64,
    pub frames_total: u64,
    pub last_frame_at_ms: Option<u64>,
}

struct ListenerRunGuard(Arc<AtomicBool>);

impl Drop for ListenerRunGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
struct ConnectionContext {
    senders: Arc<DashMap<u32, mpsc::Sender<ChannelCommand>>>,
    dropped_count: Arc<AtomicU64>,
    idle_timeouts: Arc<AtomicU64>,
    accepting_commands: Arc<AtomicBool>,
    registration_gate: Arc<RwLock<()>>,
    command_outcomes: Arc<CommandOutcomeTracker>,
    command_ledger: Arc<CommandLedger>,
    frames_total: Arc<AtomicU64>,
    last_frame_at_ms: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueDecision {
    Enqueued,
    Expired,
    Unavailable,
}

/// Shared Memory Command Listener (Event-Driven)
pub struct ShmCommandListener {
    command_senders: Arc<DashMap<u32, mpsc::Sender<ChannelCommand>>>,
    uds_path: String,
    shutdown: tokio::sync::watch::Receiver<bool>,
    dropped_count: Arc<AtomicU64>,
    connection_slots: Arc<Semaphore>,
    rejected_connections: Arc<AtomicU64>,
    idle_timeouts: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    accepting_commands: Arc<AtomicBool>,
    registration_gate: Arc<RwLock<()>>,
    command_outcomes: Arc<CommandOutcomeTracker>,
    command_ledger: Arc<CommandLedger>,
    frames_total: Arc<AtomicU64>,
    last_frame_at_ms: Arc<AtomicU64>,
}

/// An owner-only command socket reserved before runtime side effects begin.
#[derive(Debug)]
pub struct PreparedShmCommandListener {
    listener: Option<UnixListener>,
    uds_path: String,
    identity: SocketIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn read(path: &std::path::Path) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for PreparedShmCommandListener {
    fn drop(&mut self) {
        let socket_path = std::path::Path::new(&self.uds_path);
        if SocketIdentity::read(socket_path).ok() == Some(self.identity) {
            let _ = std::fs::remove_file(socket_path);
        }
    }
}

impl ShmCommandListener {
    /// Create the single command listener with its mandatory durable ledger.
    pub fn new(
        uds_path: Option<&str>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        command_ledger: Arc<CommandLedger>,
    ) -> Self {
        Self::with_outcomes_and_ledger(
            uds_path,
            shutdown,
            Arc::new(CommandOutcomeTracker::default()),
            command_ledger,
        )
    }

    /// Constructs the real listener with an injected durable ledger for
    /// the repository-owned command-path benchmark.
    ///
    /// This seam is absent from default production builds. It deliberately
    /// reuses the production connection handler instead of duplicating its
    /// admission and retry rules in benchmark-only code.
    #[cfg(feature = "bench-support")]
    #[doc(hidden)]
    pub fn with_command_ledger_for_bench(
        uds_path: &str,
        shutdown: tokio::sync::watch::Receiver<bool>,
        command_ledger: Arc<CommandLedger>,
    ) -> Self {
        Self::with_outcomes_and_ledger(
            Some(uds_path),
            shutdown,
            Arc::new(CommandOutcomeTracker::default()),
            command_ledger,
        )
    }

    pub(crate) fn with_outcomes_and_ledger(
        uds_path: Option<&str>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        command_outcomes: Arc<CommandOutcomeTracker>,
        command_ledger: Arc<CommandLedger>,
    ) -> Self {
        let path = uds_path.unwrap_or(DEFAULT_COMMAND_UDS_PATH).to_string();
        info!("ShmCommandListener: UDS path = {}", path);

        Self {
            command_senders: Arc::new(DashMap::new()),
            uds_path: path,
            shutdown,
            dropped_count: Arc::new(AtomicU64::new(0)),
            connection_slots: Arc::new(Semaphore::new(MAX_COMMAND_CONNECTIONS)),
            rejected_connections: Arc::new(AtomicU64::new(0)),
            idle_timeouts: Arc::new(AtomicU64::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            accepting_commands: Arc::new(AtomicBool::new(true)),
            registration_gate: Arc::new(RwLock::new(())),
            command_outcomes,
            command_ledger,
            frames_total: Arc::new(AtomicU64::new(0)),
            last_frame_at_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Reserve the configured M2C endpoint without constructing channel state.
    pub fn prepare_path(uds_path: Option<&str>) -> std::io::Result<PreparedShmCommandListener> {
        let uds_path = uds_path.unwrap_or(DEFAULT_COMMAND_UDS_PATH);
        let (listener, identity) = Self::prepare_one_uds_path(uds_path)?;
        Ok(PreparedShmCommandListener {
            listener: Some(listener),
            uds_path: uds_path.to_string(),
            identity,
        })
    }

    /// Reserve and secure the M2C endpoint before any device runtime is created.
    pub fn prepare(&self) -> std::io::Result<PreparedShmCommandListener> {
        Self::prepare_path(Some(&self.uds_path))
    }

    fn prepare_one_uds_path(uds_path: &str) -> std::io::Result<(UnixListener, SocketIdentity)> {
        let socket_path = std::path::Path::new(uds_path);
        if socket_path.exists() {
            if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("another listener is active on {uds_path}"),
                ));
            }
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(uds_path)?;
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(uds_path, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(uds_path);
            return Err(error);
        }
        let identity = match SocketIdentity::read(socket_path) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = std::fs::remove_file(socket_path);
                return Err(error);
            },
        };
        Ok((listener, identity))
    }

    /// Register a channel's command sender
    pub fn register_channel(&self, channel_id: u32, sender: mpsc::Sender<ChannelCommand>) {
        let _gate = match self.registration_gate.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !self.accepting_commands.load(Ordering::Acquire) {
            debug!(
                "ShmListener: ignored registration for channel {} during shutdown",
                channel_id
            );
            return;
        }
        self.command_senders.insert(channel_id, sender);
        debug!("ShmListener: registered channel {}", channel_id);
    }

    /// Stop accepting commands and detach every channel sender.
    ///
    /// This is synchronous so the shutdown coordinator can establish a hard
    /// command fence before it waits for the UDS accept task to exit.
    pub fn quiesce(&self) {
        let _gate = match self.registration_gate.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.accepting_commands.store(false, Ordering::Release);
        self.command_senders.clear();
    }

    /// Unregister a channel
    pub fn unregister_channel(&self, channel_id: u32) {
        let _gate = match self.registration_gate.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.command_senders.remove(&channel_id);
    }

    /// Start the listener
    pub async fn run(&self) -> std::io::Result<()> {
        let prepared = self.prepare()?;
        self.run_prepared(prepared).await
    }

    /// Run from an endpoint already reserved by the composition root.
    pub async fn run_prepared(
        &self,
        mut prepared: PreparedShmCommandListener,
    ) -> std::io::Result<()> {
        if prepared.uds_path != self.uds_path {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "prepared command listener path does not match runtime configuration",
            ));
        }
        let listener = prepared.listener.take().ok_or_else(|| {
            std::io::Error::other("prepared command listener was already consumed")
        })?;
        info!(path = %self.uds_path, "ShmCommandListener started (mode 0600)");

        self.running.store(true, Ordering::Release);
        let _running_guard = ListenerRunGuard(Arc::clone(&self.running));

        let mut shutdown = self.shutdown.clone();
        let connection_context = ConnectionContext {
            senders: Arc::clone(&self.command_senders),
            dropped_count: Arc::clone(&self.dropped_count),
            idle_timeouts: Arc::clone(&self.idle_timeouts),
            accepting_commands: Arc::clone(&self.accepting_commands),
            registration_gate: Arc::clone(&self.registration_gate),
            command_outcomes: Arc::clone(&self.command_outcomes),
            command_ledger: Arc::clone(&self.command_ledger),
            frames_total: Arc::clone(&self.frames_total),
            last_frame_at_ms: Arc::clone(&self.last_frame_at_ms),
        };
        // JoinSet aborts every owned connection handler if the listener future
        // itself is cancelled. A Vec<JoinHandle> would detach its remaining
        // tasks on drop and allow command dispatch after coordinator drain.
        let mut connection_handlers = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            self.spawn_connection_handler(
                                stream,
                                connection_context.clone(),
                                &mut connection_handlers,
                            );
                        }
                        Err(e) => {
                            warn!("UDS accept error: {}", e);
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("ShmCommandListener shutdown");
                        break;
                    }
                }
            }
        }

        let drain_handlers = async {
            while let Some(result) = connection_handlers.join_next().await {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    warn!(%error, "ShmListener: connection handler failed while draining");
                }
            }
        };
        if tokio::time::timeout(COMMAND_HANDLER_DRAIN_TIMEOUT, drain_handlers)
            .await
            .is_err()
        {
            connection_handlers.abort_all();
            while connection_handlers.join_next().await.is_some() {}
        }
        Ok(())
    }

    fn spawn_connection_handler(
        &self,
        stream: tokio::net::UnixStream,
        context: ConnectionContext,
        connection_handlers: &mut tokio::task::JoinSet<()>,
    ) {
        let connection_permit = match Arc::clone(&self.connection_slots).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let rejected = self
                    .rejected_connections
                    .fetch_add(1, Ordering::Relaxed)
                    .saturating_add(1);
                if rejected.is_power_of_two() {
                    warn!(
                        rejected,
                        capacity = MAX_COMMAND_CONNECTIONS,
                        "ShmListener: command connection capacity exhausted"
                    );
                }
                return;
            },
        };
        let shutdown_rx = self.shutdown.clone();
        while let Some(result) = connection_handlers.try_join_next() {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                warn!(%error, "ShmListener: connection handler failed");
            }
        }
        connection_handlers.spawn(async move {
            let _connection_permit = connection_permit;
            Self::handle_connection(stream, context, shutdown_rx).await;
        });
    }

    async fn handle_connection(
        mut stream: tokio::net::UnixStream,
        context: ConnectionContext,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        debug!("ShmListener: new connection");
        let hello = CommandHello::new();
        match tokio::time::timeout(
            COMMAND_ACK_WRITE_TIMEOUT,
            stream.write_all(&hello.to_bytes()),
        )
        .await
        {
            Ok(Ok(())) => {},
            Ok(Err(error)) => {
                warn!(%error, "ShmListener: readiness write failed");
                return;
            },
            Err(_) => {
                warn!("ShmListener: readiness write timed out");
                return;
            },
        }
        let mut buf = [0_u8; DeviceCommandFrame::SIZE];
        loop {
            tokio::select! {
                result = tokio::time::timeout(
                    COMMAND_CONNECTION_IDLE_TIMEOUT,
                    stream.read_exact(&mut buf),
                ) => {
                    let frame = match result {
                        Ok(Ok(_)) => match DeviceCommandFrame::from_bytes(&buf) {
                            Ok(frame) => {
                                context.frames_total.fetch_add(1, Ordering::Relaxed);
                                context.last_frame_at_ms.store(Self::now_ms(), Ordering::Relaxed);
                                frame
                            },
                            Err(error) => {
                                warn!(%error, "ShmListener: invalid command frame");
                                break;
                            },
                        },
                        Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Ok(Err(error)) => {
                            warn!(%error, "ShmListener: command read error");
                            break;
                        },
                        Err(_) => {
                            context.idle_timeouts.fetch_add(1, Ordering::Relaxed);
                            break;
                        },
                    };
                    let ack = Self::handle_notification(frame, &context).await;
                    match tokio::time::timeout(
                        COMMAND_ACK_WRITE_TIMEOUT,
                        stream.write_all(&ack.to_bytes()),
                    )
                    .await
                    {
                        Ok(Ok(())) => {},
                        Ok(Err(error)) => {
                            warn!(%error, "ShmListener: acknowledgement write failed");
                            break;
                        },
                        Err(_) => {
                            warn!("ShmListener: acknowledgement write timed out");
                            break;
                        },
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }

    async fn handle_notification(
        frame: DeviceCommandFrame,
        context: &ConnectionContext,
    ) -> DeviceCommandAck {
        let command_id = frame.command_id();
        let command_key = format!("{:032x}", command_id.get());
        let now_ms = Self::now_ms();
        let ack = |status, state| DeviceCommandAck::new(command_id, status, state, now_ms);
        let ledger = &context.command_ledger;
        if !context.accepting_commands.load(Ordering::Acquire) {
            return ack(
                CommandAckStatus::Unavailable,
                CommandLedgerStateCode::Unknown,
            );
        }

        let point_type = match frame.point_kind() {
            aether_domain::PointKind::Command => PointType::Control,
            aether_domain::PointKind::Action => PointType::Adjustment,
            aether_domain::PointKind::Telemetry | aether_domain::PointKind::Status => {
                return ack(CommandAckStatus::Invalid, CommandLedgerStateCode::Unknown);
            },
        };
        if CommandConstraints::unbounded()
            .validate_value(frame.value())
            .is_err()
        {
            return ack(CommandAckStatus::Invalid, CommandLedgerStateCode::Unknown);
        }

        // Resolve an existing identity before reserving queue capacity. A
        // durable retry must report its exact prior state even when the live
        // queue is currently full. The digest is recomputed by frame parsing.
        let semantic_digest = frame.semantic_digest();
        match ledger.query(command_id).await {
            Ok(Some(record)) if record.digest() != &semantic_digest => {
                return ack(CommandAckStatus::Conflict, CommandLedgerStateCode::Unknown);
            },
            Ok(Some(record)) => return Self::ack_existing(command_id, &record, now_ms),
            Ok(None) => {},
            Err(error) => {
                warn!(%error, "ShmListener: durable identity lookup failed closed");
                return ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown);
            },
        }

        let Some(sender) = context
            .senders
            .get(&frame.channel_id())
            .map(|entry| entry.clone())
        else {
            return ack(
                CommandAckStatus::Unavailable,
                CommandLedgerStateCode::Unknown,
            );
        };

        if frame.expires_at_ms() <= frame.issued_at_ms() || now_ms >= frame.expires_at_ms() {
            return match ledger
                .admit_for_channel(
                    command_id,
                    frame.channel_id(),
                    semantic_digest,
                    aether_domain::TimestampMs::new(frame.expires_at_ms()),
                )
                .await
            {
                Ok(CommandLedgerAdmission::Conflict) => {
                    ack(CommandAckStatus::Conflict, CommandLedgerStateCode::Unknown)
                },
                Ok(CommandLedgerAdmission::Same(record)) => {
                    Self::ack_existing(command_id, &record, now_ms)
                },
                Ok(CommandLedgerAdmission::New(_)) => {
                    match ledger
                        .transition(
                            command_id,
                            CommandLedgerState::Received,
                            CommandLedgerState::Expired,
                        )
                        .await
                    {
                        Ok(CommandLedgerTransition::Updated(record)) => {
                            ack(CommandAckStatus::Expired, ledger_state_code(record.state()))
                        },
                        Ok(CommandLedgerTransition::NotUpdated(record)) => {
                            ack(CommandAckStatus::Expired, ledger_state_code(record.state()))
                        },
                        Ok(CommandLedgerTransition::Missing) => {
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                        Err(error) => {
                            warn!(%error, "ShmListener: failed to persist expired command state");
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                    }
                },
                Err(error) => {
                    warn!(%error, "ShmListener: failed to persist expired command");
                    ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                },
            };
        }

        // Capacity is reserved first so backpressure remains retryable and does
        // not consume a durable identity. The command is not visible to the
        // channel task until Received and Queued have both committed.
        let permit = match tokio::time::timeout(
            std::time::Duration::from_millis(50),
            sender.reserve(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return ack(
                    CommandAckStatus::Unavailable,
                    CommandLedgerStateCode::Unknown,
                );
            },
            Err(_) => {
                context.dropped_count.fetch_add(1, Ordering::Relaxed);
                return ack(
                    CommandAckStatus::Backpressure,
                    CommandLedgerStateCode::Unknown,
                );
            },
        };

        let admission = match ledger
            .admit_for_channel(
                command_id,
                frame.channel_id(),
                semantic_digest,
                aether_domain::TimestampMs::new(frame.expires_at_ms()),
            )
            .await
        {
            Ok(admission) => admission,
            Err(error) => {
                warn!(%error, "ShmListener: durable admission failed closed");
                return ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown);
            },
        };
        match admission {
            CommandLedgerAdmission::Conflict => {
                ack(CommandAckStatus::Conflict, CommandLedgerStateCode::Unknown)
            },
            CommandLedgerAdmission::Same(record) => Self::ack_existing(command_id, &record, now_ms),
            CommandLedgerAdmission::New(_) => {
                if Self::now_ms() >= frame.expires_at_ms() {
                    return match ledger
                        .transition(
                            command_id,
                            CommandLedgerState::Received,
                            CommandLedgerState::Expired,
                        )
                        .await
                    {
                        Ok(CommandLedgerTransition::Updated(record))
                        | Ok(CommandLedgerTransition::NotUpdated(record)) => {
                            ack(CommandAckStatus::Expired, ledger_state_code(record.state()))
                        },
                        Ok(CommandLedgerTransition::Missing) => {
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                        Err(error) => {
                            warn!(%error, "ShmListener: failed to persist expiry after identity admission");
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                    };
                }
                let queued = match ledger
                    .transition(
                        command_id,
                        CommandLedgerState::Received,
                        CommandLedgerState::Queued,
                    )
                    .await
                {
                    Ok(CommandLedgerTransition::Updated(record)) => record,
                    Ok(CommandLedgerTransition::NotUpdated(record)) => {
                        return ack(CommandAckStatus::Busy, ledger_state_code(record.state()));
                    },
                    Ok(CommandLedgerTransition::Missing) => {
                        ledger.record_outcome_persistence_failure();
                        return ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown);
                    },
                    Err(error) => {
                        warn!(%error, "ShmListener: failed to persist Queued state");
                        ledger.record_outcome_persistence_failure();
                        return ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown);
                    },
                };

                // Any SQLite await must happen before taking the synchronous
                // registration fence below.
                if Self::now_ms() >= frame.expires_at_ms() {
                    return match ledger
                        .transition(
                            command_id,
                            CommandLedgerState::Queued,
                            CommandLedgerState::Expired,
                        )
                        .await
                    {
                        Ok(CommandLedgerTransition::Updated(record))
                        | Ok(CommandLedgerTransition::NotUpdated(record)) => {
                            ack(CommandAckStatus::Expired, ledger_state_code(record.state()))
                        },
                        Ok(CommandLedgerTransition::Missing) => {
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                        Err(error) => {
                            warn!(%error, "ShmListener: failed to persist post-queue expiry");
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                    };
                }

                // On success the already-reserved permit is consumed while
                // holding the registration read fence. A failed generation
                // check is persisted only after the guard has been dropped.
                let queue_decision = {
                    let _registration = match context.registration_gate.read() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    let sender_is_current = context
                        .senders
                        .get(&frame.channel_id())
                        .is_some_and(|current| current.same_channel(&sender));
                    if Self::now_ms() >= frame.expires_at_ms() {
                        QueueDecision::Expired
                    } else if context.accepting_commands.load(Ordering::Acquire)
                        && sender_is_current
                    {
                        let command = Self::build_command(point_type, frame);
                        permit.send(command);
                        context
                            .command_outcomes
                            .record(&command_key, CommandLifecycleState::Queued);
                        QueueDecision::Enqueued
                    } else {
                        QueueDecision::Unavailable
                    }
                };

                if queue_decision == QueueDecision::Enqueued {
                    let mut acceptance = None;
                    for attempt in 0..3_u32 {
                        match ledger.mark_accepted(command_id).await {
                            Ok(result) => {
                                acceptance = Some(result);
                                break;
                            },
                            Err(error) => {
                                warn!(
                                    %error,
                                    attempt = attempt + 1,
                                    "ShmListener: queue admission sent but durable acceptance marker failed"
                                );
                                if attempt < 2 {
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        10 * u64::from(attempt + 1),
                                    ))
                                    .await;
                                }
                            },
                        }
                    }
                    match acceptance {
                        Some(CommandLedgerAcceptance::Marked(record))
                        | Some(CommandLedgerAcceptance::AlreadyMarked(record)) => {
                            DeviceCommandAck::new(
                                command_id,
                                CommandAckStatus::Accepted,
                                ledger_state_code(record.state()),
                                record
                                    .accepted_at()
                                    .map(aether_domain::TimestampMs::get)
                                    .unwrap_or(now_ms),
                            )
                        },
                        Some(CommandLedgerAcceptance::NotMarkable(record)) => {
                            ledger.record_outcome_persistence_failure();
                            ack(
                                CommandAckStatus::Internal,
                                ledger_state_code(record.state()),
                            )
                        },
                        Some(CommandLedgerAcceptance::Missing) | None => {
                            ledger.record_outcome_persistence_failure();
                            ack(CommandAckStatus::Internal, CommandLedgerStateCode::Unknown)
                        },
                    }
                } else {
                    let (next, status, diagnostic) = match queue_decision {
                        QueueDecision::Expired => (
                            CommandLedgerState::Expired,
                            CommandAckStatus::Expired,
                            "command expired at the final queue-admission fence",
                        ),
                        QueueDecision::Unavailable => (
                            CommandLedgerState::Failed,
                            CommandAckStatus::Unavailable,
                            "channel registration changed before queue admission",
                        ),
                        QueueDecision::Enqueued => {
                            ledger.record_outcome_persistence_failure();
                            return ack(
                                CommandAckStatus::Internal,
                                CommandLedgerStateCode::Unknown,
                            );
                        },
                    };
                    match ledger
                        .transition_with_diagnostic(
                            command_id,
                            CommandLedgerState::Queued,
                            next,
                            Some(diagnostic),
                        )
                        .await
                    {
                        Ok(CommandLedgerTransition::Updated(record))
                        | Ok(CommandLedgerTransition::NotUpdated(record)) => {
                            ack(status, ledger_state_code(record.state()))
                        },
                        Ok(CommandLedgerTransition::Missing) => {
                            ledger.record_outcome_persistence_failure();
                            ack(
                                CommandAckStatus::Internal,
                                ledger_state_code(queued.state()),
                            )
                        },
                        Err(error) => {
                            warn!(%error, "ShmListener: failed to persist pre-admission terminal state");
                            ledger.record_outcome_persistence_failure();
                            ack(
                                CommandAckStatus::Internal,
                                ledger_state_code(queued.state()),
                            )
                        },
                    }
                }
            },
        }
    }

    fn ack_existing(
        command_id: aether_domain::CommandId,
        record: &CommandLedgerRecord,
        now_ms: u64,
    ) -> DeviceCommandAck {
        let state = ledger_state_code(record.state());
        if let Some(accepted_at) = record.accepted_at() {
            return DeviceCommandAck::new(
                command_id,
                CommandAckStatus::Duplicate,
                state,
                accepted_at.get(),
            );
        }
        let status = match record.state() {
            CommandLedgerState::Expired => CommandAckStatus::Expired,
            CommandLedgerState::Failed => CommandAckStatus::Unavailable,
            CommandLedgerState::Received
            | CommandLedgerState::Queued
            | CommandLedgerState::Dispatching
            | CommandLedgerState::Succeeded
            | CommandLedgerState::PossiblyApplied => CommandAckStatus::Busy,
        };
        DeviceCommandAck::new(command_id, status, state, now_ms)
    }

    /// Returns the number of commands dropped due to channel backpressure.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Return a current bounded-resource snapshot for readiness and operators.
    pub fn stats(&self) -> ShmListenerStats {
        ShmListenerStats {
            running: self.running.load(Ordering::Acquire),
            active_connections: MAX_COMMAND_CONNECTIONS
                .saturating_sub(self.connection_slots.available_permits()),
            connection_capacity: MAX_COMMAND_CONNECTIONS,
            rejected_connections: self.rejected_connections.load(Ordering::Relaxed),
            idle_timeouts: self.idle_timeouts.load(Ordering::Relaxed),
            frames_total: self.frames_total.load(Ordering::Relaxed),
            last_frame_at_ms: {
                let value = self.last_frame_at_ms.load(Ordering::Relaxed);
                (value != 0).then_some(value)
            },
        }
    }

    fn build_command(point_type: PointType, frame: DeviceCommandFrame) -> ChannelCommand {
        let command_id = format!("{:032x}", frame.command_id().get());
        match point_type {
            PointType::Control => ChannelCommand::Control {
                command_id,
                point_id: frame.point_id(),
                value: frame.value(),
                timestamp: frame.issued_at_ms().min(i64::MAX as u64) as i64,
                expires_at_ms: frame.expires_at_ms().min(i64::MAX as u64) as i64,
            },
            _ => ChannelCommand::Adjustment {
                command_id,
                point_id: frame.point_id(),
                value: frame.value(),
                timestamp: frame.issued_at_ms().min(i64::MAX as u64) as i64,
                expires_at_ms: frame.expires_at_ms().min(i64::MAX as u64) as i64,
            },
        }
    }
}

const fn ledger_state_code(state: CommandLedgerState) -> CommandLedgerStateCode {
    match state {
        CommandLedgerState::Received => CommandLedgerStateCode::Received,
        CommandLedgerState::Queued => CommandLedgerStateCode::Queued,
        CommandLedgerState::Dispatching => CommandLedgerStateCode::Dispatching,
        CommandLedgerState::Succeeded => CommandLedgerStateCode::Succeeded,
        CommandLedgerState::Failed => CommandLedgerStateCode::Failed,
        CommandLedgerState::Expired => CommandLedgerStateCode::Expired,
        CommandLedgerState::PossiblyApplied => CommandLedgerStateCode::PossiblyApplied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_domain::{
        ChannelCommandAddress, ChannelId, CommandId, PhysicalDeviceCommand, PointId,
    };
    use sqlx::sqlite::SqlitePoolOptions;

    fn command_frame(
        command_id: u128,
        value: f64,
        issued_at_ms: u64,
        expires_at_ms: u64,
    ) -> DeviceCommandFrame {
        let command = PhysicalDeviceCommand::new(
            CommandId::new(command_id),
            ChannelCommandAddress::new(
                ChannelId::new(7),
                aether_domain::PointKind::Command,
                PointId::new(1),
            )
            .expect("command address"),
            value,
            aether_domain::TimestampMs::new(issued_at_ms),
            aether_domain::TimestampMs::new(expires_at_ms),
        )
        .expect("physical command");
        DeviceCommandFrame::new(command).expect("command frame")
    }

    async fn command_context() -> (
        ConnectionContext,
        mpsc::Receiver<ChannelCommand>,
        Arc<CommandLedger>,
    ) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open command ledger");
        let ledger = Arc::new(
            CommandLedger::initialize(pool)
                .await
                .expect("initialize ledger"),
        );
        let senders = Arc::new(DashMap::new());
        let (sender, receiver) = mpsc::channel(4);
        senders.insert(7, sender);
        (
            ConnectionContext {
                senders,
                dropped_count: Arc::new(AtomicU64::new(0)),
                idle_timeouts: Arc::new(AtomicU64::new(0)),
                accepting_commands: Arc::new(AtomicBool::new(true)),
                registration_gate: Arc::new(RwLock::new(())),
                command_outcomes: Arc::new(CommandOutcomeTracker::with_capacity(8)),
                command_ledger: Arc::clone(&ledger),
                frames_total: Arc::new(AtomicU64::new(0)),
                last_frame_at_ms: Arc::new(AtomicU64::new(0)),
            },
            receiver,
            ledger,
        )
    }

    #[tokio::test]
    async fn unix_listener_durably_acks_exact_retry_once_and_rejects_conflict() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite");
        let ledger = Arc::new(
            CommandLedger::initialize(pool)
                .await
                .expect("command ledger"),
        );
        let directory = tempfile::Builder::new()
            .prefix("aether-command-")
            .tempdir_in("/tmp")
            .expect("short socket directory");
        let path = directory.path().join("m2c.sock");
        let path = path.to_string_lossy().into_owned();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let listener = Arc::new(ShmCommandListener::with_outcomes_and_ledger(
            Some(&path),
            shutdown_rx,
            Arc::new(CommandOutcomeTracker::with_capacity(8)),
            Arc::clone(&ledger),
        ));
        let (sender, mut receiver) = mpsc::channel(4);
        listener.register_channel(7, sender);
        let prepared = listener.prepare().expect("prebind command socket");
        let run_listener = Arc::clone(&listener);
        let task = tokio::spawn(async move { run_listener.run_prepared(prepared).await });

        let mut stream = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect command listener");
        let mut hello_bytes = [0_u8; CommandHello::SIZE];
        stream
            .read_exact(&mut hello_bytes)
            .await
            .expect("read command hello");
        CommandHello::from_bytes(&hello_bytes).expect("valid command hello");

        let now_ms = ShmCommandListener::now_ms();
        let frame = command_frame(0x41, 1.0, now_ms, now_ms + 5_000);
        let semantic_retry = command_frame(0x41, 1.0, now_ms + 1, now_ms + 6_000);
        stream
            .write_all(&frame.to_bytes())
            .await
            .expect("write first command");
        let mut ack_bytes = [0_u8; DeviceCommandAck::SIZE];
        stream
            .read_exact(&mut ack_bytes)
            .await
            .expect("read accepted ACK");
        let accepted = DeviceCommandAck::from_bytes(&ack_bytes).expect("accepted ACK");
        assert_eq!(accepted.status(), CommandAckStatus::Accepted);
        assert_eq!(accepted.state(), CommandLedgerStateCode::Queued);

        stream
            .write_all(&semantic_retry.to_bytes())
            .await
            .expect("write same semantic operation with refreshed timestamps");
        stream
            .read_exact(&mut ack_bytes)
            .await
            .expect("read duplicate ACK");
        let duplicate = DeviceCommandAck::from_bytes(&ack_bytes).expect("duplicate ACK");
        assert_eq!(duplicate.status(), CommandAckStatus::Duplicate);
        assert_eq!(duplicate.recorded_at_ms(), accepted.recorded_at_ms());
        assert_eq!(
            ledger
                .query(CommandId::new(0x41))
                .await
                .expect("query original admission")
                .expect("retained admission")
                .expires_at()
                .get(),
            now_ms + 5_000,
            "a semantic retry must not extend the original deadline"
        );

        let conflict = command_frame(0x41, 2.0, now_ms, now_ms + 5_000);
        stream
            .write_all(&conflict.to_bytes())
            .await
            .expect("write conflicting retry");
        stream
            .read_exact(&mut ack_bytes)
            .await
            .expect("read conflict ACK");
        let conflict_ack = DeviceCommandAck::from_bytes(&ack_bytes).expect("conflict ACK");
        assert_eq!(conflict_ack.status(), CommandAckStatus::Conflict);

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("first command queued")
            .expect("command receiver open");
        assert_eq!(received.durable_command_id(), Some(CommandId::new(0x41)));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
                .await
                .is_err(),
            "exact retry and digest conflict must not enqueue another command"
        );
        let stats = listener.stats();
        assert_eq!(stats.frames_total, 3);
        assert!(stats.last_frame_at_ms.is_some());

        drop(stream);
        let _ = shutdown_tx.send(true);
        task.await
            .expect("join listener")
            .expect("listener shutdown");
    }

    #[tokio::test]
    async fn ack_follows_durable_queue_admission_and_duplicate_is_idempotent() {
        let (context, mut receiver, ledger) = command_context().await;
        let now = ShmCommandListener::now_ms();
        let frame = command_frame(0x1234, 12.5, now, now + 5_000);

        let accepted = ShmCommandListener::handle_notification(frame, &context).await;
        assert_eq!(accepted.status(), CommandAckStatus::Accepted);
        assert_eq!(accepted.state(), CommandLedgerStateCode::Queued);
        let queued = receiver.recv().await.expect("one queued command");
        assert_eq!(queued.command_id(), "00000000000000000000000000001234");
        assert_eq!(
            ledger
                .query(frame.command_id())
                .await
                .expect("query ledger")
                .expect("ledger record")
                .state(),
            CommandLedgerState::Queued
        );

        let duplicate = ShmCommandListener::handle_notification(frame, &context).await;
        assert_eq!(duplicate.status(), CommandAckStatus::Duplicate);
        assert_eq!(duplicate.state(), CommandLedgerStateCode::Queued);
        assert!(
            receiver.try_recv().is_err(),
            "duplicate must not be queued twice"
        );
    }

    #[tokio::test]
    async fn same_id_with_different_canonical_payload_is_conflict() {
        let (context, mut receiver, _) = command_context().await;
        let now = ShmCommandListener::now_ms();
        let first = command_frame(0x2234, 1.0, now, now + 5_000);
        let conflicting = command_frame(0x2234, 2.0, now, now + 5_000);

        assert_eq!(
            ShmCommandListener::handle_notification(first, &context)
                .await
                .status(),
            CommandAckStatus::Accepted
        );
        let _ = receiver.recv().await.expect("first command queued");
        let conflict = ShmCommandListener::handle_notification(conflicting, &context).await;
        assert_eq!(conflict.status(), CommandAckStatus::Conflict);
        assert!(
            receiver.try_recv().is_err(),
            "conflict must not enter the queue"
        );
    }

    #[tokio::test]
    async fn expired_command_is_terminally_recorded_without_queueing() {
        let (context, mut receiver, ledger) = command_context().await;
        let frame = command_frame(0x3234, 1.0, 1, 2);

        let expired = ShmCommandListener::handle_notification(frame, &context).await;
        assert_eq!(expired.status(), CommandAckStatus::Expired);
        assert_eq!(expired.state(), CommandLedgerStateCode::Expired);
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            ledger
                .query(frame.command_id())
                .await
                .expect("query ledger")
                .expect("ledger record")
                .state(),
            CommandLedgerState::Expired
        );
    }

    #[tokio::test]
    async fn prequeue_failure_retry_is_a_nack_not_false_duplicate_success() {
        let (context, mut receiver, ledger) = command_context().await;
        let now = ShmCommandListener::now_ms();
        let frame = command_frame(0x4234, 1.0, now, now + 5_000);
        ledger
            .admit_for_channel(
                frame.command_id(),
                frame.channel_id(),
                frame.semantic_digest(),
                aether_domain::TimestampMs::new(frame.expires_at_ms()),
            )
            .await
            .expect("admit fixture");
        ledger
            .transition(
                frame.command_id(),
                CommandLedgerState::Received,
                CommandLedgerState::Queued,
            )
            .await
            .expect("queue fixture");
        ledger
            .transition_with_diagnostic(
                frame.command_id(),
                CommandLedgerState::Queued,
                CommandLedgerState::Failed,
                Some("registration changed before enqueue"),
            )
            .await
            .expect("fail before enqueue");

        let retry = ShmCommandListener::handle_notification(frame, &context).await;
        assert_eq!(retry.status(), CommandAckStatus::Unavailable);
        assert_eq!(retry.state(), CommandLedgerStateCode::Failed);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn concurrent_duplicate_before_acceptance_marker_is_busy() {
        let (context, mut receiver, ledger) = command_context().await;
        let now = ShmCommandListener::now_ms();
        let frame = command_frame(0x5234, 1.0, now, now + 5_000);
        ledger
            .admit_for_channel(
                frame.command_id(),
                frame.channel_id(),
                frame.semantic_digest(),
                aether_domain::TimestampMs::new(frame.expires_at_ms()),
            )
            .await
            .expect("admit fixture");
        ledger
            .transition(
                frame.command_id(),
                CommandLedgerState::Received,
                CommandLedgerState::Queued,
            )
            .await
            .expect("queue fixture");

        let retry = ShmCommandListener::handle_notification(frame, &context).await;
        assert_eq!(retry.status(), CommandAckStatus::Busy);
        assert_eq!(retry.state(), CommandLedgerStateCode::Queued);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn send_before_acceptance_marker_crash_is_not_reported_as_accepted() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open command ledger");
        let ledger = CommandLedger::initialize(pool.clone())
            .await
            .expect("initialize ledger");
        let now = ShmCommandListener::now_ms();
        let frame = command_frame(0x6234, 1.0, now, now + 5_000);
        ledger
            .admit_for_channel(
                frame.command_id(),
                frame.channel_id(),
                frame.semantic_digest(),
                aether_domain::TimestampMs::new(frame.expires_at_ms()),
            )
            .await
            .expect("admit fixture");
        ledger
            .transition(
                frame.command_id(),
                CommandLedgerState::Received,
                CommandLedgerState::Queued,
            )
            .await
            .expect("persist queue before simulated crash");
        drop(ledger);

        let reopened = CommandLedger::initialize(pool)
            .await
            .expect("restart ledger");
        let recovered = reopened
            .query(frame.command_id())
            .await
            .expect("query recovery")
            .expect("recovery row");
        let retry = ShmCommandListener::ack_existing(
            frame.command_id(),
            &recovered,
            ShmCommandListener::now_ms(),
        );
        assert_eq!(recovered.state(), CommandLedgerState::PossiblyApplied);
        assert_eq!(recovered.accepted_at(), None);
        assert_eq!(retry.status(), CommandAckStatus::Busy);
        assert_eq!(retry.state(), CommandLedgerStateCode::PossiblyApplied);
    }

    #[tokio::test]
    async fn accepted_terminal_retry_returns_original_admission_timestamp() {
        let (context, mut receiver, ledger) = command_context().await;
        let now = ShmCommandListener::now_ms();
        let frame = command_frame(0x7234, 1.0, now, now + 5_000);
        ledger
            .admit_for_channel(
                frame.command_id(),
                frame.channel_id(),
                frame.semantic_digest(),
                aether_domain::TimestampMs::new(frame.expires_at_ms()),
            )
            .await
            .expect("admit fixture");
        ledger
            .transition(
                frame.command_id(),
                CommandLedgerState::Received,
                CommandLedgerState::Queued,
            )
            .await
            .expect("queue fixture");
        let accepted_at = match ledger
            .mark_accepted(frame.command_id())
            .await
            .expect("mark accepted")
        {
            CommandLedgerAcceptance::Marked(record) => record.accepted_at().unwrap().get(),
            other => panic!("unexpected acceptance result: {other:?}"),
        };
        ledger
            .transition(
                frame.command_id(),
                CommandLedgerState::Queued,
                CommandLedgerState::Dispatching,
            )
            .await
            .expect("dispatch fixture");
        ledger
            .transition(
                frame.command_id(),
                CommandLedgerState::Dispatching,
                CommandLedgerState::Succeeded,
            )
            .await
            .expect("success fixture");

        let retry = ShmCommandListener::handle_notification(frame, &context).await;
        assert_eq!(retry.status(), CommandAckStatus::Duplicate);
        assert_eq!(retry.state(), CommandLedgerStateCode::Succeeded);
        assert_eq!(retry.recorded_at_ms(), accepted_at);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn active_command_socket_is_rejected_during_endpoint_reservation() {
        let directory = tempfile::tempdir().expect("command socket directory");
        let path = directory.path().join("m2c.sock");
        let active = std::os::unix::net::UnixListener::bind(&path).expect("active listener");

        let error = ShmCommandListener::prepare_path(path.to_str())
            .expect_err("a live command owner must make startup fail before activation");

        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert!(
            std::os::unix::net::UnixStream::connect(&path).is_ok(),
            "failed reservation must not remove the incumbent command socket"
        );
        drop(active);
    }

    #[tokio::test]
    async fn prepared_listener_drop_does_not_remove_a_replacement_socket() {
        let directory = tempfile::tempdir().expect("command socket directory");
        let path = directory.path().join("m2c.sock");
        let prepared = ShmCommandListener::prepare_path(path.to_str())
            .expect("reserve original command socket");
        std::fs::remove_file(&path).expect("unlink original socket name");
        let replacement =
            std::os::unix::net::UnixListener::bind(&path).expect("bind replacement listener");

        drop(prepared);

        assert!(
            std::os::unix::net::UnixStream::connect(&path).is_ok(),
            "cleanup must be fenced to the socket inode prepared by this process"
        );
        drop(replacement);
    }
}
