//! Legacy UDS notification sender retained for compatibility tests/benchmarks.
//!
//! Used by automation to send M2C command notifications to io.
//! Supports graceful degradation: on connection loss the notifier does not block,
//! and automatically reconnects using exponential backoff (1s–5s). Once the UDS
//! listener becomes available again, notifications resume transparently.
//!
//! ## Reliability Enhancements
//!
//! `notify()` returns `NotifyResult` instead of a simple `io::Result<()>`,
//! allowing callers to distinguish:
//! - Successfully sent (`uds_sent = true`)
//! - Degraded to fallback delivery (`fallback_used = true`)
//! - Completely disabled (`disabled = true`)

use std::io;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_model::PointType;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tracing::{debug, info, warn};

use crate::notification::ShmNotification;

/// Default UDS path
pub const DEFAULT_UDS_PATH: &str = "/tmp/aether-m2c.sock";

/// UDS notification result
///
/// Provides detailed send status, allowing callers to take action based on the result.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotifyResult {
    /// UDS send succeeded
    pub uds_sent: bool,
    /// Degraded to fallback path (UDS send failed)
    pub fallback_used: bool,
    /// Notifications completely disabled (path is empty or unconfigured)
    pub disabled: bool,
}

impl NotifyResult {
    /// UDS send succeeded
    fn sent() -> Self {
        Self {
            uds_sent: true,
            ..Self::default()
        }
    }

    /// UDS failed, degraded to fallback path
    fn degraded() -> Self {
        Self {
            fallback_used: true,
            ..Self::default()
        }
    }

    /// Notifications completely disabled
    fn off() -> Self {
        Self {
            disabled: true,
            ..Self::default()
        }
    }

    /// Check if sent successfully (via UDS)
    #[inline]
    pub fn is_success(&self) -> bool {
        self.uds_sent
    }

    /// Check if UDS notification failed and delivery is degraded
    #[inline]
    pub fn is_degraded(&self) -> bool {
        self.fallback_used
    }
}

/// SHM command notification sender
///
/// Sends M2C command notifications to io via Unix Domain Socket.
/// Supports graceful degradation: if connection fails, notifications are silently ignored.
/// Supports auto-reconnection: uses exponential backoff strategy after disconnection.
pub struct ShmNotifier {
    stream: Option<UnixStream>,
    path: String,
    producer_id: u64,
    next_seq: u64,
    /// Last connection attempt time
    last_connect_attempt: Option<Instant>,
    /// Current backoff duration (milliseconds)
    backoff_ms: u64,
    /// Consecutive reconnect failure count (reset to 0 on success)
    reconnect_attempts: u32,
}

impl ShmNotifier {
    /// Minimum backoff duration (milliseconds)
    const MIN_BACKOFF_MS: u64 = 1000; // 1 second
    /// Maximum backoff duration (milliseconds)
    const MAX_BACKOFF_MS: u64 = 5_000; // 5 seconds — sufficient for same-host UDS
    /// Send retry count
    const MAX_RETRIES: u32 = 3;
    /// Retry interval (milliseconds)
    const RETRY_DELAY_MS: u64 = 10;

