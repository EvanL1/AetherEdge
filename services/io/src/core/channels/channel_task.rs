//! Unified channel task — the async event loop
//!
//! Owns the protocol client exclusively and uses `tokio::select!` to handle:
//! - Protocol commands (connect/disconnect/diagnostics)
//! - Business commands (control/adjustment from M2C SHM)
//! - Periodic polling

use arc_swap::ArcSwapOption;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use aether_core::PointType;

use crate::core::channels::traits::ChannelCommand;
use crate::core::channels::types::ProtocolCommand;
use crate::protocols::core::DataEventIngressObserver;
use crate::protocols::core::data::DataBatch;
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler};
use crate::protocols::core::traits::{DataEvent, DataEventReceiver, PollResult};
use crate::protocols::runtime::ChannelRuntime;
use crate::runtime::reconnect::{
    AutoRecoveryPolicy, ReconnectHelper, ReconnectPolicy, ReconnectState,
};
use crate::store::ShmDataStore;

use super::command_guard::CommandGuard;
use super::command_ledger::{CommandLedger, CommandLedgerState, CommandLedgerTransition};
use super::command_outcome::{CommandDropReason, CommandLifecycleState, CommandOutcomeTracker};

const WATCHDOG_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Shutdown processes at most 64 fair ingress events (16,384 points at the
/// current data drain batch size) before accounting and discarding the rest.
const DATA_EVENT_SHUTDOWN_DRAIN_LIMIT: usize = 64;

/// Mutable task state shared with the lock-free channel query surface.
///
/// Keeping these atomics behind one `Arc` avoids one allocation and reference
/// count pair per field while preserving independent lock-free updates.
pub(super) struct ChannelSharedState {
    pub cached_connection_state: AtomicU8,
    pub cached_diagnostics: ArcSwapOption<crate::protocols::core::traits::Diagnostics>,
    pub watchdog_progress_tick_ms: AtomicI64,
    pub reconnect_total_attempts: AtomicU64,
    pub reconnect_failed: AtomicBool,
    /// Epoch timestamp retained for external status/API reporting.
    pub last_successful_read_ms: AtomicI64,
    /// Process-local monotonic tick used for freshness decisions.
    pub last_successful_read_tick_ms: AtomicI64,
    /// Admission telemetry for the event-driven runtime, if present.
    pub data_event_ingress: ArcSwapOption<DataEventIngressObserver>,
}

impl ChannelSharedState {
    pub fn new(initial_connection_state: u8) -> Self {
        Self {
            cached_connection_state: AtomicU8::new(initial_connection_state),
            cached_diagnostics: ArcSwapOption::empty(),
            watchdog_progress_tick_ms: AtomicI64::new(0),
            reconnect_total_attempts: AtomicU64::new(0),
            reconnect_failed: AtomicBool::new(false),
            last_successful_read_ms: AtomicI64::new(0),
            last_successful_read_tick_ms: AtomicI64::new(0),
            data_event_ingress: ArcSwapOption::empty(),
        }
    }
}

/// Shared immutable context for channel polling operations.
///
/// Groups the Arc/Atomic fields that are threaded unchanged through the poll loop,
/// reducing function signatures from 14+ params to ≤ 6.
pub(super) struct ChannelPollContext {
    pub store: Arc<ShmDataStore>,
    pub channel_id: u32,
    pub poll_interval_ms: NonZeroU64,
    pub shared: Arc<ChannelSharedState>,
    pub log_handler: Arc<dyn ChannelLogHandler>,
    /// Consecutive zero-data poll cycles before triggering disconnect (0 = disabled)
    pub zero_data_threshold: u32,
    /// Final validation and post-acceptance lifecycle observation.
    pub commands: CommandDispatchContext,
}

pub(super) struct CommandDispatchContext {
    pub guard: CommandGuard,
    pub outcomes: Arc<CommandOutcomeTracker>,
    pub ledger: Option<Arc<CommandLedger>>,
}

struct CommandCompletion<'a> {
    tracker: &'a CommandOutcomeTracker,
    command_id: String,
    completed: bool,
}

impl<'a> CommandCompletion<'a> {
    fn new(tracker: &'a CommandOutcomeTracker, command_id: String) -> Self {
        Self {
            tracker,
            command_id,
            completed: false,
        }
    }

    fn complete(mut self, state: CommandLifecycleState) {
        self.tracker.record(&self.command_id, state);
        self.completed = true;
    }
}

impl Drop for CommandCompletion<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.tracker.record(
                &self.command_id,
                CommandLifecycleState::DeviceFailed {
                    diagnostic: "device execution cancelled before an outcome".to_string(),
                },
            );
        }
    }
}

fn rejected_command_state(
    expires_at_ms: i64,
    now_ms: i64,
    error: &impl std::fmt::Display,
) -> CommandLifecycleState {
    if now_ms >= expires_at_ms {
        CommandLifecycleState::Expired
    } else {
        CommandLifecycleState::DeviceFailed {
            diagnostic: format!("rejected before device dispatch: {error}"),
        }
    }
}

/// Update cached connection state from protocol runtime.
fn update_cached_state(state: &dyn ChannelRuntime, cache: &AtomicU8) {
    let channel_state: crate::core::channels::types::ConnectionState =
        state.connection_state().into();
    cache.store(channel_state.as_u8(), Ordering::Relaxed);
}

/// Connect a protocol and activate its event stream as one lifecycle operation.
///
/// Event startup is part of a successful connection: leaving the transport
/// connected after subscription/GI startup fails would expose a false-online
/// channel that can never deliver data.
async fn connect_and_start_events(
    protocol: &mut dyn ChannelRuntime,
) -> crate::protocols::core::Result<()> {
    protocol.connect().await?;
    if protocol.is_event_driven()
        && let Err(error) = protocol.start_events().await
    {
        let _ = protocol.disconnect().await;
        return Err(error);
    }
    Ok(())
}

/// Stop an event stream before closing its underlying transport.
async fn stop_events_and_disconnect(
    protocol: &mut dyn ChannelRuntime,
) -> crate::protocols::core::Result<()> {
    let stop_result = if protocol.is_event_driven() {
        protocol.stop_events().await
    } else {
        Ok(())
    };
    let disconnect_result = protocol.disconnect().await;
    stop_result.and(disconnect_result)
}

/// Wait for the next event without creating a busy loop for polling protocols.
async fn receive_protocol_event(receiver: &mut Option<DataEventReceiver>) -> Option<DataEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// One completed unit selected by the channel task.
enum ChannelWork {
    Poll,
    Heartbeat,
    Protocol(ProtocolCommand),
    Business(ChannelCommand),
    Event(Option<DataEvent>),
}

enum BackoffWait {
    Elapsed,
    Protocol(ProtocolCommand),
}

#[derive(Clone, Copy)]
enum NonPollSource {
    Protocol,
    Business,
    Event,
    Heartbeat,
}

impl NonPollSource {
    const fn after(work: &ChannelWork, current: Self) -> Self {
        match work {
            ChannelWork::Protocol(_) => Self::Business,
            ChannelWork::Business(_) => Self::Event,
            ChannelWork::Event(_) => Self::Heartbeat,
            ChannelWork::Heartbeat => Self::Protocol,
            ChannelWork::Poll => current,
        }
    }
}

/// Select the next unit of work while enforcing the polling deadline.
///
/// A due interval tick is deliberately first in the biased selection. This is
/// deterministic: continuously-ready command/event queues may use the time
/// between deadlines, but cannot keep an overdue acquisition poll from running.
/// Non-poll sources are selected in a rotating deterministic order. Immediately
/// after a poll they get one priority turn, so successive overdue ticks cannot
/// hide protocol shutdown, pushed acquisition data, or the loop heartbeat.
/// With all three external queues continuously ready, each advances within
/// four completed non-poll units, including a due heartbeat (and a queued
/// shutdown already at the protocol FIFO head within four polls if every poll
/// itself consumes a complete interval). If a selected unit gets stuck, the
/// completion-only loop heartbeat lets the watchdog detect it.
async fn next_channel_work(
    poll_interval: &mut tokio::time::Interval,
    heartbeat_interval: &mut tokio::time::Interval,
    protocol_rx: &mut tokio::sync::mpsc::Receiver<ProtocolCommand>,
    business_rx: &mut tokio::sync::mpsc::Receiver<ChannelCommand>,
    event_rx: &mut Option<DataEventReceiver>,
    poll_has_priority: bool,
    next_non_poll_source: NonPollSource,
) -> ChannelWork {
    if poll_has_priority {
        tokio::select! {
            biased;

            _ = poll_interval.tick() => ChannelWork::Poll,
            work = next_non_poll_work(
                heartbeat_interval, protocol_rx, business_rx, event_rx,
                next_non_poll_source,
            ) => work,
        }
    } else {
        tokio::select! {
            biased;

            work = next_non_poll_work(
                heartbeat_interval, protocol_rx, business_rx, event_rx,
                next_non_poll_source,
            ) => work,
            _ = poll_interval.tick() => ChannelWork::Poll,
        }
    }
}

async fn next_non_poll_work(
    heartbeat_interval: &mut tokio::time::Interval,
    protocol_rx: &mut tokio::sync::mpsc::Receiver<ProtocolCommand>,
    business_rx: &mut tokio::sync::mpsc::Receiver<ChannelCommand>,
    event_rx: &mut Option<DataEventReceiver>,
    next_source: NonPollSource,
) -> ChannelWork {
    macro_rules! select_non_poll {
        ($first:ident, $second:ident, $third:ident, $fourth:ident) => {
            tokio::select! {
                biased;

                Some(work) = non_poll_source!(
                    $first, heartbeat_interval, protocol_rx, business_rx, event_rx,
                ) => work,
                Some(work) = non_poll_source!(
                    $second, heartbeat_interval, protocol_rx, business_rx, event_rx,
                ) => work,
                Some(work) = non_poll_source!(
                    $third, heartbeat_interval, protocol_rx, business_rx, event_rx,
                ) => work,
                Some(work) = non_poll_source!(
                    $fourth, heartbeat_interval, protocol_rx, business_rx, event_rx,
                ) => work,
            }
        };
    }
    macro_rules! non_poll_source {
        (Protocol, $heartbeat:ident, $protocol:ident, $business:ident, $event:ident $(,)?) => {
            async { $protocol.recv().await.map(ChannelWork::Protocol) }
        };
        (Business, $heartbeat:ident, $protocol:ident, $business:ident, $event:ident $(,)?) => {
            async { $business.recv().await.map(ChannelWork::Business) }
        };
        (Event, $heartbeat:ident, $protocol:ident, $business:ident, $event:ident $(,)?) => {
            async { Some(ChannelWork::Event(receive_protocol_event($event).await)) }
        };
        (Heartbeat, $heartbeat:ident, $protocol:ident, $business:ident, $event:ident $(,)?) => {
            async {
                $heartbeat.tick().await;
                Some(ChannelWork::Heartbeat)
            }
        };
    }

    // The queue arms return Option so a closed external source disables only
    // that arm instead of turning the event loop into a busy loop.
    match next_source {
        NonPollSource::Protocol => {
            select_non_poll!(Protocol, Business, Event, Heartbeat)
        },
        NonPollSource::Business => {
            select_non_poll!(Business, Event, Heartbeat, Protocol)
        },
        NonPollSource::Event => {
            select_non_poll!(Event, Heartbeat, Protocol, Business)
        },
        NonPollSource::Heartbeat => {
            select_non_poll!(Heartbeat, Protocol, Business, Event)
        },
    }
}

