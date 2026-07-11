//! Channel entry types and metadata
//!
//! Contains ChannelEntry, ChannelMetadata, ChannelStats, and related helpers.

use arc_swap::ArcSwapOption;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::core::config::ChannelConfig;
use crate::protocols::core::logging::ChannelLogHandler;
use crate::protocols::gateway::ChannelRuntime;
use crate::runtime::reconnect::{AutoRecoveryPolicy, ReconnectPolicy};
use crate::store::ShmDataStore;

use super::channel_task::{ChannelPollContext, run_unified_channel_task};
use super::command_guard::CommandGuard;

/// Maximum number of channel slots (pre-allocated for O(1) access)
/// Channel IDs must be < MAX_CHANNELS
pub(crate) const MAX_CHANNELS: usize = 10000;

// ============================================================================
// Channel Types
// ============================================================================

/// Channel metadata
#[derive(Debug)]
pub struct ChannelMetadata {
    pub name: Arc<str>,
    pub protocol_type: String,
    pub created_at: Instant,
    /// Last accessed timestamp in milliseconds since Unix epoch (lock-free)
    pub last_accessed_ms: AtomicI64,
}

impl Clone for ChannelMetadata {
    fn clone(&self) -> Self {
        Self {
            name: Arc::clone(&self.name),
            protocol_type: self.protocol_type.clone(),
            created_at: self.created_at,
            last_accessed_ms: AtomicI64::new(self.last_accessed_ms.load(Ordering::Relaxed)),
        }
    }
}

/// Helper function to get current Unix timestamp in milliseconds
pub fn unix_timestamp_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => {
            warn!("System time error (clock before UNIX epoch?): {}", e);
            0
        },
    }
}

/// Channel entry with integrated protocol runtime and storage
///
/// ## Lock-Free Architecture
///
/// This struct uses message-passing instead of shared locks:
/// - Protocol client is owned by the unified channel task (not shared)
/// - External code sends `ProtocolCommand` via `protocol_tx` channel
/// - The unified task processes commands in its `tokio::select!` loop
/// - Results are returned via embedded `oneshot::Sender`
///
/// This eliminates lock contention between polling and command execution.
#[derive(Clone)]
pub struct ChannelEntry {
    /// Protocol command sender - for connect/disconnect/diagnostics operations
    /// Commands are processed by the unified channel task
    pub protocol_tx: tokio::sync::mpsc::Sender<super::types::ProtocolCommand>,
    /// Data store for persisting polled data
    pub store: Arc<ShmDataStore>,
    /// Unified channel task handle (polling + command execution)
    task_handle: Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
    /// Channel metadata (name, protocol type, etc.)
    pub metadata: ChannelMetadata,

    /// Channel configuration
    pub channel_config: Arc<ChannelConfig>,
    /// Direct command sender for M2C business commands (control/adjustment)
    pub command_tx: Option<tokio::sync::mpsc::Sender<super::traits::ChannelCommand>>,
    /// Cached connection state for non-blocking access (updated by unified task)
    cached_connection_state: Arc<AtomicU8>,
    /// Cached diagnostics for non-blocking access (updated by unified task after each poll)
    cached_diagnostics: Arc<ArcSwapOption<crate::protocols::core::traits::Diagnostics>>,

    // ── Watchdog shared fields (written by task, read by lifecycle/health) ──
    /// Heartbeat timestamp in millis since epoch (0 = task not yet started)
    pub(crate) watchdog_heartbeat_ms: Arc<AtomicI64>,
    /// Total reconnect attempts (synced from ReconnectHelper stats)
    pub(crate) reconnect_total_attempts: Arc<AtomicU64>,
    /// Whether reconnection has permanently failed
    pub(crate) reconnect_failed: Arc<AtomicBool>,
    /// Timestamp (millis since epoch) of the most recent poll cycle that
    /// returned at least one successful point. 0 means no successful poll
    /// has happened yet on this entry. Used by `is_connected()` to surface
    /// "TCP up but Modbus dead" zombies as disconnected to the UI.
    pub(crate) last_successful_read_ms: Arc<AtomicI64>,
    /// Per-channel freshness window derived from poll interval.
    data_freshness_timeout_ms: i64,
    /// Per-channel first-poll grace window derived from poll interval.
    first_poll_grace_ms: i64,
}