    fn new_producer_id() -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        now ^ ((std::process::id() as u64) << 32)
    }

    /// Connect to UDS listener
    ///
    /// If connection fails, returns a disabled notifier (notifications will be ignored).
    /// Subsequent calls to `notify()` will automatically attempt reconnection.
    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        match UnixStream::connect(&path_str).await {
            Ok(stream) => {
                debug!("ShmNotifier connected to {}", path_str);
                Ok(Self {
                    stream: Some(stream),
                    path: path_str,
                    producer_id: Self::new_producer_id(),
                    next_seq: 1,
                    last_connect_attempt: None,
                    backoff_ms: Self::MIN_BACKOFF_MS,
                    reconnect_attempts: 0,
                })
            },
            Err(e) => {
                warn!(
                    "ShmNotifier: UDS connect failed ({}), will retry on notify: {}",
                    path_str, e
                );
                Ok(Self {
                    stream: None,
                    path: path_str,
                    producer_id: Self::new_producer_id(),
                    next_seq: 1,
                    last_connect_attempt: Some(Instant::now()),
                    backoff_ms: Self::MIN_BACKOFF_MS,
                    reconnect_attempts: 0,
                })
            },
        }
    }

    /// Connect using the default path
    pub async fn connect_default() -> io::Result<Self> {
        Self::connect(DEFAULT_UDS_PATH).await
    }

    /// Create a disabled notifier (for testing or scenarios that don't need notifications)
    pub fn disabled() -> Self {
        Self {
            stream: None,
            path: String::new(),
            producer_id: Self::new_producer_id(),
            next_seq: 1,
            last_connect_attempt: None,
            backoff_ms: Self::MIN_BACKOFF_MS,
            reconnect_attempts: 0,
        }
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Get connection path
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Send notification
    ///
    /// If not connected, attempts reconnection (using exponential backoff).
    /// Retries up to 3 times on send failure; after all retries fail, marks as disconnected
    /// and triggers reconnection on next call.
    ///
    /// ## Return Value
    ///
    /// Returns `NotifyResult` instead of `io::Result<()>`, allowing callers to distinguish:
    /// - `uds_sent = true`: UDS send succeeded, command will be processed via low-latency path
    /// - `fallback_used = true`: UDS failed, degraded to fallback path
    /// - `disabled = true`: Notification feature is completely disabled
    ///
    /// ## Usage Example
    ///
    /// ```rust,ignore
    /// let result = notifier
    ///     .notify(channel_id, point_type, point_id, value, timestamp_ms, expires_at_ms)
    ///     .await;
    /// if result.is_degraded() {
    ///     // UDS failed, delivery to io is not guaranteed
    /// }
    /// ```
    pub async fn notify(
        &mut self,
        channel_id: u32,
        point_type: PointType,
        point_id: u32,
        value: f64,
        timestamp_ms: u64,
        expires_at_ms: u64,
    ) -> NotifyResult {
        if self.path.is_empty() {
            return NotifyResult::off();
        }

        self.try_reconnect().await;

        if let Some(ref mut stream) = self.stream {
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1).max(1);
            let bytes = ShmNotification::new(
                channel_id,
                point_type,
                point_id,
                value,
                timestamp_ms,
                expires_at_ms,
                self.producer_id,
                seq,
            )
            .to_bytes();

            for attempt in 0..Self::MAX_RETRIES {
                match stream.write_all(&bytes).await {
                    Ok(_) => {
                        debug!(
                            "ShmNotifier: sent ch={} type={:?} point={} seq={}",
                            channel_id, point_type, point_id, seq
                        );
                        return NotifyResult::sent();
                    },
                    Err(e) if attempt < Self::MAX_RETRIES - 1 => {
                        warn!(
                            "ShmNotifier: attempt {} failed: {}, retrying",
                            attempt + 1,
                            e
                        );
                        tokio::time::sleep(Duration::from_millis(Self::RETRY_DELAY_MS)).await;
                    },
                    Err(e) => {
                        warn!(
                            "ShmNotifier: all {} retries failed for ch{}:{}:{}: {}",
                            Self::MAX_RETRIES,
                            channel_id,
                            point_type.as_str(),
                            point_id,
                            e
                        );
                        self.stream = None;
                        self.last_connect_attempt = Some(Instant::now());
                        return NotifyResult::degraded();
                    },
                }
            }
        }

        NotifyResult::degraded()
    }

    /// Attempt reconnection (if disconnected and backoff time has elapsed)
    async fn try_reconnect(&mut self) {
        // Already connected or path is empty, skip
        if self.stream.is_some() || self.path.is_empty() {
            return;
        }

        // Check backoff time
        if let Some(last_attempt) = self.last_connect_attempt
            && last_attempt.elapsed().as_millis() < self.backoff_ms as u128
        {
            return; // Within backoff period, skip
        }

        // Attempt reconnection
        match UnixStream::connect(&self.path).await {
            Ok(stream) => {
                self.stream = Some(stream);
                self.backoff_ms = Self::MIN_BACKOFF_MS;
                self.last_connect_attempt = None;
                self.reconnect_attempts = 0;
                info!("ShmNotifier: reconnected to {}", self.path);
            },
            Err(e) => {
                // Increase backoff duration (exponential backoff)
                self.reconnect_attempts += 1;
                self.backoff_ms = (self.backoff_ms * 2).min(Self::MAX_BACKOFF_MS);
                self.last_connect_attempt = Some(Instant::now());
                if self.reconnect_attempts.is_multiple_of(10) {
                    warn!(
                        "ShmNotifier: {} consecutive reconnect failures, UDS notifications degraded (backoff {}ms): {}",
                        self.reconnect_attempts, self.backoff_ms, e
                    );
                } else {
                    debug!(
                        "ShmNotifier: reconnect failed (attempt {}, backoff {}ms): {}",
                        self.reconnect_attempts, self.backoff_ms, e
                    );
                }
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_disabled_notifier() {
        let mut notifier = ShmNotifier::disabled();
        assert!(!notifier.is_connected());

        // Disabled notifier should return disabled = true
        let result = notifier
            .notify(1001, PointType::Control, 0, 1.0, 123, 5_123)
            .await;
        assert!(result.disabled);
        assert!(!result.uds_sent);
        assert!(!result.fallback_used);
    }

    #[tokio::test]
    async fn test_connect_nonexistent_path() {
        let notifier = ShmNotifier::connect("/tmp/nonexistent-test-socket.sock")
            .await
            .unwrap();

        // Connection failed, but returns a disabled notifier instead of an error
        assert!(!notifier.is_connected());
    }

    #[tokio::test]
    async fn test_notify_result_helpers() {
        let success = NotifyResult::sent();
        assert!(success.is_success());
        assert!(!success.is_degraded());

        let fallback = NotifyResult::degraded();
        assert!(!fallback.is_success());
        assert!(fallback.is_degraded());

        let disabled = NotifyResult::off();
        assert!(!disabled.is_success());
        assert!(!disabled.is_degraded());
    }
}