/// Benchmark-only access to the production channel-work selector.
///
/// Keeping the driver here prevents the benchmark target from copying the
/// scheduling policy it is intended to measure. This module is absent from
/// normal service builds.
#[cfg(feature = "bench-support")]
pub mod bench_support {
    use super::*;
    use crate::protocols::core::DataEventAdmission;
    use crate::protocols::core::data_event_ingress::data_event_channel_with_capacity;
    use std::time::Instant;

    /// Progress observed for one non-poll selector lane.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct LaneProgressSample {
        pub completed: u64,
        pub max_non_poll_gap: u64,
    }

    /// Raw measurements from one continuously-ready selector run.
    #[derive(Debug)]
    pub struct SelectorBenchmarkSample {
        pub poll_interval_ns: u64,
        pub heartbeat_interval_ns: u64,
        pub polls: u64,
        pub total_work_units: u64,
        pub poll_gap_ns: Vec<u64>,
        pub protocol: LaneProgressSample,
        pub business: LaneProgressSample,
        pub event: LaneProgressSample,
        pub heartbeat: LaneProgressSample,
    }

    #[derive(Default)]
    struct LaneTracker {
        completed: u64,
        last_non_poll_unit: Option<u64>,
        max_non_poll_gap: u64,
    }

    impl LaneTracker {
        fn record(&mut self, non_poll_unit: u64) {
            let gap = self
                .last_non_poll_unit
                .map_or(non_poll_unit, |last| non_poll_unit.saturating_sub(last));
            self.max_non_poll_gap = self.max_non_poll_gap.max(gap);
            self.last_non_poll_unit = Some(non_poll_unit);
            self.completed = self.completed.saturating_add(1);
        }

        fn finish(mut self, non_poll_units: u64) -> LaneProgressSample {
            if let Some(last) = self.last_non_poll_unit {
                self.max_non_poll_gap = self
                    .max_non_poll_gap
                    .max(non_poll_units.saturating_sub(last));
            }
            LaneProgressSample {
                completed: self.completed,
                max_non_poll_gap: self.max_non_poll_gap,
            }
        }
    }

    fn duration_ns(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    /// Exercise the real selector while protocol, business, and event ingress
    /// remain continuously ready. Timing values are observations only; the
    /// deterministic four-unit external-lane fairness bound is the contract.
    pub async fn run_selector_benchmark(
        poll_interval: Duration,
        heartbeat_interval: Duration,
        target_polls: u64,
    ) -> Result<SelectorBenchmarkSample, String> {
        if poll_interval.is_zero() || heartbeat_interval.is_zero() {
            return Err("selector benchmark intervals must be non-zero".to_string());
        }
        if target_polls < 2 {
            return Err("selector benchmark requires at least two polls".to_string());
        }

        let mut poll_timer = tokio::time::interval(poll_interval);
        poll_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut heartbeat_timer = tokio::time::interval(heartbeat_interval);
        heartbeat_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
        let (protocol_tx, mut protocol_rx) = tokio::sync::mpsc::channel(1);
        protocol_tx
            .try_send(ProtocolCommand::SetLogLevel {
                level: "info".to_string(),
                response_tx,
            })
            .map_err(|error| format!("prime protocol lane: {error}"))?;

        let (business_tx, mut business_rx) = tokio::sync::mpsc::channel(1);
        business_tx
            .try_send(ChannelCommand::Control {
                command_id: "selector-benchmark".to_string(),
                point_id: 1,
                value: 1.0,
                timestamp: 0,
                expires_at_ms: i64::MAX,
            })
            .map_err(|error| format!("prime business lane: {error}"))?;

        let (event_tx, event_rx) = data_event_channel_with_capacity(1);
        if !matches!(
            event_tx.publish(DataEvent::Heartbeat),
            DataEventAdmission::Accepted
        ) {
            return Err("prime event lane was not accepted".to_string());
        }
        let mut event_rx = Some(event_rx);

        let mut poll_has_priority = true;
        let mut next_non_poll_source = NonPollSource::Protocol;
        let mut polls = 0_u64;
        let mut total_work_units = 0_u64;
        let mut non_poll_units = 0_u64;
        let mut last_poll = None;
        let mut poll_gap_ns = Vec::with_capacity(target_polls.saturating_sub(1) as usize);
        let mut protocol = LaneTracker::default();
        let mut business = LaneTracker::default();
        let mut event = LaneTracker::default();
        let mut heartbeat = LaneTracker::default();

        while polls < target_polls {
            let work = next_channel_work(
                &mut poll_timer,
                &mut heartbeat_timer,
                &mut protocol_rx,
                &mut business_rx,
                &mut event_rx,
                poll_has_priority,
                next_non_poll_source,
            )
            .await;
            next_non_poll_source = NonPollSource::after(&work, next_non_poll_source);
            poll_has_priority = !matches!(&work, ChannelWork::Poll);
            total_work_units = total_work_units.saturating_add(1);

            match work {
                ChannelWork::Poll => {
                    let now = Instant::now();
                    if let Some(previous) = last_poll.replace(now) {
                        poll_gap_ns.push(duration_ns(now.saturating_duration_since(previous)));
                    }
                    polls = polls.saturating_add(1);
                },
                ChannelWork::Protocol(command) => {
                    non_poll_units = non_poll_units.saturating_add(1);
                    protocol.record(non_poll_units);
                    protocol_tx
                        .try_send(command)
                        .map_err(|error| format!("refill protocol lane: {error}"))?;
                },
                ChannelWork::Business(command) => {
                    non_poll_units = non_poll_units.saturating_add(1);
                    business.record(non_poll_units);
                    business_tx
                        .try_send(command)
                        .map_err(|error| format!("refill business lane: {error}"))?;
                },
                ChannelWork::Event(Some(event_work)) => {
                    non_poll_units = non_poll_units.saturating_add(1);
                    event.record(non_poll_units);
                    if !matches!(
                        event_tx.publish(event_work),
                        DataEventAdmission::Accepted | DataEventAdmission::Coalesced
                    ) {
                        return Err("refill event lane was rejected".to_string());
                    }
                },
                ChannelWork::Event(None) => {
                    return Err("event lane closed during selector benchmark".to_string());
                },
                ChannelWork::Heartbeat => {
                    non_poll_units = non_poll_units.saturating_add(1);
                    heartbeat.record(non_poll_units);
                },
            }
        }

        let protocol = protocol.finish(non_poll_units);
        let business = business.finish(non_poll_units);
        let event = event.finish(non_poll_units);
        let heartbeat = heartbeat.finish(non_poll_units);
        for (name, lane) in [
            ("protocol", protocol),
            ("business", business),
            ("event", event),
        ] {
            if lane.completed == 0 {
                return Err(format!("{name} lane made no progress"));
            }
            if lane.max_non_poll_gap > 4 {
                return Err(format!(
                    "{name} lane exceeded four non-poll units: {}",
                    lane.max_non_poll_gap
                ));
            }
        }

        Ok(SelectorBenchmarkSample {
            poll_interval_ns: duration_ns(poll_interval),
            heartbeat_interval_ns: duration_ns(heartbeat_interval),
            polls,
            total_work_units,
            poll_gap_ns,
            protocol,
            business,
            event,
            heartbeat,
        })
    }
}

/// Record completion of one event-loop unit without claiming data freshness.
fn mark_loop_progress(shared: &ChannelSharedState) {
    shared.watchdog_progress_tick_ms.store(
        super::channel_entry::monotonic_timestamp_ms(),
        Ordering::Relaxed,
    );
}

/// Wait through a configured reconnect delay without making a healthy task
/// indistinguishable from a hung adapter future.
///
/// Reconnect delays may legally be much longer than the watchdog's stale-task
/// threshold. Completing bounded wait slices is real event-loop progress, so
/// each slice refreshes only the watchdog tick. Data freshness is deliberately
/// untouched: an offline device remains offline throughout the backoff.
async fn wait_reconnect_backoff(
    delay: Duration,
    protocol_rx: &mut tokio::sync::mpsc::Receiver<ProtocolCommand>,
    shared: &ChannelSharedState,
) -> BackoffWait {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return BackoffWait::Elapsed;
        }
        let slice = remaining.min(WATCHDOG_HEARTBEAT_INTERVAL);
        tokio::select! {
            _ = tokio::time::sleep(slice) => {
                mark_loop_progress(shared);
            }
            Some(command) = protocol_rx.recv() => {
                return BackoffWait::Protocol(command);
            }
        }
    }
}

fn write_batch_and_mark_fresh(
    store: &ShmDataStore,
    channel_id: u32,
    batch: &DataBatch,
    shared: &ChannelSharedState,
) -> crate::protocols::core::Result<()> {
    store.write_batch(channel_id, batch)?;
    shared
        .last_successful_read_ms
        .store(super::channel_entry::unix_timestamp_ms(), Ordering::Relaxed);
    shared.last_successful_read_tick_ms.store(
        super::channel_entry::monotonic_timestamp_ms(),
        Ordering::Relaxed,
    );
    Ok(())
}