/// Minimum freshness window: preserves the old behavior for fast poll intervals.
const MIN_DATA_FRESHNESS_TIMEOUT_MS: i64 = 90_000;
/// Minimum first-poll grace window: avoids startup flapping for fast channels.
const MIN_FIRST_POLL_GRACE_MS: i64 = 60_000;

fn scaled_poll_window_ms(poll_interval_ms: u64, multiplier: u64, minimum_ms: i64) -> i64 {
    let scaled = poll_interval_ms.saturating_mul(multiplier);
    scaled.max(minimum_ms as u64).min(i64::MAX as u64) as i64
}

fn data_freshness_timeout_ms(poll_interval_ms: u64) -> i64 {
    scaled_poll_window_ms(poll_interval_ms, 3, MIN_DATA_FRESHNESS_TIMEOUT_MS)
}

fn first_poll_grace_ms(poll_interval_ms: u64) -> i64 {
    scaled_poll_window_ms(poll_interval_ms, 2, MIN_FIRST_POLL_GRACE_MS)
}

impl std::fmt::Debug for ChannelEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelEntry")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

/// Channel statistics
#[derive(Debug, Clone)]
pub struct ChannelStats {
    pub channel_id: u32,
    pub name: String,
    pub protocol_type: String,
    pub is_connected: bool,
    pub created_at: Instant,
    /// Last accessed timestamp in milliseconds since Unix epoch
    pub last_accessed_ms: i64,
    /// Watchdog heartbeat timestamp in millis since epoch (0 = not yet started)
    pub watchdog_heartbeat_ms: i64,
    /// Whether reconnection has permanently failed
    pub reconnect_failed: bool,
    /// Total reconnect attempts so far
    pub reconnect_total_attempts: u64,
}

