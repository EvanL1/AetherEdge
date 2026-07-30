//! Channel entry types and metadata
//!
//! Contains ChannelEntry, ChannelMetadata, ChannelStats, and related helpers.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::error::Result;
use crate::protocols::core::logging::ChannelLogHandler;
use crate::protocols::runtime::ChannelRuntime;
use crate::store::ShmDataStore;

use super::channel_task::{ChannelPollContext, ChannelSharedState, run_unified_channel_task};
use super::command_guard::CommandGuard;
use super::runtime_policy::ChannelRuntimePolicy;

/// Maximum number of channel slots (pre-allocated for O(1) access)
/// Channel IDs must be < MAX_CHANNELS
pub(crate) const MAX_CHANNELS: usize = 10000;

// ============================================================================
// Channel Types
// ============================================================================

/// Channel metadata
#[derive(Debug)]
pub struct ChannelMetadata {
    pub name: String,
    pub protocol_type: &'static str,
    pub created_at: Instant,
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
pub struct ChannelEntry {
    /// Protocol command sender - for connect/disconnect/diagnostics operations
    /// Commands are processed by the unified channel task
    pub protocol_tx: tokio::sync::mpsc::Sender<super::types::ProtocolCommand>,
    /// Unified channel task handle (polling + command execution)
    task_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
    /// Channel metadata (name, protocol type, etc.)
    pub metadata: ChannelMetadata,

    channel_id: u32,
    /// Lock-free connection, diagnostics, watchdog, and freshness state shared
    /// with the unified channel task through one allocation.
    shared: Arc<ChannelSharedState>,
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
#[derive(Debug)]
pub struct ChannelStats {
    pub channel_id: u32,
    pub is_connected: bool,
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
        channel_id: u32,
        channel_name: String,
        protocol_type: &'static str,
        runtime_policy: ChannelRuntimePolicy,
        log_handler: Arc<dyn ChannelLogHandler>,
        command_guard: CommandGuard,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Sender<super::traits::ChannelCommand>,
    )> {
        let poll_interval_ms = runtime_policy.poll_interval_ms;
        let poll_interval_value = poll_interval_ms.get();

        // Create protocol command channel (for connect/disconnect/diagnostics)
        let (protocol_tx, protocol_rx) =
            tokio::sync::mpsc::channel::<super::types::ProtocolCommand>(32);

        // Create business command channel (for control/adjustment from M2C SHM)
        // Buffer size 1024 prevents backpressure drops during burst M2C traffic
        let (business_tx, business_rx) =
            tokio::sync::mpsc::channel::<super::traits::ChannelCommand>(1024);

        let shared = Arc::new(ChannelSharedState::new(
            super::types::ConnectionState::Connecting.as_u8(),
        ));

        let metadata = ChannelMetadata {
            name: channel_name,
            protocol_type,
            created_at: Instant::now(),
        };
        let data_freshness_timeout = data_freshness_timeout_ms(poll_interval_value);
        let first_poll_grace = first_poll_grace_ms(poll_interval_value);

        // Spawn the unified channel task
        let ctx = ChannelPollContext {
            store,
            channel_id,
            poll_interval_ms,
            shared: Arc::clone(&shared),
            log_handler,
            zero_data_threshold: runtime_policy.zero_data_threshold,
            command_guard,
        };
        let task_handle = tokio::spawn(async move {
            run_unified_channel_task(
                ctx,
                protocol,
                protocol_rx,
                business_rx,
                runtime_policy.reconnect,
                runtime_policy.auto_recovery,
            )
            .await;
        });

        Ok((
            Self {
                protocol_tx,
                task_handle: std::sync::Mutex::new(Some(task_handle)),
                metadata,
                channel_id,
                shared,
                data_freshness_timeout_ms: data_freshness_timeout,
                first_poll_grace_ms: first_poll_grace,
            },
            business_tx,
        ))
    }

    /// Get channel statistics
    pub fn get_stats(&self) -> ChannelStats {
        let heartbeat = self.shared.watchdog_heartbeat_ms.load(Ordering::Relaxed);

        ChannelStats {
            channel_id: self.channel_id,
            is_connected: self.is_connected(),
            watchdog_heartbeat_ms: heartbeat,
            reconnect_failed: self.shared.reconnect_failed.load(Ordering::Relaxed),
            reconnect_total_attempts: self.shared.reconnect_total_attempts.load(Ordering::Relaxed),
        }
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
        let state_u8 = self.shared.cached_connection_state.load(Ordering::Relaxed);
        if !super::types::ConnectionState::from_u8(state_u8).is_connected() {
            return false;
        }

        let last_read = self.shared.last_successful_read_ms.load(Ordering::Relaxed);
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

    /// Last successful live-state commit timestamp, if this entry has produced one.
    pub fn last_successful_read_ms(&self) -> Option<i64> {
        match self.shared.last_successful_read_ms.load(Ordering::Relaxed) {
            0 => None,
            timestamp => Some(timestamp),
        }
    }

    /// Get cached diagnostics information (non-blocking).
    ///
    /// Returns the cached diagnostics that is updated by the unified channel task
    /// after each poll cycle. This is safe to call from API handlers without
    /// blocking on slow protocol operations.
    #[allow(clippy::disallowed_methods)]
    pub fn get_diagnostics(&self) -> serde_json::Value {
        match self.shared.cached_diagnostics.load().as_deref() {
            Some(d) => serde_json::json!({
                "protocol_type": "unified",
                "connected": d.connection_state.is_connected(),
                "channel_id": self.channel_id,
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
                "channel_id": self.channel_id,
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
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_id))?;

        // Add 30s timeout to prevent indefinite blocking on connect
        tokio::time::timeout(Duration::from_secs(30), response_rx)
            .await
            .map_err(|_| {
                crate::error::IoError::timeout(format!(
                    "Ch{} connect timeout (30s)",
                    self.channel_id
                ))
            })?
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_id))?
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
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_id))?;

        response_rx
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_id))?;

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
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_id))?;

        response_rx
            .await
            .map_err(|_| crate::error::IoError::channel_not_found(self.channel_id))?
            .map_err(crate::error::IoError::ValidationError)
    }

    /// Get the channel ID from metadata name (parsed from config)
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    /// Shutdown the unified channel task gracefully.
    ///
    /// Sends a Shutdown command to the unified task. The task will process
    /// the command and exit its loop cleanly, allowing proper resource cleanup.
    pub(crate) async fn shutdown(&self) -> bool {
        use super::types::ProtocolCommand;

        self.protocol_tx
            .send(ProtocolCommand::Shutdown)
            .await
            .is_ok()
    }

    /// Take the task handle out for awaiting. Returns None if already taken.
    pub fn take_task_handle(&self) -> Option<JoinHandle<()>> {
        self.task_handle.lock().ok()?.take()
    }
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