fn handle_protocol_event(
    event: DataEvent,
    ctx: &ChannelPollContext,
    prev_online: &mut Option<bool>,
) {
    match event {
        DataEvent::DataUpdate(batch) => {
            if !batch.is_empty()
                && let Err(error) = write_batch_and_mark_fresh(
                    ctx.store.as_ref(),
                    ctx.channel_id,
                    &batch,
                    &ctx.shared,
                )
            {
                error!(
                    "Ch{} failed to write event data to SHM: {}",
                    ctx.channel_id, error
                );
            }
        },
        DataEvent::ConnectionChanged(state) => {
            let cached_state: crate::core::channels::types::ConnectionState = state.into();
            ctx.shared
                .cached_connection_state
                .store(cached_state.as_u8(), Ordering::Relaxed);
            let online = state.is_connected();
            if *prev_online != Some(online) {
                *prev_online = Some(online);
                ctx.store.publish_channel_online(ctx.channel_id, online);
            }
        },
        DataEvent::Error(message) => {
            warn!("Ch{} event stream error: {}", ctx.channel_id, message);
        },
        DataEvent::Heartbeat => {},
    }
}

/// Check if channel online state changed and publish it to the SHM health plane.
///
/// Avoids redundant SHM writes by tracking previous state.
fn check_online_change(
    protocol: &dyn ChannelRuntime,
    prev_online: &mut Option<bool>,
    store: &ShmDataStore,
    channel_id: u32,
) {
    let current_online = protocol.connection_state().is_connected();
    if *prev_online != Some(current_online) {
        *prev_online = Some(current_online);
        store.publish_channel_online(channel_id, current_online);
    }
}

/// Apply log level to protocol and log handler.
///
/// Returns Ok for valid levels ("debug"/"info"/"error"), Err for invalid.
fn apply_log_level(
    protocol: &mut dyn ChannelRuntime,
    log_handler: &dyn ChannelLogHandler,
    level: &str,
) -> std::result::Result<(), String> {
    match level {
        "debug" => {
            protocol.set_log_config(ChannelLogConfig::all());
            log_handler.set_log_level("debug");
            Ok(())
        },
        "info" => {
            protocol.set_log_config(ChannelLogConfig::default());
            log_handler.set_log_level("info");
            Ok(())
        },
        "error" => {
            protocol.set_log_config(ChannelLogConfig::errors_only());
            log_handler.set_log_level("info");
            Ok(())
        },
        other => Err(format!(
            "Invalid log level '{}', use: debug/info/error",
            other
        )),
    }
}

/// Run the unified channel task that handles both polling and commands.
///
/// ## Lock-Free Architecture
///
/// This function owns the protocol client exclusively (no shared Mutex).
/// It uses `tokio::select!` to handle multiple event sources:
/// - Timer tick: Execute poll_once() and write data to store
/// - Protocol command: Handle connect/disconnect/diagnostics requests
/// - Business command: Execute write_control/write_adjustment
///
/// This design eliminates lock contention between polling and command execution,
/// reducing command latency from 300ms to <10ms.
pub(super) async fn run_unified_channel_task(
    ctx: ChannelPollContext,
    mut protocol: Box<dyn ChannelRuntime>,
    mut protocol_rx: tokio::sync::mpsc::Receiver<ProtocolCommand>,
    mut business_rx: tokio::sync::mpsc::Receiver<ChannelCommand>,
    reconnect_policy: ReconnectPolicy,
    auto_recovery_policy: Option<AutoRecoveryPolicy>,
) {
    let event_driven = protocol.is_event_driven();
    // Take the receiver before connecting so startup events (notably IEC 104
    // GI data) cannot race ahead of the runtime consumer.
    let mut event_rx = if event_driven {
        protocol.take_event_receiver()
    } else {
        None
    };
    ctx.shared.data_event_ingress.store(
        event_rx
            .as_ref()
            .map(|receiver| Arc::new(receiver.observer())),
    );

    info!(
        "Ch{} unified task started (interval: {}ms, reconnect: max_attempts={}, initial_delay={:?})",
        ctx.channel_id,
        ctx.poll_interval_ms,
        reconnect_policy.max_attempts,
        reconnect_policy.initial_delay
    );

    // Create reconnection helper for auto-reconnect functionality
    let mut reconnect_helper = ReconnectHelper::new(reconnect_policy);
    if let Some(policy) = auto_recovery_policy {
        reconnect_helper = reconnect_helper.with_auto_recovery(policy);
    }

    // Track previous online state for change detection.
    let mut prev_online: Option<bool> = None;

    // Wait a bit for the connection to be established
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Update initial connection state
    update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
    check_online_change(
        protocol.as_ref(),
        &mut prev_online,
        &ctx.store,
        ctx.channel_id,
    );

    // Use configured poll interval
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(
        ctx.poll_interval_ms.get(),
    ));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut heartbeat_interval = tokio::time::interval(WATCHDOG_HEARTBEAT_INTERVAL);
    heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Track previous error count to detect new errors
    let mut prev_error_count: u64 = 0;

    // Track consecutive poll cycles with zero successful data (for liveness detection)
    let mut consecutive_zero_data: u32 = 0;

    // Track failed state log frequency (per-channel, not static)
    let mut failed_log_tick_counter: u32 = 0;

    // Establish a watchdog baseline before awaiting the first unit. Later
    // heartbeats are written only after a unit completes, so a stuck poll or
    // device command remains detectable.
    mark_loop_progress(&ctx.shared);
    let mut poll_has_priority = true;
    let mut next_non_poll_source = NonPollSource::Protocol;

    loop {
        let work = next_channel_work(
            &mut interval,
            &mut heartbeat_interval,
            &mut protocol_rx,
            &mut business_rx,
            &mut event_rx,
            poll_has_priority,
            next_non_poll_source,
        )
        .await;
        next_non_poll_source = NonPollSource::after(&work, next_non_poll_source);
        poll_has_priority = !matches!(&work, ChannelWork::Poll);

        let should_break = match work {
            ChannelWork::Protocol(cmd) => {
                // Shutdown must break the outer loop; handle_protocol_command
                // would only log it (the backoff branch handles its own copy).
                if matches!(cmd, ProtocolCommand::Shutdown) {
                    info!("Ch{} shutdown received, exiting loop", ctx.channel_id);
                    true
                } else {
                    handle_protocol_command(cmd, &mut protocol, &ctx.log_handler, ctx.channel_id)
                        .await
                }
            },
            ChannelWork::Business(cmd) => {
                handle_business_command(cmd, &mut protocol, &ctx).await;
                false
            },
            ChannelWork::Event(event) => {
                match event {
                    Some(event) => handle_protocol_event(event, &ctx, &mut prev_online),
                    None => {
                        warn!("Ch{} event queue closed", ctx.channel_id);
                        event_rx = None;
                    },
                }
                false
            },
            ChannelWork::Poll => {
                let action = handle_poll_tick(
                    &ctx,
                    &mut protocol,
                    &mut protocol_rx,
                    &mut reconnect_helper,
                    &mut failed_log_tick_counter,
                    &mut prev_online,
                    &mut prev_error_count,
                    &mut consecutive_zero_data,
                )
                .await;
                matches!(action, TickAction::Break)
            },
            ChannelWork::Heartbeat => false,
        };

        // Commands and protocol events are task progress too. Updating only on
        // poll ticks makes a healthy, command-heavy loop look dead to watchdog.
        // This update occurs after awaited work, so a genuinely stuck adapter
        // still leaves a stale heartbeat.
        mark_loop_progress(&ctx.shared);
        if should_break {
            break;
        }
    }

    business_rx.close();
    while let Ok(command) = business_rx.try_recv() {
        let outcome = CommandLifecycleState::Dropped(CommandDropReason::ServiceStopping);
        persist_pre_dispatch_outcome(&ctx, command.durable_command_id(), &outcome).await;
        ctx.commands.outcomes.record(command.command_id(), outcome);
    }

    if let Some(mut receiver) = event_rx.take() {
        let (drained, discarded) = receiver.close_and_drain(DATA_EVENT_SHUTDOWN_DRAIN_LIMIT);
        for event in drained {
            handle_protocol_event(event, &ctx, &mut prev_online);
        }
        if discarded > 0 {
            warn!(
                "Ch{} discarded {} bounded protocol events during shutdown",
                ctx.channel_id, discarded
            );
        }
    }

    // Stop protocol background tasks (e.g. CAN receive/read loops) on any exit path.
    let _ = stop_events_and_disconnect(protocol.as_mut()).await;

    // Mark as disconnected on shutdown
    ctx.shared.cached_connection_state.store(
        crate::core::channels::types::ConnectionState::Disconnected.as_u8(),
        Ordering::Relaxed,
    );
    // Publish offline status to the SHM health plane on shutdown.
    ctx.store.publish_channel_online(ctx.channel_id, false);
    info!("Ch{} unified task stopped", ctx.channel_id);
}

/// Action returned by poll tick handler
enum TickAction {
    /// Continue to next select iteration (skip remaining tick logic)
    Continue,
    /// Break out of the main loop (shutdown)
    Break,
    /// Proceed with normal post-tick processing
    Proceed,
}

/// Handle a protocol command from the command channel.
///
/// Returns `true` when the unified task should exit (Shutdown received).
async fn handle_protocol_command(
    cmd: ProtocolCommand,
    protocol: &mut Box<dyn ChannelRuntime>,
    log_handler: &Arc<dyn ChannelLogHandler>,
    channel_id: u32,
) -> bool {
    match cmd {
        ProtocolCommand::Connect { response_tx } => {
            let result = connect_and_start_events(protocol.as_mut()).await;
            let _ = response_tx.send(result);
        },
        ProtocolCommand::Disconnect { response_tx } => {
            let _ = stop_events_and_disconnect(protocol.as_mut()).await;
            let _ = response_tx.send(());
        },
        ProtocolCommand::SetLogLevel { level, response_tx } => {
            let result = apply_log_level(protocol.as_mut(), log_handler.as_ref(), &level);
            if result.is_ok() {
                info!("Ch{} log level set to {}", channel_id, level);
            }
            let _ = response_tx.send(result);
        },
        ProtocolCommand::Shutdown => {
            // Unreachable: the main select! arm peels Shutdown off before
            // dispatching here. Kept for exhaustiveness; if hit, the loop
            // wasn't broken correctly.
            debug_assert!(false, "Shutdown should be handled in select! arm");
            info!("Ch{} unexpected shutdown in handler", channel_id);
            let _ = protocol.disconnect().await;
            return true;
        },
    }
    false
}