impl ChannelEntry {
    /// Create new channel entry and start the unified channel task
    ///
    /// This method spawns a background task that owns the protocol client
    /// and processes both polling and commands via `tokio::select!`.
    pub(crate) fn new(
        protocol: Box<dyn ChannelRuntime>,
        store: Arc<ShmDataStore>,
        channel_config: Arc<ChannelConfig>,
        protocol_type: String,
        poll_interval_ms: u64,
        log_handler: Arc<dyn ChannelLogHandler>,
        command_guard: CommandGuard,
    ) -> Self {
        let metadata = ChannelMetadata {
            name: Arc::from(channel_config.name()),
            protocol_type,
            created_at: Instant::now(),
            last_accessed_ms: AtomicI64::new(unix_timestamp_ms()),
        };

        let channel_id = channel_config.id();

        // Create protocol command channel (for connect/disconnect/diagnostics)
        let (protocol_tx, protocol_rx) =
            tokio::sync::mpsc::channel::<super::types::ProtocolCommand>(32);

        // Create business command channel (for control/adjustment from M2C SHM)
        // Buffer size 1024 prevents backpressure drops during burst M2C traffic
        let (business_tx, business_rx) =
            tokio::sync::mpsc::channel::<super::traits::ChannelCommand>(1024);

        // Create shared connection state cache (initialized as Connecting)
        let cached_state = Arc::new(AtomicU8::new(
            super::types::ConnectionState::Connecting.as_u8(),
        ));
        let cached_state_clone = Arc::clone(&cached_state);

        // Create shared diagnostics cache (initialized as None)
        let cached_diagnostics = Arc::new(ArcSwapOption::empty());
        let cached_diagnostics_clone = Arc::clone(&cached_diagnostics);

        // Parse reconnection policy from channel parameters
        let reconnect_policy = parse_reconnect_policy(&channel_config.parameters);
        let auto_recovery_policy = parse_auto_recovery_policy(&channel_config.parameters);

        // Create watchdog shared atomics
        let watchdog_heartbeat_ms = Arc::new(AtomicI64::new(0));
        let reconnect_total_attempts = Arc::new(AtomicU64::new(0));
        let reconnect_failed = Arc::new(AtomicBool::new(false));
        let last_successful_read_ms = Arc::new(AtomicI64::new(0));

        let heartbeat_clone = Arc::clone(&watchdog_heartbeat_ms);
        let attempts_clone = Arc::clone(&reconnect_total_attempts);
        let failed_clone = Arc::clone(&reconnect_failed);
        let last_read_clone = Arc::clone(&last_successful_read_ms);

        // Parse zero-data liveness threshold (consecutive zero-data polls → disconnect)
        let zero_data_threshold = channel_config
            .parameters
            .get("zero_data_threshold")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(5);
        let data_freshness_timeout = data_freshness_timeout_ms(poll_interval_ms);
        let first_poll_grace = first_poll_grace_ms(poll_interval_ms);

        // Spawn the unified channel task
        let ctx = ChannelPollContext {
            store: Arc::clone(&store),
            channel_id,
            poll_interval_ms,
            cached_state: cached_state_clone,
            cached_diagnostics: cached_diagnostics_clone,
            log_handler,
            watchdog_heartbeat_ms: heartbeat_clone,
            reconnect_total_attempts: attempts_clone,
            reconnect_failed: failed_clone,
            last_successful_read_ms: last_read_clone,
            zero_data_threshold,
            command_guard,
        };
        let task_handle = tokio::spawn(async move {
            run_unified_channel_task(
                ctx,
                protocol,
                protocol_rx,
                business_rx,
                reconnect_policy,
                auto_recovery_policy,
            )
            .await;
        });

        Self {
            protocol_tx,
            store,
            task_handle: Arc::new(std::sync::Mutex::new(Some(task_handle))),
            metadata,
            channel_config,
            command_tx: Some(business_tx),
            cached_connection_state: cached_state,
            cached_diagnostics,
            watchdog_heartbeat_ms,
            reconnect_total_attempts,
            reconnect_failed,
            last_successful_read_ms,
            data_freshness_timeout_ms: data_freshness_timeout,
            first_poll_grace_ms: first_poll_grace,
        }
    }

    /// Get channel statistics
    pub async fn get_stats(&self, channel_id: u32) -> ChannelStats {
        let heartbeat = self.watchdog_heartbeat_ms.load(Ordering::Relaxed);
        // Use heartbeat as last_accessed if available (fixes bug: task never called touch())
        let last_accessed = if heartbeat > 0 {
            heartbeat
        } else {
            self.metadata.last_accessed_ms.load(Ordering::Relaxed)
        };

        ChannelStats {
            channel_id,
            name: self.metadata.name.to_string(),
            protocol_type: self.metadata.protocol_type.clone(),
            is_connected: self.is_connected(),
            created_at: self.metadata.created_at,
            last_accessed_ms: last_accessed,
            watchdog_heartbeat_ms: heartbeat,
            reconnect_failed: self.reconnect_failed.load(Ordering::Relaxed),
            reconnect_total_attempts: self.reconnect_total_attempts.load(Ordering::Relaxed),
        }
    }

    /// Update last accessed time (lock-free)
    pub fn touch(&self) {
        self.metadata
            .last_accessed_ms
            .store(unix_timestamp_ms(), Ordering::Relaxed);
    }

