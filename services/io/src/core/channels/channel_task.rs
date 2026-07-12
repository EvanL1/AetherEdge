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

use crate::core::channels::traits::ChannelCommand;
use crate::core::channels::types::ProtocolCommand;
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler};
use crate::protocols::core::traits::{DataEvent, DataEventReceiver, PollResult};
use crate::protocols::gateway::ChannelRuntime;
use crate::runtime::reconnect::{
    AutoRecoveryPolicy, ReconnectHelper, ReconnectPolicy, ReconnectState,
};
use crate::store::ShmDataStore;

use super::command_guard::CommandGuard;

/// Shared immutable context for channel polling operations.
///
/// Groups the Arc/Atomic fields that are threaded unchanged through the poll loop,
/// reducing function signatures from 14+ params to ≤ 6.
pub(super) struct ChannelPollContext {
    pub store: Arc<ShmDataStore>,
    pub channel_id: u32,
    pub poll_interval_ms: NonZeroU64,
    pub cached_state: Arc<AtomicU8>,
    pub cached_diagnostics: Arc<ArcSwapOption<crate::protocols::core::traits::Diagnostics>>,
    pub log_handler: Arc<dyn ChannelLogHandler>,
    pub watchdog_heartbeat_ms: Arc<AtomicI64>,
    pub reconnect_total_attempts: Arc<AtomicU64>,
    pub reconnect_failed: Arc<AtomicBool>,
    /// Timestamp (millis since epoch) of the most recent poll cycle that
    /// returned at least one successful point. Drives `is_connected()` so the
    /// UI reflects data freshness, not just TCP state.
    pub last_successful_read_ms: Arc<AtomicI64>,
    /// Consecutive zero-data poll cycles before triggering disconnect (0 = disabled)
    pub zero_data_threshold: u32,
    /// Final fail-closed command policy for this channel.
    pub command_guard: CommandGuard,
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
async fn receive_protocol_event(
    receiver: &mut Option<DataEventReceiver>,
) -> std::result::Result<DataEvent, tokio::sync::broadcast::error::RecvError> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn handle_protocol_event(
    event: DataEvent,
    ctx: &ChannelPollContext,
    prev_online: &mut Option<bool>,
) {
    match event {
        DataEvent::DataUpdate(batch) => {
            if !batch.is_empty() {
                let now_ms = super::channel_entry::unix_timestamp_ms();
                ctx.last_successful_read_ms.store(now_ms, Ordering::Relaxed);
                ctx.watchdog_heartbeat_ms.store(now_ms, Ordering::Relaxed);
                ctx.store
                    .refresh_channel_health_heartbeat(now_ms.max(0) as u64);
                if let Err(error) = ctx
                    .store
                    .write_batch(ctx.channel_id, batch.as_ref().clone())
                    .await
                {
                    error!(
                        "Ch{} failed to write event data to SHM: {}",
                        ctx.channel_id, error
                    );
                }
            }
        },
        DataEvent::ConnectionChanged(state) => {
            let cached_state: crate::core::channels::types::ConnectionState = state.into();
            ctx.cached_state
                .store(cached_state.as_u8(), Ordering::Relaxed);
            let online = state.is_connected();
            if *prev_online != Some(online) {
                *prev_online = Some(online);
                ctx.store
                    .publish_channel_online(ctx.channel_id, online)
                    .await;
            }
        },
        DataEvent::Error(message) => {
            warn!("Ch{} event stream error: {}", ctx.channel_id, message);
        },
        DataEvent::Heartbeat => {
            let now_ms = super::channel_entry::unix_timestamp_ms();
            ctx.watchdog_heartbeat_ms.store(now_ms, Ordering::Relaxed);
            ctx.store
                .refresh_channel_health_heartbeat(now_ms.max(0) as u64);
        },
    }
}

/// Check if channel online state changed and publish it to the SHM health plane.
///
/// Avoids redundant SHM writes by tracking previous state.
async fn check_online_change(
    protocol: &dyn ChannelRuntime,
    prev_online: &mut Option<bool>,
    store: &ShmDataStore,
    channel_id: u32,
) {
    let heartbeat_ms = super::channel_entry::unix_timestamp_ms().max(0) as u64;
    store.refresh_channel_health_heartbeat(heartbeat_ms);
    let current_online = protocol.connection_state().is_connected();
    if *prev_online != Some(current_online) {
        *prev_online = Some(current_online);
        store
            .publish_channel_online(channel_id, current_online)
            .await;
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
    match level.to_lowercase().as_str() {
        "debug" | "verbose" => {
            protocol.set_log_config(ChannelLogConfig::all());
            log_handler.set_log_level("debug");
            Ok(())
        },
        "info" | "standard" => {
            protocol.set_log_config(ChannelLogConfig::default());
            log_handler.set_log_level("info");
            Ok(())
        },
        "error" | "minimal" => {
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
    // Subscribe before connecting so startup events (notably IEC 104 GI data)
    // cannot race ahead of the runtime receiver.
    let mut event_rx = if event_driven {
        protocol.subscribe()
    } else {
        None
    };

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
    update_cached_state(protocol.as_ref(), &ctx.cached_state);
    check_online_change(
        protocol.as_ref(),
        &mut prev_online,
        &ctx.store,
        ctx.channel_id,
    )
    .await;

    // Use configured poll interval
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(
        ctx.poll_interval_ms.get(),
    ));

    // Track previous error count to detect new errors
    let mut prev_error_count: u64 = 0;

    // Track consecutive poll cycles with zero successful data (for liveness detection)
    let mut consecutive_zero_data: u32 = 0;

    // Track failed state log frequency (per-channel, not static)
    let mut failed_log_tick_counter: u32 = 0;

    loop {
        // Use biased select to prioritize commands over polling
        // This ensures commands are processed promptly even during heavy polling
        tokio::select! {
            biased;

            // Priority 1: Protocol commands (connect/disconnect/diagnostics)
            Some(cmd) = protocol_rx.recv() => {
                // Shutdown must break the outer loop; handle_protocol_command
                // would only log it (the backoff branch handles its own copy).
                if matches!(cmd, ProtocolCommand::Shutdown) {
                    info!("Ch{} shutdown received, exiting loop", ctx.channel_id);
                    break;
                }
                if handle_protocol_command(
                    cmd, &mut protocol, &ctx.log_handler, ctx.channel_id,
                )
                .await
                {
                    break;
                }
            }

            // Priority 2: Business commands (control/adjustment from M2C SHM)
            Some(cmd) = business_rx.recv() => {
                handle_business_command(cmd, &mut protocol, &ctx).await;
            }

            // Priority 3: Data pushed by event-driven protocols.
            event = receive_protocol_event(&mut event_rx) => {
                match event {
                    Ok(event) => handle_protocol_event(event, &ctx, &mut prev_online).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("Ch{} event receiver lagged, skipped {} events", ctx.channel_id, skipped);
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        warn!("Ch{} event stream closed", ctx.channel_id);
                        event_rx = None;
                    },
                }
            }

            // Priority 4: Periodic polling
            _ = interval.tick() => {
                let action = handle_poll_tick(
                    &ctx, &mut protocol, &mut protocol_rx,
                    &mut reconnect_helper, &mut failed_log_tick_counter,
                    &mut prev_online, &mut prev_error_count,
                    &mut consecutive_zero_data,
                ).await;
                match action {
                    TickAction::Continue => continue,
                    TickAction::Break => break,
                    TickAction::Proceed => {}
                }
            }
        }
    }

    // Stop protocol background tasks (e.g. CAN receive/read loops) on any exit path.
    let _ = stop_events_and_disconnect(protocol.as_mut()).await;

    // Mark as disconnected on shutdown
    ctx.cached_state.store(
        crate::core::channels::types::ConnectionState::Disconnected.as_u8(),
        Ordering::Relaxed,
    );
    // Publish offline status to the SHM health plane on shutdown.
    ctx.store
        .publish_channel_online(ctx.channel_id, false)
        .await;
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
        ProtocolCommand::GetDiagnostics { response_tx } => {
            let diag = protocol.diagnostics().await.ok();
            let _ = response_tx.send(diag);
        },
        ProtocolCommand::GetConnectionState { response_tx } => {
            let state: crate::core::channels::types::ConnectionState =
                protocol.connection_state().into();
            let _ = response_tx.send(state);
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
    match cmd {
        ChannelCommand::Control {
            command_id,
            point_id,
            value,
            timestamp,
            expires_at_ms,
        } => {
            if let Err(error) = ctx.command_guard.validate(
                aether_model::PointType::Control,
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
                return;
            }
            match protocol.write_control(&[(point_id, value)]).await {
                Ok(n) if n > 0 => debug!("Ch{} control pt{} = {} ok", channel_id, point_id, value),
                Ok(_) => warn!("Ch{} control pt{} = {} failed", channel_id, point_id, value),
                Err(e) => error!("Ch{} control pt{} err: {}", channel_id, point_id, e),
            }
        },
        ChannelCommand::Adjustment {
            command_id,
            point_id,
            value,
            timestamp,
            expires_at_ms,
        } => {
            if let Err(error) = ctx.command_guard.validate(
                aether_model::PointType::Adjustment,
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
                return;
            }
            match protocol.write_adjustment(&[(point_id, value)]).await {
                Ok(n) if n > 0 => {
                    debug!("Ch{} adjustment pt{} = {} ok", channel_id, point_id, value)
                },
                Ok(_) => warn!(
                    "Ch{} adjustment pt{} = {} failed",
                    channel_id, point_id, value
                ),
                Err(e) => error!("Ch{} adjustment pt{} err: {}", channel_id, point_id, e),
            }
        },
        ChannelCommand::BatchControl {
            command_id,
            points,
            timestamp,
            expires_at_ms,
        } => {
            if let Some((point_id, error)) = points.iter().find_map(|(point_id, value)| {
                ctx.command_guard
                    .validate(
                        aether_model::PointType::Control,
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
                return;
            }
            match protocol.write_control(&points).await {
                Ok(n) => debug!("Ch{} batch control {}/{} ok", channel_id, n, points.len()),
                Err(e) => error!("Ch{} batch control err: {}", channel_id, e),
            }
        },
        ChannelCommand::BatchAdjustment {
            command_id,
            points,
            timestamp,
            expires_at_ms,
        } => {
            if let Some((point_id, error)) = points.iter().find_map(|(point_id, value)| {
                ctx.command_guard
                    .validate(
                        aether_model::PointType::Adjustment,
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
                return;
            }
            match protocol.write_adjustment(&points).await {
                Ok(n) => debug!("Ch{} batch adj {}/{} ok", channel_id, n, points.len()),
                Err(e) => error!("Ch{} batch adj err: {}", channel_id, e),
            }
        },
    }
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
    // Update watchdog heartbeat on every tick (proves task is alive)
    ctx.watchdog_heartbeat_ms
        .store(super::channel_entry::unix_timestamp_ms(), Ordering::Relaxed);

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
        ctx.reconnect_failed.store(false, Ordering::Relaxed);
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
        // Mark "data is flowing" — is_connected() requires this to stay fresh
        // so a TCP-up-but-Modbus-dead zombie reports as disconnected after
        // ~3 missed polls.
        ctx.last_successful_read_ms
            .store(super::channel_entry::unix_timestamp_ms(), Ordering::Relaxed);
        tracing::trace!("Ch{} poll ok: {} pts", ctx.channel_id, count);
        if let Err(e) = ctx.store.write_batch(ctx.channel_id, result.data).await {
            error!("Ch{} failed to write to SHM: {}", ctx.channel_id, e);
        }
    } else if !event_driven && ctx.zero_data_threshold > 0 {
        *consecutive_zero_data += 1;
        if *consecutive_zero_data >= ctx.zero_data_threshold {
            warn!(
                "Ch{} no data for {} consecutive cycles, triggering disconnect",
                ctx.channel_id, consecutive_zero_data
            );
            let _ = stop_events_and_disconnect(protocol.as_mut()).await;
            *consecutive_zero_data = 0;
            update_cached_state(protocol.as_ref(), &ctx.cached_state);
            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id).await;
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
        ctx.cached_diagnostics.store(Some(Arc::new(diag)));
    }

    // Update cached connection state after each poll cycle
    update_cached_state(protocol.as_ref(), &ctx.cached_state);
    check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id).await;

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
    ctx.reconnect_total_attempts
        .store(reconnect_helper.stats().total_attempts, Ordering::Relaxed);

    match reconnect_helper.connection_state() {
        ReconnectState::Failed => {
            ctx.reconnect_failed.store(true, Ordering::Relaxed);

            // Check auto-recovery before giving up
            if reconnect_helper.check_auto_recovery() {
                info!(
                    "Ch{} auto-recovery triggered, returning to Disconnected state",
                    ctx.channel_id
                );
                ctx.reconnect_failed.store(false, Ordering::Relaxed);
                *failed_log_tick_counter = 0;
                update_cached_state(protocol.as_ref(), &ctx.cached_state);
                check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id)
                    .await;
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
            update_cached_state(protocol.as_ref(), &ctx.cached_state);
            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id).await;
            TickAction::Continue
        },
        ReconnectState::Reconnecting => TickAction::Continue,
        ReconnectState::Connected | ReconnectState::Disconnected => {
            if reconnect_helper.connection_state() == ReconnectState::Connected {
                warn!("Ch{} connection lost unexpectedly", ctx.channel_id);
                reconnect_helper.mark_disconnected();
            }
            if !reconnect_helper.record_attempt() {
                update_cached_state(protocol.as_ref(), &ctx.cached_state);
                check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id)
                    .await;
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
                // Remain responsive to commands during backoff
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    Some(cmd) = protocol_rx.recv() => {
                        let action = handle_backoff_command(
                            cmd, protocol, reconnect_helper,
                            failed_log_tick_counter, &ctx.log_handler, ctx.channel_id,
                        ).await;
                        if let Some(a) = action {
                            update_cached_state(protocol.as_ref(), &ctx.cached_state);
                            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id).await;
                            return a;
                        }
                        update_cached_state(protocol.as_ref(), &ctx.cached_state);
                        check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id).await;
                        return TickAction::Continue;
                    }
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
                    ctx.reconnect_failed.store(false, Ordering::Relaxed);
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
            update_cached_state(protocol.as_ref(), &ctx.cached_state);
            check_online_change(protocol.as_ref(), prev_online, &ctx.store, ctx.channel_id).await;
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
        ProtocolCommand::GetConnectionState { response_tx } => {
            let state: crate::core::channels::types::ConnectionState =
                protocol.connection_state().into();
            let _ = response_tx.send(state);
        },
        ProtocolCommand::GetDiagnostics { response_tx } => {
            let diag = protocol.diagnostics().await.ok();
            let _ = response_tx.send(diag);
        },
        ProtocolCommand::SetLogLevel { level, response_tx } => {
            let result = apply_log_level(protocol.as_mut(), log_handler.as_ref(), &level);
            let _ = response_tx.send(result);
        },
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::protocols::core::data::DataBatch;
    use crate::protocols::core::error::{GatewayError, Result};
    use crate::protocols::core::traits::{ConnectionState, Diagnostics, PollResult};

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

    #[async_trait]
    impl ChannelRuntime for EventLifecycleProbe {
        fn id(&self) -> u32 {
            104
        }

        fn name(&self) -> &str {
            "event-lifecycle-probe"
        }

        fn protocol(&self) -> &str {
            "event-probe"
        }

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

        async fn write_control(&mut self, _commands: &[(u32, f64)]) -> Result<usize> {
            Ok(0)
        }

        async fn write_adjustment(&mut self, _adjustments: &[(u32, f64)]) -> Result<usize> {
            Ok(0)
        }

        fn subscribe(&self) -> Option<crate::protocols::core::DataEventReceiver> {
            None
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
}