/// Handle a business command (control/adjustment from M2C SHM).
async fn handle_business_command(
    cmd: ChannelCommand,
    protocol: &mut Box<dyn ChannelRuntime>,
    ctx: &ChannelPollContext,
) {
    let channel_id = ctx.channel_id;
    let now_ms = super::channel_entry::unix_timestamp_ms();
    let durable_id = cmd.durable_command_id();
    let completion =
        CommandCompletion::new(ctx.commands.outcomes.as_ref(), cmd.command_id().to_owned());
    match cmd {
        ChannelCommand::Control {
            command_id,
            point_id,
            value,
            timestamp,
            expires_at_ms,
        } => {
            if let Err(error) = ctx.commands.guard.validate(
                PointType::Control,
                point_id,
                value,
                timestamp,
                expires_at_ms,
                now_ms,
            ) {
                warn!(
                    "Ch{} command {} rejected before control pt{} dispatch: {}",
                    channel_id, command_id, point_id, error
                );
                let state = rejected_command_state(expires_at_ms, now_ms, &error);
                persist_pre_dispatch_outcome(ctx, durable_id, &state).await;
                completion.complete(state);
                return;
            }
            if !begin_durable_dispatch(ctx, durable_id).await {
                completion.complete(CommandLifecycleState::DeviceFailed {
                    diagnostic: "durable ledger refused device dispatch".to_string(),
                });
                return;
            }
            if expire_at_adapter_boundary(ctx, durable_id, expires_at_ms).await {
                completion.complete(CommandLifecycleState::Expired);
                return;
            }
            let outcome = match protocol.write_control(&[(point_id, value)]).await {
                Ok(n) if n > 0 => {
                    debug!("Ch{} control pt{} = {} ok", channel_id, point_id, value);
                    CommandLifecycleState::DeviceSucceeded { writes: n }
                },
                Ok(_) => {
                    warn!("Ch{} control pt{} = {} failed", channel_id, point_id, value);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: "device reported zero successful control writes".to_string(),
                    }
                },
                Err(e) => {
                    error!("Ch{} control pt{} err: {}", channel_id, point_id, e);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: e.to_string(),
                    }
                },
            };
            persist_post_dispatch_outcome(ctx, durable_id, &outcome).await;
            completion.complete(outcome);
        },
        ChannelCommand::Adjustment {
            command_id,
            point_id,
            value,
            timestamp,
            expires_at_ms,
        } => {
            if let Err(error) = ctx.commands.guard.validate(
                PointType::Adjustment,
                point_id,
                value,
                timestamp,
                expires_at_ms,
                now_ms,
            ) {
                warn!(
                    "Ch{} command {} rejected before adjustment pt{} dispatch: {}",
                    channel_id, command_id, point_id, error
                );
                let state = rejected_command_state(expires_at_ms, now_ms, &error);
                persist_pre_dispatch_outcome(ctx, durable_id, &state).await;
                completion.complete(state);
                return;
            }
            if !begin_durable_dispatch(ctx, durable_id).await {
                completion.complete(CommandLifecycleState::DeviceFailed {
                    diagnostic: "durable ledger refused device dispatch".to_string(),
                });
                return;
            }
            if expire_at_adapter_boundary(ctx, durable_id, expires_at_ms).await {
                completion.complete(CommandLifecycleState::Expired);
                return;
            }
            let outcome = match protocol.write_adjustment(&[(point_id, value)]).await {
                Ok(n) if n > 0 => {
                    debug!("Ch{} adjustment pt{} = {} ok", channel_id, point_id, value);
                    CommandLifecycleState::DeviceSucceeded { writes: n }
                },
                Ok(_) => {
                    warn!(
                        "Ch{} adjustment pt{} = {} failed",
                        channel_id, point_id, value
                    );
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: "device reported zero successful adjustment writes".to_string(),
                    }
                },
                Err(e) => {
                    error!("Ch{} adjustment pt{} err: {}", channel_id, point_id, e);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: e.to_string(),
                    }
                },
            };
            persist_post_dispatch_outcome(ctx, durable_id, &outcome).await;
            completion.complete(outcome);
        },
        ChannelCommand::BatchControl {
            command_id,
            points,
            timestamp,
            expires_at_ms,
        } => {
            if let Some((point_id, error)) = points.iter().find_map(|(point_id, value)| {
                ctx.commands
                    .guard
                    .validate(
                        PointType::Control,
                        *point_id,
                        *value,
                        timestamp,
                        expires_at_ms,
                        now_ms,
                    )
                    .err()
                    .map(|error| (*point_id, error))
            }) {
                warn!(
                    "Ch{} batch command {} rejected at control pt{}: {}",
                    channel_id, command_id, point_id, error
                );
                let state = rejected_command_state(expires_at_ms, now_ms, &error);
                persist_pre_dispatch_outcome(ctx, durable_id, &state).await;
                completion.complete(state);
                return;
            }
            if !begin_durable_dispatch(ctx, durable_id).await {
                completion.complete(CommandLifecycleState::DeviceFailed {
                    diagnostic: "durable ledger refused device dispatch".to_string(),
                });
                return;
            }
            if expire_at_adapter_boundary(ctx, durable_id, expires_at_ms).await {
                completion.complete(CommandLifecycleState::Expired);
                return;
            }
            let expected = points.len();
            let outcome = match protocol.write_control(&points).await {
                Ok(n) if n == expected => {
                    debug!("Ch{} batch control {}/{} ok", channel_id, n, expected);
                    CommandLifecycleState::DeviceSucceeded { writes: n }
                },
                Ok(n) => {
                    warn!("Ch{} batch control {}/{} partial", channel_id, n, expected);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: format!("device completed {n}/{expected} control writes"),
                    }
                },
                Err(e) => {
                    error!("Ch{} batch control err: {}", channel_id, e);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: e.to_string(),
                    }
                },
            };
            persist_post_dispatch_outcome(ctx, durable_id, &outcome).await;
            completion.complete(outcome);
        },
        ChannelCommand::BatchAdjustment {
            command_id,
            points,
            timestamp,
            expires_at_ms,
        } => {
            if let Some((point_id, error)) = points.iter().find_map(|(point_id, value)| {
                ctx.commands
                    .guard
                    .validate(
                        PointType::Adjustment,
                        *point_id,
                        *value,
                        timestamp,
                        expires_at_ms,
                        now_ms,
                    )
                    .err()
                    .map(|error| (*point_id, error))
            }) {
                warn!(
                    "Ch{} batch command {} rejected at adjustment pt{}: {}",
                    channel_id, command_id, point_id, error
                );
                let state = rejected_command_state(expires_at_ms, now_ms, &error);
                persist_pre_dispatch_outcome(ctx, durable_id, &state).await;
                completion.complete(state);
                return;
            }
            if !begin_durable_dispatch(ctx, durable_id).await {
                completion.complete(CommandLifecycleState::DeviceFailed {
                    diagnostic: "durable ledger refused device dispatch".to_string(),
                });
                return;
            }
            if expire_at_adapter_boundary(ctx, durable_id, expires_at_ms).await {
                completion.complete(CommandLifecycleState::Expired);
                return;
            }
            let expected = points.len();
            let outcome = match protocol.write_adjustment(&points).await {
                Ok(n) if n == expected => {
                    debug!("Ch{} batch adj {}/{} ok", channel_id, n, expected);
                    CommandLifecycleState::DeviceSucceeded { writes: n }
                },
                Ok(n) => {
                    warn!("Ch{} batch adj {}/{} partial", channel_id, n, expected);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: format!("device completed {n}/{expected} adjustment writes"),
                    }
                },
                Err(e) => {
                    error!("Ch{} batch adj err: {}", channel_id, e);
                    CommandLifecycleState::DeviceFailed {
                        diagnostic: e.to_string(),
                    }
                },
            };
            persist_post_dispatch_outcome(ctx, durable_id, &outcome).await;
            completion.complete(outcome);
        },
    }
}

async fn expire_at_adapter_boundary(
    ctx: &ChannelPollContext,
    command_id: Option<aether_domain::CommandId>,
    expires_at_ms: i64,
) -> bool {
    let now_ms = super::channel_entry::unix_timestamp_ms();
    if now_ms > 0 && now_ms < expires_at_ms {
        return false;
    }
    if let (Some(ledger), Some(command_id)) = (&ctx.commands.ledger, command_id) {
        // Dispatching was already durably committed. Keep the general state
        // machine conservative and close this narrow expiry race as
        // PossiblyApplied, while the diagnostic proves this consumer never
        // invoked the adapter.
        persist_terminal_outcome(
            ledger,
            command_id,
            CommandLedgerState::Dispatching,
            CommandLedgerState::PossiblyApplied,
            Some(if now_ms <= 0 {
                "system clock unavailable after dispatch admission; adapter was not invoked"
            } else {
                "command expired after dispatch admission; adapter was not invoked"
            }),
        )
        .await;
    }
    true
}

async fn begin_durable_dispatch(
    ctx: &ChannelPollContext,
    command_id: Option<aether_domain::CommandId>,
) -> bool {
    let (Some(ledger), Some(command_id)) = (&ctx.commands.ledger, command_id) else {
        return true;
    };
    const ATTEMPTS: u32 = 3;
    for attempt in 0..ATTEMPTS {
        match ledger
            .transition(
                command_id,
                CommandLedgerState::Queued,
                CommandLedgerState::Dispatching,
            )
            .await
        {
            Ok(CommandLedgerTransition::Updated(_)) => return true,
            Ok(CommandLedgerTransition::NotUpdated(record))
                if record.state() == CommandLedgerState::Queued => {},
            Ok(CommandLedgerTransition::NotUpdated(record))
                if record.state() == CommandLedgerState::Dispatching =>
            {
                // A prior SQLite attempt may have committed even though its
                // response was lost. Invoking the adapter now could apply the
                // command twice, so close the ambiguous state without replay.
                persist_terminal_outcome(
                    ledger,
                    command_id,
                    CommandLedgerState::Dispatching,
                    CommandLedgerState::PossiblyApplied,
                    Some(
                        "dispatch admission became ambiguous before adapter invocation; command was not replayed",
                    ),
                )
                .await;
                return false;
            },
            Ok(CommandLedgerTransition::NotUpdated(record)) => {
                warn!(
                    channel_id = ctx.channel_id,
                    command_id = %format_args!("{:032x}", command_id.get()),
                    state = ?record.state(),
                    "durable command is no longer queued; suppressing device dispatch"
                );
                return false;
            },
            Ok(CommandLedgerTransition::Missing) => {
                error!(
                    channel_id = ctx.channel_id,
                    command_id = %format_args!("{:032x}", command_id.get()),
                    attempt = attempt + 1,
                    "durable command identity is missing before device dispatch"
                );
            },
            Err(error) => {
                error!(
                    %error,
                    channel_id = ctx.channel_id,
                    command_id = %format_args!("{:032x}", command_id.get()),
                    attempt = attempt + 1,
                    "durable command could not enter Dispatching"
                );
            },
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt + 1))).await;
        }
    }

    // The adapter has not been called. If the transition is still definitely
    // Queued, reject it durably. If SQLite shows Dispatching, a commit may have
    // crossed an uncertain response boundary, so conservatively record an
    // ambiguous outcome and never replay it.
    match ledger.query(command_id).await {
        Ok(Some(record)) if record.state() == CommandLedgerState::Queued => {
            persist_terminal_outcome(
                ledger,
                command_id,
                CommandLedgerState::Queued,
                CommandLedgerState::Failed,
                Some("durable dispatch admission failed before adapter invocation"),
            )
            .await;
        },
        Ok(Some(record)) if record.state() == CommandLedgerState::Dispatching => {
            persist_terminal_outcome(
                ledger,
                command_id,
                CommandLedgerState::Dispatching,
                CommandLedgerState::PossiblyApplied,
                Some("dispatch admission commit outcome was ambiguous; command was not replayed"),
            )
            .await;
        },
        Ok(Some(record)) if record.state().is_terminal() => {},
        Ok(Some(_)) | Ok(None) | Err(_) => ledger.record_outcome_persistence_failure(),
    }
    false
}