    /// Check if channel is connected.
    ///
    /// Combines two signals so the UI cannot show "Connected" when reads have
    /// silently stopped flowing:
    ///
    /// 1. The cached TCP-level connection state (set by the protocol runtime).
    /// 2. Recency of the last successful poll — at least one point must come
    ///    back within the per-channel freshness window.
    ///
    /// The first poll has a `FIRST_POLL_GRACE_MS` window after channel creation
    /// so we don't flap to disconnected before the loop has a chance to run.
    pub fn is_connected(&self) -> bool {
        let state_u8 = self.cached_connection_state.load(Ordering::Relaxed);
        if !super::types::ConnectionState::from_u8(state_u8).is_connected() {
            return false;
        }

        let last_read = self.last_successful_read_ms.load(Ordering::Relaxed);
        if last_read == 0 {
            // No successful poll yet on this entry. Trust TCP state only while
            // we are still inside the first-poll grace window; after that, a
            // protocol that has produced zero successful reads is treated as
            // disconnected even if the TCP socket appears up.
            let age_ms = self
                .metadata
                .created_at
                .elapsed()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            return age_ms < self.first_poll_grace_ms;
        }

        // We have at least one historical successful poll — require freshness.
        let age_ms = unix_timestamp_ms().saturating_sub(last_read);
        age_ms < self.data_freshness_timeout_ms
    }

    /// Get channel status.
    pub async fn get_status(&self) -> super::types::ChannelStatus {
        super::types::ChannelStatus {
            is_connected: self.is_connected(),
            last_update: chrono::Utc::now().timestamp(),
        }
    }

    /// Get cached diagnostics information (non-blocking).
    ///
    /// Returns the cached diagnostics that is updated by the unified channel task
    /// after each poll cycle. This is safe to call from API handlers without
    /// blocking on slow protocol operations.
    #[allow(clippy::disallowed_methods)]
    pub fn get_diagnostics(&self, channel_id: u32) -> serde_json::Value {
        match self.cached_diagnostics.load().as_deref() {
            Some(d) => serde_json::json!({
                "protocol_type": "unified",
                "connected": d.connection_state.is_connected(),
                "channel_id": channel_id,
                "error_count": d.error_count,
                "last_error": d.last_error,
                "read_count": d.read_count,
                "write_count": d.write_count,
                "protocol": d.protocol,
                "extra": d.extra
            }),
            None => serde_json::json!({
                "protocol_type": "unified",
                "connected": false,
                "channel_id": channel_id,
                "error_count": 0,
                "last_error": null
            }),
        }
    }