async fn persist_pre_dispatch_outcome(
    ctx: &ChannelPollContext,
    command_id: Option<aether_domain::CommandId>,
    outcome: &CommandLifecycleState,
) {
    let (Some(ledger), Some(command_id)) = (&ctx.commands.ledger, command_id) else {
        return;
    };
    let next = if matches!(outcome, CommandLifecycleState::Expired) {
        CommandLedgerState::Expired
    } else {
        CommandLedgerState::Failed
    };
    let diagnostic = match outcome {
        CommandLifecycleState::Expired => "command expired before device adapter invocation",
        CommandLifecycleState::DeviceFailed { diagnostic } => diagnostic.as_str(),
        CommandLifecycleState::Dropped(CommandDropReason::ServiceStopping) => {
            "channel task stopped before queued command reached device dispatch"
        },
        _ => "command rejected before device adapter invocation",
    };
    persist_terminal_outcome(
        ledger,
        command_id,
        CommandLedgerState::Queued,
        next,
        Some(diagnostic),
    )
    .await;
}

async fn persist_post_dispatch_outcome(
    ctx: &ChannelPollContext,
    command_id: Option<aether_domain::CommandId>,
    outcome: &CommandLifecycleState,
) {
    let (Some(ledger), Some(command_id)) = (&ctx.commands.ledger, command_id) else {
        return;
    };
    // Any adapter call may have reached a physical device. Only an explicit
    // full success is certain; errors, zero writes, partial writes, and task
    // cancellation remain PossiblyApplied and are never automatically replayed.
    let next = if matches!(outcome, CommandLifecycleState::DeviceSucceeded { .. }) {
        CommandLedgerState::Succeeded
    } else {
        CommandLedgerState::PossiblyApplied
    };
    let diagnostic = match outcome {
        CommandLifecycleState::DeviceSucceeded { .. } => None,
        CommandLifecycleState::DeviceFailed { diagnostic } => Some(diagnostic.as_str()),
        _ => Some("device outcome became ambiguous after dispatch began"),
    };
    persist_terminal_outcome(
        ledger,
        command_id,
        CommandLedgerState::Dispatching,
        next,
        diagnostic,
    )
    .await;
}

async fn persist_terminal_outcome(
    ledger: &Arc<CommandLedger>,
    command_id: aether_domain::CommandId,
    expected: CommandLedgerState,
    next: CommandLedgerState,
    diagnostic: Option<&str>,
) {
    const ATTEMPTS: u32 = 3;
    for attempt in 0..ATTEMPTS {
        match ledger
            .transition_with_diagnostic(command_id, expected, next, diagnostic)
            .await
        {
            Ok(CommandLedgerTransition::Updated(_)) => return,
            Ok(CommandLedgerTransition::NotUpdated(record)) if record.state().is_terminal() => {
                return;
            },
            Ok(CommandLedgerTransition::NotUpdated(record)) => {
                warn!(
                    command_id = %format_args!("{:032x}", command_id.get()),
                    state = ?record.state(),
                    attempt = attempt + 1,
                    "terminal command outcome has not reached a persistable state"
                );
            },
            Ok(CommandLedgerTransition::Missing) => {
                error!(
                    command_id = %format_args!("{:032x}", command_id.get()),
                    attempt = attempt + 1,
                    "terminal command outcome identity is missing"
                );
            },
            Err(error) => {
                error!(
                    %error,
                    command_id = %format_args!("{:032x}", command_id.get()),
                    attempt = attempt + 1,
                    "failed to persist terminal command outcome"
                );
            },
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt + 1))).await;
        }
    }
    ledger.record_outcome_persistence_failure();
}

/// Handle a periodic poll tick — reconnection logic + data polling.
async fn handle_poll_tick(
    ctx: &ChannelPollContext,
    protocol: &mut Box<dyn ChannelRuntime>,
    protocol_rx: &mut tokio::sync::mpsc::Receiver<ProtocolCommand>,
    reconnect_helper: &mut ReconnectHelper,
    failed_log_tick_counter: &mut u32,
    prev_online: &mut Option<bool>,
    prev_error_count: &mut u64,
    consecutive_zero_data: &mut u32,
) -> TickAction {
    let event_driven = protocol.is_event_driven();

    // Step 1: Check connection state before polling
    let conn_state = protocol.connection_state();

    if !conn_state.is_connected() {
        return handle_disconnected(
            ctx,
            protocol,
            protocol_rx,
            reconnect_helper,
            failed_log_tick_counter,
            prev_online,
        )
        .await;
    }

    // Step 2: Connected - only reset counter if it was non-zero
    if reconnect_helper.connection_state() != ReconnectState::Connected {
        reconnect_helper.mark_connected();
        *failed_log_tick_counter = 0;
        // Sync reconnect stats
        ctx.shared.reconnect_failed.store(false, Ordering::Relaxed);
    }

    // Step 3: Poll data using ChannelRuntime interface
    let result: PollResult = protocol.poll_once().await;

    // Log partial failures from poll result (only when failures exist)
    let failure_count = result.failures.len();
    if failure_count > 0 {
        let sample_errors: Vec<_> = result
            .failures
            .iter()
            .take(3)
            .map(|f| format!("pt{}:{}", f.point_id, f.error))
            .collect();
        warn!(
            "Ch{} partial read: {} failed, samples: [{}]",
            ctx.channel_id,
            failure_count,
            sample_errors.join(", ")
        );
    }

    let count = result.data.len();
    if count > 0 {
        *consecutive_zero_data = 0;
        if let Err(e) = write_batch_and_mark_fresh(
            ctx.store.as_ref(),
            ctx.channel_id,
            &result.data,
            &ctx.shared,
        ) {
            error!("Ch{} failed to write to SHM: {}", ctx.channel_id, e);
        } else {
            // Mark "data is flowing" only after the authoritative commit.
            // A decoded batch that SHM rejects must not keep a zombie channel
            // looking fresh.
            tracing::trace!("Ch{} poll ok: {} pts", ctx.channel_id, count);
        }
    }

    // Commit successful samples first, then degrade exact failed source slots.
    if failure_count > 0 {
        match ctx
            .store
            .mark_point_failures_bad(ctx.channel_id, &result.failures)
        {
            Ok(marked) => tracing::debug!(
                "Ch{} marked {}/{} failed points Bad in SHM",
                ctx.channel_id,
                marked,
                failure_count
            ),
            Err(error) => error!(
                "Ch{} failed to degrade partial-read quality in SHM: {}",
                ctx.channel_id, error
            ),
        }
    }

    if count == 0 && !event_driven && ctx.zero_data_threshold > 0 {
        *consecutive_zero_data += 1;
        if *consecutive_zero_data >= ctx.zero_data_threshold {
            warn!(
                "Ch{} no data for {} consecutive cycles, triggering disconnect",
                ctx.channel_id, consecutive_zero_data
            );
            let _ = stop_events_and_disconnect(protocol.as_mut()).await;
            *consecutive_zero_data = 0;
            update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);
            return TickAction::Proceed;
        }
    }

    // Check diagnostics for accumulated errors and update cache
    if let Ok(diag) = protocol.diagnostics().await {
        if diag.error_count > *prev_error_count {
            let new_errors = diag.error_count - *prev_error_count;
            warn!(
                "Ch{} accumulated errors: {} new errors, last error: {:?}",
                ctx.channel_id, new_errors, diag.last_error
            );
            *prev_error_count = diag.error_count;
        }
        ctx.shared.cached_diagnostics.store(Some(Arc::new(diag)));
    }

    // Update cached connection state after each poll cycle
    update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
    check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);

    TickAction::Proceed
}

/// Handle disconnected state — reconnection logic with backoff.
async fn handle_disconnected(
    ctx: &ChannelPollContext,
    protocol: &mut Box<dyn ChannelRuntime>,
    protocol_rx: &mut tokio::sync::mpsc::Receiver<ProtocolCommand>,
    reconnect_helper: &mut ReconnectHelper,
    failed_log_tick_counter: &mut u32,
    prev_online: &mut Option<bool>,
) -> TickAction {
    // Sync reconnect stats to shared atomics on every disconnected tick
    ctx.shared
        .reconnect_total_attempts
        .store(reconnect_helper.stats().total_attempts, Ordering::Relaxed);

    match reconnect_helper.connection_state() {
        ReconnectState::Failed => {
            ctx.shared.reconnect_failed.store(true, Ordering::Relaxed);

            // Check auto-recovery before giving up
            if reconnect_helper.check_auto_recovery() {
                info!(
                    "Ch{} auto-recovery triggered, returning to Disconnected state",
                    ctx.channel_id
                );
                ctx.shared.reconnect_failed.store(false, Ordering::Relaxed);
                *failed_log_tick_counter = 0;
                update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
                check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);
                return TickAction::Continue;
            }

            // Max retry attempts reached, log periodically (every 60 ticks)
            *failed_log_tick_counter += 1;
            if failed_log_tick_counter.is_multiple_of(60) {
                if let Some(remaining) = reconnect_helper.recovery_cooldown_remaining() {
                    warn!(
                        "Ch{} reconnection failed (max attempts reached), \
                         auto-recovery in {:?} (round {}/{})",
                        ctx.channel_id,
                        remaining,
                        reconnect_helper.recovery_rounds() + 1,
                        3 // max_recovery_rounds default
                    );
                } else {
                    warn!(
                        "Ch{} reconnection permanently failed, \
                         manual intervention required (disable/enable)",
                        ctx.channel_id
                    );
                }
            }
            update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);
            TickAction::Continue
        },
        ReconnectState::Reconnecting => TickAction::Continue,
        ReconnectState::Connected | ReconnectState::Disconnected => {
            if reconnect_helper.connection_state() == ReconnectState::Connected {
                warn!("Ch{} connection lost unexpectedly", ctx.channel_id);
                reconnect_helper.mark_disconnected();
            }
            if !reconnect_helper.record_attempt() {
                update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
                check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);
                return TickAction::Continue;
            }

            // Apply backoff delay for retry attempts after the first
            let current_attempt = reconnect_helper.stats().total_attempts;
            if current_attempt > 1 {
                let delay = reconnect_helper.calculate_next_delay();
                info!(
                    "Ch{} waiting {:?} before reconnect attempt",
                    ctx.channel_id, delay
                );
                // Remain responsive to lifecycle commands and expose bounded
                // task progress even when the configured backoff exceeds the
                // watchdog threshold. This never updates data freshness.
                if let BackoffWait::Protocol(cmd) =
                    wait_reconnect_backoff(delay, protocol_rx, &ctx.shared).await
                {
                    let action = handle_backoff_command(
                        cmd,
                        protocol,
                        reconnect_helper,
                        failed_log_tick_counter,
                        &ctx.log_handler,
                        ctx.channel_id,
                    )
                    .await;
                    if let Some(a) = action {
                        update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
                        check_online_change(
                            protocol.as_ref(),
                            prev_online,
                            &ctx.store,
                            ctx.channel_id,
                        );
                        return a;
                    }
                    update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
                    check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);
                    return TickAction::Continue;
                }
            }

            // Attempt reconnection with timeout to prevent hanging
            info!("Ch{} attempting reconnect", ctx.channel_id);
            match tokio::time::timeout(
                Duration::from_secs(30),
                connect_and_start_events(protocol.as_mut()),
            )
            .await
            {
                Ok(Ok(())) => {
                    info!("Ch{} reconnected successfully", ctx.channel_id);
                    reconnect_helper.mark_connected();
                    ctx.shared.reconnect_failed.store(false, Ordering::Relaxed);
                    *failed_log_tick_counter = 0;
                },
                Ok(Err(e)) => {
                    warn!("Ch{} reconnect failed: {}", ctx.channel_id, e);
                    reconnect_helper.record_failure();
                },
                Err(_) => {
                    warn!("Ch{} reconnect timed out (30s)", ctx.channel_id);
                    reconnect_helper.record_failure();
                },
            }
            update_cached_state(protocol.as_ref(), &ctx.shared.cached_connection_state);
            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id);
            TickAction::Continue
        },
    }
}

/// Handle a protocol command received during reconnect backoff.
/// Returns Some(TickAction) if the caller should return immediately, None to continue.
async fn handle_backoff_command(
    cmd: ProtocolCommand,
    protocol: &mut Box<dyn ChannelRuntime>,
    reconnect_helper: &mut ReconnectHelper,
    failed_log_tick_counter: &mut u32,
    log_handler: &Arc<dyn ChannelLogHandler>,
    channel_id: u32,
) -> Option<TickAction> {
    match cmd {
        ProtocolCommand::Shutdown => {
            info!("Ch{} shutdown during reconnect backoff", channel_id);
            return Some(TickAction::Break);
        },
        ProtocolCommand::Connect { response_tx } => {
            let result = connect_and_start_events(protocol.as_mut()).await;
            if result.is_ok() {
                reconnect_helper.mark_connected();
                *failed_log_tick_counter = 0;
            }
            let _ = response_tx.send(result);
        },
        ProtocolCommand::Disconnect { response_tx } => {
            let _ = stop_events_and_disconnect(protocol.as_mut()).await;
            let _ = response_tx.send(());
        },
        ProtocolCommand::SetLogLevel { level, response_tx } => {
            let result = apply_log_level(protocol.as_mut(), log_handler.as_ref(), &level);
            let _ = response_tx.send(result);
        },
    }
    None
}

/// Repository-owned benchmark seam for exercising the production command
/// consumer without constructing a physical protocol adapter.
///
/// The module is absent from default builds. It deliberately reuses
/// [`handle_business_command`], [`CommandGuard`], [`ChannelPollContext`], and
/// the real durable ledger transitions; only the final `ChannelRuntime`
/// implementation is a counting adapter.
#[cfg(feature = "bench-support")]
#[doc(hidden)]
pub mod command_bench_support {
    use std::path::Path;

    use aether_routing::RoutingCache;
    use aether_shm_bridge::{ChannelPointManifest, ShmRuntimeConfig, ShmWriterHandle};
    use async_trait::async_trait;

    use super::*;
    use crate::core::channels::RuntimeChannelConfig;
    use crate::core::channels::command_ledger::CommandLedger;
    use crate::core::config::{
        ChannelConfig, ChannelCore, ChannelLoggingConfig, ControlPoint, Point,
    };
    use crate::protocols::core::data::DataBatch;
    use crate::protocols::core::error::Result as ProtocolResult;
    use crate::protocols::core::log_handlers::TracingLogHandler;
    use crate::protocols::core::traits::{ConnectionState as ProtocolConnectionState, Diagnostics};

    /// Typed composition failure for the feature-gated production consumer.
    #[derive(Debug, thiserror::Error)]
    #[error("command benchmark consumer composition failed: {message}")]
    pub struct ProductionCommandBenchError {
        message: String,
    }

    impl ProductionCommandBenchError {
        fn new(message: impl Into<String>) -> Self {
            Self {
                message: message.into(),
            }
        }
    }