    /// Connect the channel
    ///
    /// Sends a Connect command to the unified channel task.
    pub async fn connect(&self) -> crate::error::Result<()> {
        use super::types::ProtocolCommand;
        use std::time::Duration;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.protocol_tx
            .send(ProtocolCommand::Connect { response_tx })
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_config.id()))?;

        // Add 30s timeout to prevent indefinite blocking on connect
        tokio::time::timeout(Duration::from_secs(30), response_rx)
            .await
            .map_err(|_| {
                crate::error::IoError::timeout(format!(
                    "Ch{} connect timeout (30s)",
                    self.channel_config.id()
                ))
            })?
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_config.id()))?
            .map_err(crate::error::IoError::from)
    }

    /// Disconnect the channel
    ///
    /// Sends a Disconnect command to the unified channel task.
    pub async fn disconnect(&self) -> crate::error::Result<()> {
        use super::types::ProtocolCommand;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.protocol_tx
            .send(ProtocolCommand::Disconnect { response_tx })
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_config.id()))?;

        response_rx
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_config.id()))?;

        Ok(())
    }

    /// Set the channel log level dynamically.
    ///
    /// Sends a SetLogLevel command to the unified channel task.
    /// Valid levels: "debug" (verbose), "info" (standard), "error" (minimal)
    pub async fn set_log_level(&self, level: &str) -> crate::error::Result<()> {
        use super::types::ProtocolCommand;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.protocol_tx
            .send(ProtocolCommand::SetLogLevel {
                level: level.to_string(),
                response_tx,
            })
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_config.id()))?;

        response_rx
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_config.id()))?
            .map_err(crate::error::IoError::ValidationError)
    }

    /// Get the channel ID from metadata name (parsed from config)
    pub fn channel_id(&self) -> u32 {
        self.channel_config.id()
    }

    /// Shutdown the unified channel task gracefully.
    ///
    /// Sends a Shutdown command to the unified task. The task will process
    /// the command and exit its loop cleanly, allowing proper resource cleanup.
    ///
    /// NOTE: This method does NOT abort the task immediately. Use `abort_task()`
    /// if you need to force-terminate after a timeout.
    pub fn shutdown(&self) {
        use super::types::ProtocolCommand;

        // Send shutdown command (fire-and-forget)
        // The unified task will receive this and break out of its loop
        let _ = self.protocol_tx.try_send(ProtocolCommand::Shutdown);
    }

    /// Force-abort the unified channel task.
    ///
    /// Use this only after `shutdown()` if the task doesn't exit in time.
    /// This is a last resort that may cause resource leaks.
    pub fn abort_task(&self) {
        if let Ok(mut handle) = self.task_handle.lock()
            && let Some(h) = handle.take()
            && !h.is_finished()
        {
            warn!(
                "Ch{} task did not exit gracefully, aborting",
                self.channel_id()
            );
            h.abort();
        }
    }

    /// Check if the unified task has finished.
    pub fn is_task_finished(&self) -> bool {
        match self.task_handle.lock() {
            Ok(handle) => {
                match handle.as_ref() {
                    Some(h) => h.is_finished(),
                    None => true, // Task was already taken/aborted
                }
            },
            _ => {
                true // Lock poisoned, assume finished
            },
        }
    }

    /// Take the task handle out for awaiting. Returns None if already taken.
    pub fn take_task_handle(&self) -> Option<JoinHandle<()>> {
        self.task_handle.lock().ok()?.take()
    }
}

/// Parse reconnection policy from channel parameters.
///
/// Supports: reconnect_max_attempts, reconnect_initial_delay_ms,
///           reconnect_max_delay_ms, reconnect_backoff_multiplier
fn parse_reconnect_policy(
    params: &std::collections::HashMap<String, serde_json::Value>,
) -> ReconnectPolicy {
    let max_attempts = params
        .get("reconnect_max_attempts")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(0); // 0 = unlimited

    let initial_delay_ms = params
        .get("reconnect_initial_delay_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(1000);

    let max_delay_ms = params
        .get("reconnect_max_delay_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(60000);

    let backoff_multiplier = params
        .get("reconnect_backoff_multiplier")
        .and_then(|v| v.as_f64())
        .unwrap_or(2.0);

    ReconnectPolicy::from_config(
        max_attempts,
        initial_delay_ms,
        max_delay_ms,
        backoff_multiplier,
    )
}

/// Parse auto-recovery policy from channel parameters.
///
/// Supports: watchdog_recovery_cooldown_secs, watchdog_max_recovery_rounds
/// Returns None if max_recovery_rounds is explicitly set to 0.
fn parse_auto_recovery_policy(
    params: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<AutoRecoveryPolicy> {
    let cooldown_secs = params
        .get("watchdog_recovery_cooldown_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(300);

    let max_rounds = params
        .get("watchdog_max_recovery_rounds")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(3);

    if max_rounds == 0 {
        return None;
    }

    Some(AutoRecoveryPolicy {
        cooldown: std::time::Duration::from_secs(cooldown_secs),
        max_recovery_rounds: max_rounds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_windows_keep_old_minimums_for_fast_polling() {
        assert_eq!(data_freshness_timeout_ms(1_000), 90_000);
        assert_eq!(first_poll_grace_ms(1_000), 60_000);
    }

    #[test]
    fn freshness_windows_scale_for_slow_polling() {
        assert_eq!(data_freshness_timeout_ms(120_000), 360_000);
        assert_eq!(first_poll_grace_ms(120_000), 240_000);
    }
}