    /// Invocation counters observed around one real production dispatch.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProductionCommandDispatchObservation {
        pub adapter_invocations_before: u64,
        pub adapter_invocations_after: u64,
        pub adapter_invocation_delta: u64,
    }

    /// Minimal owner for the real command consumer and a counting protocol.
    pub struct ProductionCommandBenchDriver {
        context: ChannelPollContext,
        protocol: Box<dyn ChannelRuntime>,
        adapter_invocations: Arc<AtomicU64>,
    }

    impl ProductionCommandBenchDriver {
        /// Composes the same guard, ledger context, and SHM dependency shape as
        /// one production channel task. The SHM path is benchmark-owned.
        pub fn new(
            shm_path: &Path,
            ledger: Arc<CommandLedger>,
            channel_id: u32,
            control_point_id: u32,
        ) -> std::result::Result<Self, ProductionCommandBenchError> {
            let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
                channel_id,
                [1, 0, 0, 0],
            )]));
            let shm_handle = ShmWriterHandle::create(
                ShmRuntimeConfig::new(shm_path, 8),
                manifest,
                None,
                None,
                1,
            )
            .map(Arc::new)
            .map_err(|error| ProductionCommandBenchError::new(error.to_string()))?;
            let store = ShmDataStore::new(shm_handle, Arc::new(RoutingCache::new()))
                .map(Arc::new)
                .map_err(|error| ProductionCommandBenchError::new(error.to_string()))?;

            let mut runtime = RuntimeChannelConfig::from_base(ChannelConfig {
                core: ChannelCore {
                    id: channel_id,
                    name: "command-benchmark".to_string(),
                    description: None,
                    protocol: "counting-benchmark".to_string(),
                    enabled: true,
                },
                parameters: std::collections::HashMap::new(),
                logging: ChannelLoggingConfig::default(),
            });
            runtime.control_points.push(ControlPoint {
                base: Point {
                    point_id: control_point_id,
                    signal_name: "benchmark-control".to_string(),
                    description: None,
                    unit: None,
                    protocol_mappings: None,
                },
                reverse: false,
                control_type: "latching".to_string(),
                on_value: 1,
                off_value: 0,
                pulse_duration_ms: None,
            });
            let guard = CommandGuard::from_runtime(&runtime)
                .map_err(|error| ProductionCommandBenchError::new(error.to_string()))?;
            let adapter_invocations = Arc::new(AtomicU64::new(0));
            let protocol: Box<dyn ChannelRuntime> = Box::new(CountingCommandRuntime {
                adapter_invocations: Arc::clone(&adapter_invocations),
            });
            Ok(Self {
                context: ChannelPollContext {
                    store,
                    channel_id,
                    poll_interval_ms: NonZeroU64::new(1)
                        .ok_or_else(|| ProductionCommandBenchError::new("zero poll interval"))?,
                    shared: Arc::new(ChannelSharedState::new(0)),
                    log_handler: Arc::new(TracingLogHandler),
                    zero_data_threshold: 0,
                    commands: CommandDispatchContext {
                        guard,
                        outcomes: Arc::new(CommandOutcomeTracker::default()),
                        ledger: Some(ledger),
                    },
                },
                protocol,
                adapter_invocations,
            })
        }

        /// Sends one already-admitted queue item through the production
        /// consumer and reports the actual `ChannelRuntime` call delta.
        pub async fn consume(
            &mut self,
            command: ChannelCommand,
        ) -> ProductionCommandDispatchObservation {
            let before = self.adapter_invocations();
            handle_business_command(command, &mut self.protocol, &self.context).await;
            let after = self.adapter_invocations();
            ProductionCommandDispatchObservation {
                adapter_invocations_before: before,
                adapter_invocations_after: after,
                adapter_invocation_delta: after.saturating_sub(before),
            }
        }

        /// Returns the total number of real adapter method invocations.
        #[must_use]
        pub fn adapter_invocations(&self) -> u64 {
            self.adapter_invocations.load(Ordering::Relaxed)
        }
    }

    struct CountingCommandRuntime {
        adapter_invocations: Arc<AtomicU64>,
    }

    #[async_trait]
    impl ChannelRuntime for CountingCommandRuntime {
        async fn connect(&mut self) -> ProtocolResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> ProtocolResult<()> {
            Ok(())
        }

        async fn poll_once(&mut self) -> PollResult {
            PollResult::success(DataBatch::new())
        }

        async fn write_control(&mut self, commands: &[(u32, f64)]) -> ProtocolResult<usize> {
            self.adapter_invocations.fetch_add(1, Ordering::Relaxed);
            Ok(commands.len())
        }

        async fn diagnostics(&self) -> ProtocolResult<Diagnostics> {
            Ok(Diagnostics::new("counting-command-benchmark"))
        }

        fn connection_state(&self) -> ProtocolConnectionState {
            ProtocolConnectionState::Connected
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use aether_dataplane::SlotWriter;
    use aether_routing::RoutingCache;
    use aether_shm_bridge::{ChannelPointManifest, ShmAcquisitionStateWriter};
    use async_trait::async_trait;

    use super::*;
    use crate::protocols::core::data::{DataBatch, DataPoint};
    use crate::protocols::core::error::{GatewayError, Result};
    use crate::protocols::core::log_handlers::TracingLogHandler;
    use crate::protocols::core::traits::{
        ConnectionState, Diagnostics, PollResult, data_event_channel_with_capacity,
    };

    #[test]
    fn channel_log_level_rejects_retired_spellings() {
        let (mut protocol, _) = EventLifecycleProbe::new(false);
        let log_handler = TracingLogHandler;
        for canonical in ["debug", "info", "error"] {
            assert!(apply_log_level(&mut protocol, &log_handler, canonical).is_ok());
        }
        for retired in ["DEBUG", "verbose", "standard", "minimal"] {
            assert!(
                apply_log_level(&mut protocol, &log_handler, retired).is_err(),
                "accepted {retired}"
            );
        }
    }

    struct EventLifecycleProbe {
        calls: Arc<Mutex<Vec<&'static str>>>,
        fail_start: bool,
    }

    impl EventLifecycleProbe {
        fn new(fail_start: bool) -> (Self, Arc<Mutex<Vec<&'static str>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                    fail_start,
                },
                calls,
            )
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().expect("lifecycle probe lock").push(call);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn long_reconnect_backoff_reports_task_progress_without_claiming_fresh_data() {
        let shared = Arc::new(ChannelSharedState::new(
            ConnectionState::Disconnected.into(),
        ));
        mark_loop_progress(&shared);
        let initial_progress = shared.watchdog_progress_tick_ms.load(Ordering::Relaxed);
        // The liveness stamp deliberately uses the process monotonic clock, not
        // Tokio's test clock. Give it a distinct millisecond before advancing
        // the paused reconnect timer.
        std::thread::sleep(Duration::from_millis(2));

        let (protocol_tx, mut protocol_rx) = tokio::sync::mpsc::channel(1);
        let wait_shared = Arc::clone(&shared);
        let waiter = tokio::spawn(async move {
            wait_reconnect_backoff(Duration::from_secs(300), &mut protocol_rx, &wait_shared).await
        });
        tokio::task::yield_now().await;

        tokio::time::advance(WATCHDOG_HEARTBEAT_INTERVAL).await;
        tokio::task::yield_now().await;

        assert!(
            shared.watchdog_progress_tick_ms.load(Ordering::Relaxed) > initial_progress,
            "a completed backoff slice is legitimate task progress"
        );
        assert_eq!(
            shared.last_successful_read_tick_ms.load(Ordering::Relaxed),
            0,
            "waiting for an offline device must not claim acquisition freshness"
        );
        assert_eq!(
            shared.last_successful_read_ms.load(Ordering::Relaxed),
            0,
            "waiting for an offline device must not publish a sample timestamp"
        );

        protocol_tx
            .send(ProtocolCommand::Shutdown)
            .await
            .expect("send shutdown during backoff");
        assert!(matches!(
            waiter.await.expect("backoff waiter"),
            BackoffWait::Protocol(ProtocolCommand::Shutdown)
        ));
    }

    #[async_trait]
    impl ChannelRuntime for EventLifecycleProbe {
        fn is_event_driven(&self) -> bool {
            true
        }

        async fn connect(&mut self) -> Result<()> {
            self.record("connect");
            Ok(())
        }

        async fn disconnect(&mut self) -> Result<()> {
            self.record("disconnect");
            Ok(())
        }

        async fn poll_once(&mut self) -> PollResult {
            PollResult::success(DataBatch::new())
        }

        async fn start_events(&mut self) -> Result<()> {
            self.record("start_events");
            if self.fail_start {
                Err(GatewayError::Protocol("event startup failed".to_string()))
            } else {
                Ok(())
            }
        }

        async fn stop_events(&mut self) -> Result<()> {
            self.record("stop_events");
            Ok(())
        }

        async fn diagnostics(&self) -> Result<Diagnostics> {
            Ok(Diagnostics::new("event-probe"))
        }

        fn connection_state(&self) -> ConnectionState {
            ConnectionState::Disconnected
        }
    }

    #[tokio::test]
    async fn event_protocol_activation_starts_stream_after_connecting() {
        let (mut protocol, calls) = EventLifecycleProbe::new(false);

        connect_and_start_events(&mut protocol).await.unwrap();

        assert_eq!(
            calls.lock().expect("lifecycle probe lock").as_slice(),
            ["connect", "start_events"]
        );
    }

    #[tokio::test]
    async fn event_start_failure_disconnects_the_transport() {
        let (mut protocol, calls) = EventLifecycleProbe::new(true);

        assert!(connect_and_start_events(&mut protocol).await.is_err());

        assert_eq!(
            calls.lock().expect("lifecycle probe lock").as_slice(),
            ["connect", "start_events", "disconnect"]
        );
    }

    #[tokio::test]
    async fn unsupported_writes_fail_closed_by_default() {
        let (mut protocol, _) = EventLifecycleProbe::new(false);

        assert!(matches!(
            protocol.write_control(&[(1, 1.0)]).await,
            Err(GatewayError::Unsupported(_))
        ));
        assert!(matches!(
            protocol.write_adjustment(&[(1, 1.0)]).await,
            Err(GatewayError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn expiry_at_the_adapter_boundary_closes_dispatch_without_adapter_invocation() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory command ledger");
        let ledger = Arc::new(
            CommandLedger::initialize(pool)
                .await
                .expect("command ledger"),
        );
        let command_id = aether_domain::CommandId::new(0xe11);
        ledger
            .admit_for_channel(
                command_id,
                7,
                [0xe1; 32],
                aether_domain::TimestampMs::new(u64::MAX >> 1),
            )
            .await
            .expect("admit durable command");
        for (from, to) in [
            (CommandLedgerState::Received, CommandLedgerState::Queued),
            (CommandLedgerState::Queued, CommandLedgerState::Dispatching),
        ] {
            assert!(matches!(
                ledger.transition(command_id, from, to).await,
                Ok(CommandLedgerTransition::Updated(_))
            ));
        }

        let directory = tempfile::tempdir().expect("create test SHM directory");
        let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
            7,
            [1, 0, 0, 0],
        )]));
        let writer = Arc::new(
            SlotWriter::create(
                directory.path().join("command-expiry.shm"),
                manifest.slot_count(),
                manifest.layout_hash(),
                1,
            )
            .expect("create acquisition writer"),
        );
        let store = Arc::new(ShmDataStore::from_acquisition_writer(
            Arc::new(ShmAcquisitionStateWriter::new(writer, manifest)),
            Arc::new(RoutingCache::default()),
        ));
        let runtime_config = crate::core::channels::RuntimeChannelConfig::from_base(
            crate::core::config::ChannelConfig {
                core: crate::core::config::ChannelCore {
                    id: 7,
                    name: "command-expiry".to_string(),
                    description: None,
                    protocol: "test".to_string(),
                    enabled: true,
                },
                parameters: HashMap::new(),
                logging: crate::core::config::ChannelLoggingConfig::default(),
            },
        );
        let ctx = ChannelPollContext {
            store,
            channel_id: 7,
            poll_interval_ms: NonZeroU64::new(1).expect("non-zero poll interval"),
            shared: Arc::new(ChannelSharedState::new(0)),
            log_handler: Arc::new(TracingLogHandler),
            zero_data_threshold: 0,
            commands: CommandDispatchContext {
                guard: CommandGuard::from_runtime(&runtime_config).expect("command guard"),
                outcomes: Arc::new(CommandOutcomeTracker::default()),
                ledger: Some(Arc::clone(&ledger)),
            },
        };

        assert!(expire_at_adapter_boundary(&ctx, Some(command_id), 0).await);
        let record = ledger
            .query(command_id)
            .await
            .expect("query command outcome")
            .expect("retained command");
        assert_eq!(record.state(), CommandLedgerState::PossiblyApplied);
        assert!(
            record
                .diagnostic()
                .is_some_and(|value| value.contains("adapter was not invoked"))
        );
    }

    #[tokio::test]
    async fn failed_acquisition_commit_does_not_advance_freshness() {
        let directory = tempfile::tempdir().expect("create test SHM directory");
        let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
            7,
            [1, 0, 0, 0],
        )]));
        let writer = Arc::new(
            SlotWriter::create(
                directory.path().join("freshness.shm"),
                manifest.slot_count(),
                manifest.layout_hash(),
                1,
            )
            .expect("create acquisition writer"),
        );
        let store = ShmDataStore::from_acquisition_writer(
            Arc::new(ShmAcquisitionStateWriter::new(writer, manifest)),
            Arc::new(RoutingCache::default()),
        );
        let shared = ChannelSharedState::new(0);
        shared.last_successful_read_ms.store(123, Ordering::Relaxed);
        shared
            .last_successful_read_tick_ms
            .store(456, Ordering::Relaxed);
        let batch = DataBatch::from_points(vec![DataPoint::telemetry(99, 42.0)]);

        assert!(write_batch_and_mark_fresh(&store, 7, &batch, &shared).is_err());
        assert_eq!(shared.last_successful_read_ms.load(Ordering::Relaxed), 123);
        assert_eq!(
            shared.last_successful_read_tick_ms.load(Ordering::Relaxed),
            456
        );
    }

    #[tokio::test(start_paused = true)]
    async fn overdue_poll_deadline_wins_when_all_inputs_are_ready() {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        interval.tick().await;
        let mut heartbeat_interval = tokio::time::interval(WATCHDOG_HEARTBEAT_INTERVAL);
        heartbeat_interval.tick().await;
        tokio::time::advance(Duration::from_millis(10)).await;

        let (protocol_tx, mut protocol_rx) = tokio::sync::mpsc::channel(1);
        protocol_tx.send(ProtocolCommand::Shutdown).await.unwrap();

        let (business_tx, mut business_rx) = tokio::sync::mpsc::channel(1);
        business_tx
            .send(ChannelCommand::Control {
                command_id: "ready-business-command".to_string(),
                point_id: 1,
                value: 1.0,
                timestamp: 0,
                expires_at_ms: i64::MAX,
            })
            .await
            .unwrap();

        let (event_tx, event_rx) = data_event_channel_with_capacity(1);
        event_tx.publish(DataEvent::Heartbeat);
        let mut event_rx = Some(event_rx);

        let work = next_channel_work(
            &mut interval,
            &mut heartbeat_interval,
            &mut protocol_rx,
            &mut business_rx,
            &mut event_rx,
            true,
            NonPollSource::Protocol,
        )
        .await;

        assert!(matches!(work, ChannelWork::Poll));
    }

    #[tokio::test(start_paused = true)]
    async fn continuous_ready_sources_all_advance_between_successive_poll_deadlines() {
        const POLL_INTERVAL: Duration = Duration::from_millis(10);
        const DEADLINES: usize = 6;

        fn protocol_command() -> ProtocolCommand {
            let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
            ProtocolCommand::SetLogLevel {
                level: "info".to_string(),
                response_tx,
            }
        }

        fn business_command() -> ChannelCommand {
            ChannelCommand::Control {
                command_id: "continuous-business-command".to_string(),
                point_id: 1,
                value: 1.0,
                timestamp: 0,
                expires_at_ms: i64::MAX,
            }
        }

        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.tick().await;
        let mut heartbeat_interval = tokio::time::interval(WATCHDOG_HEARTBEAT_INTERVAL);
        heartbeat_interval.tick().await;

        let (protocol_tx, mut protocol_rx) = tokio::sync::mpsc::channel(1);
        protocol_tx.send(protocol_command()).await.unwrap();
        let (business_tx, mut business_rx) = tokio::sync::mpsc::channel(1);
        business_tx.send(business_command()).await.unwrap();
        let (event_tx, event_rx) = data_event_channel_with_capacity(1);
        event_tx.publish(DataEvent::Heartbeat);
        let mut event_rx = Some(event_rx);

        let mut polls = 0;
        let mut protocol_work = 0;
        let mut business_work = 0;
        let mut event_work = 0;
        let mut next_source = NonPollSource::Protocol;
        for _ in 0..DEADLINES {
            tokio::time::advance(POLL_INTERVAL).await;
            let work = next_channel_work(
                &mut interval,
                &mut heartbeat_interval,
                &mut protocol_rx,
                &mut business_rx,
                &mut event_rx,
                true,
                next_source,
            )
            .await;
            assert!(matches!(work, ChannelWork::Poll));
            polls += 1;

            // All three external sources stay ready. After the hard-deadline
            // poll, one rotating non-poll source must make progress.
            let work = next_channel_work(
                &mut interval,
                &mut heartbeat_interval,
                &mut protocol_rx,
                &mut business_rx,
                &mut event_rx,
                false,
                next_source,
            )
            .await;
            next_source = NonPollSource::after(&work, next_source);
            match work {
                ChannelWork::Protocol(_) => {
                    protocol_work += 1;
                    protocol_tx.send(protocol_command()).await.unwrap();
                },
                ChannelWork::Business(_) => {
                    business_work += 1;
                    business_tx.send(business_command()).await.unwrap();
                },
                ChannelWork::Event(Some(_)) => {
                    event_work += 1;
                    event_tx.publish(DataEvent::Heartbeat);
                },
                _ => panic!("an always-ready external source must be selected"),
            }
        }

        assert_eq!(polls, DEADLINES);
        assert!(protocol_work >= 2);
        assert!(business_work >= 2);
        assert!(event_work >= 2);
    }

    #[tokio::test(start_paused = true)]
    async fn protocol_head_shutdown_has_a_four_poll_bound_in_the_worst_rotation() {
        let mut poll_interval = tokio::time::interval(Duration::from_millis(10));
        poll_interval.tick().await;
        let mut heartbeat_interval = tokio::time::interval(WATCHDOG_HEARTBEAT_INTERVAL);
        heartbeat_interval.tick().await;
        tokio::time::advance(WATCHDOG_HEARTBEAT_INTERVAL).await;

        let (protocol_tx, mut protocol_rx) = tokio::sync::mpsc::channel(1);
        protocol_tx.send(ProtocolCommand::Shutdown).await.unwrap();
        let (business_tx, mut business_rx) = tokio::sync::mpsc::channel(1);
        business_tx
            .send(ChannelCommand::Control {
                command_id: "shutdown-bound-business".to_string(),
                point_id: 1,
                value: 1.0,
                timestamp: 0,
                expires_at_ms: i64::MAX,
            })
            .await
            .unwrap();
        let (event_tx, event_rx) = data_event_channel_with_capacity(1);
        event_tx.publish(DataEvent::Heartbeat);
        let mut event_rx = Some(event_rx);
        let mut next_source = NonPollSource::Business;
        let mut shutdown_after_polls = None;

        for poll_count in 1..=4 {
            if poll_count > 1 {
                // Model a poll that occupied the complete configured period,
                // keeping the next acquisition deadline overdue.
                tokio::time::advance(Duration::from_millis(10)).await;
            }
            let poll = next_channel_work(
                &mut poll_interval,
                &mut heartbeat_interval,
                &mut protocol_rx,
                &mut business_rx,
                &mut event_rx,
                true,
                next_source,
            )
            .await;
            assert!(matches!(poll, ChannelWork::Poll));

            let work = next_channel_work(
                &mut poll_interval,
                &mut heartbeat_interval,
                &mut protocol_rx,
                &mut business_rx,
                &mut event_rx,
                false,
                next_source,
            )
            .await;
            next_source = NonPollSource::after(&work, next_source);
            match work {
                ChannelWork::Protocol(ProtocolCommand::Shutdown) => {
                    shutdown_after_polls = Some(poll_count);
                    break;
                },
                ChannelWork::Business(command) => business_tx.send(command).await.unwrap(),
                ChannelWork::Event(Some(event)) => {
                    event_tx.publish(event);
                },
                ChannelWork::Heartbeat => {},
                _ => panic!("unexpected work while proving shutdown bound"),
            }
        }

        assert_eq!(shutdown_after_polls, Some(4));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_long_poll_channel_keeps_loop_heartbeat_without_claiming_freshness() {
        const LONG_POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
        const HEARTBEATS: usize = 4;

        let mut poll_interval = tokio::time::interval(LONG_POLL_INTERVAL);
        poll_interval.tick().await;
        let mut heartbeat_interval = tokio::time::interval(WATCHDOG_HEARTBEAT_INTERVAL);
        heartbeat_interval.tick().await;

        let (_protocol_tx, mut protocol_rx) = tokio::sync::mpsc::channel(1);
        let (_business_tx, mut business_rx) = tokio::sync::mpsc::channel(1);
        let mut event_rx = None;
        let shared = ChannelSharedState::new(0);
        shared.last_successful_read_ms.store(123, Ordering::Relaxed);
        shared
            .last_successful_read_tick_ms
            .store(456, Ordering::Relaxed);

        for _ in 0..HEARTBEATS {
            tokio::time::advance(WATCHDOG_HEARTBEAT_INTERVAL).await;
            let work = next_channel_work(
                &mut poll_interval,
                &mut heartbeat_interval,
                &mut protocol_rx,
                &mut business_rx,
                &mut event_rx,
                true,
                NonPollSource::Protocol,
            )
            .await;
            assert!(matches!(work, ChannelWork::Heartbeat));
            mark_loop_progress(&shared);
        }

        assert!(shared.watchdog_progress_tick_ms.load(Ordering::Relaxed) > 0);
        assert_eq!(shared.last_successful_read_ms.load(Ordering::Relaxed), 123);
        assert_eq!(
            shared.last_successful_read_tick_ms.load(Ordering::Relaxed),
            456
        );
    }

    #[test]
    fn event_driven_data_commit_advances_freshness_without_owning_loop_heartbeat() {
        let directory = tempfile::tempdir().expect("create test SHM directory");
        let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
            7,
            [1, 0, 0, 0],
        )]));
        let writer = Arc::new(
            SlotWriter::create(
                directory.path().join("event-freshness.shm"),
                manifest.slot_count(),
                manifest.layout_hash(),
                1,
            )
            .expect("create acquisition writer"),
        );
        let store = Arc::new(ShmDataStore::from_acquisition_writer(
            Arc::new(ShmAcquisitionStateWriter::new(writer, manifest)),
            Arc::new(RoutingCache::default()),
        ));
        let shared = Arc::new(ChannelSharedState::new(0));
        shared
            .watchdog_progress_tick_ms
            .store(123, Ordering::Relaxed);
        let runtime_config = crate::core::channels::RuntimeChannelConfig::from_base(
            crate::core::config::ChannelConfig {
                core: crate::core::config::ChannelCore {
                    id: 7,
                    name: "event-freshness".to_string(),
                    description: None,
                    protocol: "test".to_string(),
                    enabled: true,
                },
                parameters: HashMap::new(),
                logging: crate::core::config::ChannelLoggingConfig::default(),
            },
        );
        let ctx = ChannelPollContext {
            store,
            channel_id: 7,
            poll_interval_ms: NonZeroU64::new(1).unwrap(),
            shared: Arc::clone(&shared),
            log_handler: Arc::new(TracingLogHandler),
            zero_data_threshold: 0,
            commands: CommandDispatchContext {
                guard: CommandGuard::from_runtime(&runtime_config).unwrap(),
                outcomes: Arc::new(CommandOutcomeTracker::default()),
                ledger: None,
            },
        };
        let mut prev_online = None;

        handle_protocol_event(
            DataEvent::DataUpdate(DataBatch::from_points(vec![DataPoint::telemetry(0, 42.0)])),
            &ctx,
            &mut prev_online,
        );

        assert!(shared.last_successful_read_ms.load(Ordering::Relaxed) > 0);
        assert!(shared.last_successful_read_tick_ms.load(Ordering::Relaxed) > 0);
        assert_eq!(
            shared.watchdog_progress_tick_ms.load(Ordering::Relaxed),
            123
        );
    }

    #[test]
    fn loop_progress_is_distinct_from_acquisition_freshness() {
        let shared = ChannelSharedState::new(0);
        shared.last_successful_read_ms.store(123, Ordering::Relaxed);
        shared
            .last_successful_read_tick_ms
            .store(456, Ordering::Relaxed);

        mark_loop_progress(&shared);

        assert!(shared.watchdog_progress_tick_ms.load(Ordering::Relaxed) > 0);
        assert_eq!(shared.last_successful_read_ms.load(Ordering::Relaxed), 123);
        assert_eq!(
            shared.last_successful_read_tick_ms.load(Ordering::Relaxed),
            456
        );
    }
}
