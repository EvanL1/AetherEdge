//! Reconnection mechanism implementation
//!
//! Provides a generic reconnection helper with exponential backoff and jitter support

use rand::Rng;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Default cooldown before auto-recovery attempt (seconds)
const DEFAULT_RECOVERY_COOLDOWN_SECS: u64 = 300;

/// Default maximum recovery rounds before permanent failure
const DEFAULT_MAX_RECOVERY_ROUNDS: u32 = 3;

/// Default initial delay between reconnection attempts (seconds)
const DEFAULT_INITIAL_DELAY_SECS: u64 = 1;

/// Default maximum delay between reconnection attempts (seconds)
const DEFAULT_MAX_DELAY_SECS: u64 = 60;

/// Reconnection error types
#[derive(Error, Debug)]
pub enum ReconnectError {
    /// Maximum retry attempts exceeded
    #[error("Maximum reconnection attempts exceeded")]
    MaxAttemptsExceeded,

    /// Connection failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Reconnection was cancelled
    #[error("Reconnection cancelled")]
    Cancelled,
}

/// Reconnection state enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectState {
    /// Successfully connected
    Connected,
    /// Disconnected
    Disconnected,
    /// Currently reconnecting
    Reconnecting,
    /// Reconnection failed (max attempts reached)
    Failed,
}

/// Auto-recovery policy for channels that reach Failed state.
///
/// After all reconnect attempts are exhausted (Failed state), the channel enters
/// a cooldown period. Once cooldown expires, reconnection is automatically retried
/// from scratch. This repeats up to `max_recovery_rounds` times before giving up permanently.
#[derive(Debug, Clone)]
pub struct AutoRecoveryPolicy {
    /// Cooldown duration before auto-recovery attempt (default: 300s)
    pub cooldown: Duration,
    /// Maximum recovery rounds (default: 3, 0 = disabled)
    pub max_recovery_rounds: u32,
}

impl Default for AutoRecoveryPolicy {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(DEFAULT_RECOVERY_COOLDOWN_SECS),
            max_recovery_rounds: DEFAULT_MAX_RECOVERY_ROUNDS,
        }
    }
}

/// Reconnection policy configuration
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Maximum retry attempts (0 means unlimited)
    pub max_attempts: u32,
    /// Initial delay between attempts
    pub initial_delay: Duration,
    /// Maximum delay between attempts
    pub max_delay: Duration,
    /// Backoff multiplier for exponential delay
    pub backoff_multiplier: f64,
    /// Whether to add jitter to delays
    pub jitter: bool,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_secs(DEFAULT_INITIAL_DELAY_SECS),
            max_delay: Duration::from_secs(DEFAULT_MAX_DELAY_SECS),
            backoff_multiplier: 2.0,
            jitter: true,
        }
    }
}

impl ReconnectPolicy {
    /// Create from configuration values
    pub fn from_config(
        max_attempts: u32,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        backoff_multiplier: f64,
    ) -> Self {
        Self {
            max_attempts,
            initial_delay: Duration::from_millis(initial_delay_ms),
            max_delay: Duration::from_millis(max_delay_ms),
            backoff_multiplier,
            jitter: true,
        }
    }
}

/// Reconnection context tracking current state and attempts
#[derive(Debug, Clone)]
pub struct ReconnectContext {
    /// Current retry attempt count
    pub current_attempt: u32,
    /// Last retry attempt time
    pub last_attempt: Option<Instant>,
    /// Next scheduled retry time
    pub next_attempt: Option<Instant>,
    /// Reconnection state
    pub connection_state: ReconnectState,
}

impl Default for ReconnectContext {
    fn default() -> Self {
        Self {
            current_attempt: 0,
            last_attempt: None,
            next_attempt: None,
            connection_state: ReconnectState::Disconnected,
        }
    }
}

/// Reconnection statistics tracking
#[derive(Debug, Default, Clone)]
pub struct ReconnectStats {
    /// Total reconnection attempts
    pub total_attempts: u64,
    /// Successful reconnection count
    pub successful_reconnects: u64,
    /// Failed reconnection count
    pub failed_reconnects: u64,
    /// Last successful connection time
    pub last_connected: Option<Instant>,
    /// Connection start time
    pub connection_start: Option<Instant>,
}

/// Generic reconnection helper with backoff and statistics
#[derive(Debug)]
pub struct ReconnectHelper {
    /// Reconnection policy configuration
    policy: ReconnectPolicy,
    /// Current reconnection context
    context: ReconnectContext,
    /// Connection statistics
    stats: ReconnectStats,
    /// Auto-recovery policy (None = disabled)
    auto_recovery: Option<AutoRecoveryPolicy>,
    /// Timestamp when the helper entered Failed state
    failed_at: Option<Instant>,
    /// Number of auto-recovery rounds completed
    recovery_rounds: u32,
}

impl ReconnectHelper {
    /// Create a new reconnection helper
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            context: ReconnectContext::default(),
            stats: ReconnectStats::default(),
            auto_recovery: None,
            failed_at: None,
            recovery_rounds: 0,
        }
    }

    /// Enable auto-recovery with the given policy (builder pattern)
    pub fn with_auto_recovery(mut self, policy: AutoRecoveryPolicy) -> Self {
        if policy.max_recovery_rounds > 0 {
            self.auto_recovery = Some(policy);
        }
        self
    }

    /// Get the current connection state
    pub fn connection_state(&self) -> ReconnectState {
        self.context.connection_state
    }

    /// Get connection statistics
    pub fn stats(&self) -> &ReconnectStats {
        &self.stats
    }

    /// Reset the reconnection context
    pub fn reset(&mut self) {
        self.context.current_attempt = 0;
        self.context.last_attempt = None;
        self.context.next_attempt = None;
        if self.context.connection_state != ReconnectState::Connected {
            self.context.connection_state = ReconnectState::Disconnected;
        }
    }

    /// Check if auto-recovery should trigger.
    ///
    /// Returns true if the cooldown has elapsed and recovery was performed.
    /// The helper is reset to Disconnected state, allowing reconnection to start over.
    pub fn check_auto_recovery(&mut self) -> bool {
        let (cooldown, max_rounds) = match &self.auto_recovery {
            Some(p) => (p.cooldown, p.max_recovery_rounds),
            None => return false,
        };

        if self.context.connection_state != ReconnectState::Failed {
            return false;
        }

        // Record when we first entered Failed state
        let failed_at = *self.failed_at.get_or_insert_with(Instant::now);

        // Check if we've exceeded max recovery rounds
        if self.recovery_rounds >= max_rounds {
            return false;
        }

        // Check if cooldown has elapsed
        if failed_at.elapsed() < cooldown {
            return false;
        }

        // Trigger auto-recovery
        self.recovery_rounds += 1;
        self.failed_at = None; // Reset for next round
        self.reset();

        info!(
            "Auto-recovery triggered (round {}/{})",
            self.recovery_rounds, max_rounds
        );

        true
    }

    /// Get remaining cooldown time before next auto-recovery attempt.
    /// Returns None if auto-recovery is disabled, exhausted, or not in Failed state.
    pub fn recovery_cooldown_remaining(&self) -> Option<Duration> {
        let policy = self.auto_recovery.as_ref()?;

        if self.context.connection_state != ReconnectState::Failed {
            return None;
        }
        if self.recovery_rounds >= policy.max_recovery_rounds {
            return None;
        }

        let failed_at = self.failed_at?;
        let elapsed = failed_at.elapsed();
        if elapsed >= policy.cooldown {
            Some(Duration::ZERO)
        } else {
            Some(policy.cooldown - elapsed)
        }
    }

    /// Get the number of recovery rounds completed
    pub fn recovery_rounds(&self) -> u32 {
        self.recovery_rounds
    }

    /// Mark the connection as successful
    pub fn mark_connected(&mut self) {
        self.context.connection_state = ReconnectState::Connected;
        self.context.current_attempt = 0;
        self.stats.last_connected = Some(Instant::now());
        self.stats.connection_start = Some(Instant::now());
        // Successful connection resets auto-recovery tracking
        self.failed_at = None;
        self.recovery_rounds = 0;
        debug!("Connection marked as successful");
    }

    /// Mark the connection as disconnected
    pub fn mark_disconnected(&mut self) {
        self.context.connection_state = ReconnectState::Disconnected;
        self.stats.connection_start = None;
        debug!("Connection marked as disconnected");
    }

    /// Record a reconnection attempt manually (for use without execute_reconnect)
    ///
    /// Returns true if the attempt is allowed, false if max attempts already reached.
    /// This method checks the limit BEFORE incrementing, consistent with execute_reconnect.
    pub fn record_attempt(&mut self) -> bool {
        // Check if maximum retry attempts reached BEFORE incrementing
        if self.policy.max_attempts > 0 && self.context.current_attempt >= self.policy.max_attempts
        {
            self.context.connection_state = ReconnectState::Failed;
            self.failed_at.get_or_insert_with(Instant::now);
            warn!(
                "Maximum reconnection attempts ({}) exceeded",
                self.policy.max_attempts
            );
            return false;
        }

        // Increment attempt counter
        self.context.current_attempt += 1;
        self.stats.total_attempts += 1;
        self.context.last_attempt = Some(Instant::now());
        self.context.connection_state = ReconnectState::Reconnecting;

        info!(
            "Reconnection attempt {}/{}",
            self.context.current_attempt,
            if self.policy.max_attempts == 0 {
                "∞".to_string()
            } else {
                self.policy.max_attempts.to_string()
            }
        );
        true
    }

    /// Mark a reconnection attempt as failed (for manual use)
    pub fn record_failure(&mut self) {
        self.stats.failed_reconnects += 1;
        if self.policy.max_attempts > 0 && self.context.current_attempt >= self.policy.max_attempts
        {
            self.context.connection_state = ReconnectState::Failed;
            self.failed_at.get_or_insert_with(Instant::now);
            debug!(
                "Reconnection attempt {} failed, max attempts reached",
                self.context.current_attempt
            );
        } else {
            self.context.connection_state = ReconnectState::Disconnected;
            debug!(
                "Reconnection attempt {}/{} failed, will retry",
                self.context.current_attempt,
                if self.policy.max_attempts == 0 {
                    "∞".to_string()
                } else {
                    self.policy.max_attempts.to_string()
                }
            );
        }
    }

    /// Calculate the next retry delay with exponential backoff
    pub fn calculate_next_delay(&self) -> Duration {
        let attempt = self.context.current_attempt.saturating_sub(1);
        let base_delay = self.policy.initial_delay;
        let multiplier = self.policy.backoff_multiplier;

        // Compute the multiplier in f64 and clamp BEFORE handing to Duration::mul_f64,
        // which panics when the result overflows Duration (~u64::MAX seconds).
        // With max_attempts=0 (unlimited) the attempt counter can grow until
        // 2^attempt overflows; clamp to (max_delay / base_delay) so the
        // subsequent cap step is purely a formality and Duration::mul_f64 cannot panic.
        let factor = multiplier.powi(attempt as i32);
        let max_factor = if base_delay.is_zero() {
            1.0
        } else {
            self.policy.max_delay.as_secs_f64() / base_delay.as_secs_f64()
        };
        let safe_factor = factor.clamp(0.0, max_factor.max(1.0));

        let mut delay = base_delay.mul_f64(safe_factor);

        // Cap at maximum delay (defense-in-depth; clamp above already enforces this).
        if delay > self.policy.max_delay {
            delay = self.policy.max_delay;
        }

        // Add jitter (±25% of delay)
        if self.policy.jitter {
            let jitter_range = delay.as_millis() as f64 * 0.25;
            let jitter = rand::thread_rng().gen_range(-jitter_range..jitter_range);
            let delay_ms = (delay.as_millis() as f64 + jitter).max(0.0);
            delay = Duration::from_millis(delay_ms as u64);
        }

        delay
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exponential_backoff() {
        let policy = ReconnectPolicy {
            max_attempts: 5,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // First attempt has no delay
        assert_eq!(helper.context.current_attempt, 0);

        // Set current attempt count and validate delay
        helper.context.current_attempt = 1;
        assert_eq!(helper.calculate_next_delay(), Duration::from_millis(100));

        helper.context.current_attempt = 2;
        assert_eq!(helper.calculate_next_delay(), Duration::from_millis(200));

        helper.context.current_attempt = 3;
        assert_eq!(helper.calculate_next_delay(), Duration::from_millis(400));

        helper.context.current_attempt = 4;
        assert_eq!(helper.calculate_next_delay(), Duration::from_millis(800));
    }

    #[tokio::test]
    async fn test_unlimited_retry_does_not_panic_on_overflow() {
        // Regression test for the production crash where io would panic in
        // Duration::mul_f64(2.0_f64.powi(64)) after ~65 reconnect attempts when
        // max_attempts=0 (unlimited). With panic = "abort" in release, this
        // SIGABRTed the process and Docker restarted it ~hourly while a
        // Modbus device stayed unreachable.
        let policy = ReconnectPolicy {
            max_attempts: 0, // unlimited — production default
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // Walk attempt counter past the historical crash point.
        for n in [1u32, 10, 50, 65, 100, 1_000, u32::MAX] {
            helper.context.current_attempt = n;
            let delay = helper.calculate_next_delay();
            assert!(
                delay <= Duration::from_secs(60),
                "attempt={} must stay capped at max_delay, got {:?}",
                n,
                delay
            );
        }
    }

    #[tokio::test]
    async fn test_max_delay_limit() {
        let policy = ReconnectPolicy {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(5),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // Test that delay doesn't exceed maximum
        helper.context.current_attempt = 10;
        let delay = helper.calculate_next_delay();
        assert!(delay <= Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_record_attempt_manual() {
        let policy = ReconnectPolicy {
            max_attempts: 3,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // First attempt - should succeed
        assert!(helper.record_attempt());
        assert_eq!(helper.context.current_attempt, 1);
        assert_eq!(
            helper.context.connection_state,
            ReconnectState::Reconnecting
        );

        // Simulate failure
        helper.record_failure();
        assert_eq!(
            helper.context.connection_state,
            ReconnectState::Disconnected
        );
        assert_eq!(helper.stats.failed_reconnects, 1);

        // Second attempt - should succeed
        assert!(helper.record_attempt());
        assert_eq!(helper.context.current_attempt, 2);

        // Simulate failure again
        helper.record_failure();
        assert_eq!(helper.stats.failed_reconnects, 2);

        // Third attempt - should succeed
        assert!(helper.record_attempt());
        assert_eq!(helper.context.current_attempt, 3);

        // Fourth attempt - should fail (max_attempts = 3)
        assert!(!helper.record_attempt());
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);
    }

    #[tokio::test]
    async fn test_unlimited_attempts() {
        let policy = ReconnectPolicy {
            max_attempts: 0, // Unlimited
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // Should always return true for unlimited attempts
        for i in 1..=100 {
            assert!(helper.record_attempt());
            assert_eq!(helper.context.current_attempt, i);
            helper.record_failure();
        }

        // State should still be Disconnected, not Failed
        assert_eq!(
            helper.context.connection_state,
            ReconnectState::Disconnected
        );
    }

    #[tokio::test]
    async fn test_mark_connected_resets_state() {
        let policy = ReconnectPolicy {
            max_attempts: 3,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // Simulate 2 failed attempts
        helper.record_attempt();
        helper.record_failure();
        helper.record_attempt();
        assert_eq!(helper.context.current_attempt, 2);

        // Successful connection resets attempt count
        helper.mark_connected();
        assert_eq!(helper.context.current_attempt, 0);
        assert_eq!(helper.context.connection_state, ReconnectState::Connected);

        // Next disconnection should start from attempt 1 again
        helper.mark_disconnected();
        helper.record_attempt();
        assert_eq!(helper.context.current_attempt, 1);
    }

    // ========================================================================
    // Auto-Recovery Tests
    // ========================================================================

    #[test]
    fn test_auto_recovery_disabled_by_default() {
        let policy = ReconnectPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy);

        // Push to Failed state
        helper.record_attempt();
        helper.record_failure();
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);

        // Without auto-recovery, check should return false
        assert!(!helper.check_auto_recovery());
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);
    }

    #[test]
    fn test_auto_recovery_cooldown_not_elapsed() {
        let policy = ReconnectPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy).with_auto_recovery(AutoRecoveryPolicy {
            cooldown: Duration::from_secs(300), // 5 minutes
            max_recovery_rounds: 3,
        });

        // Push to Failed state
        helper.record_attempt();
        helper.record_failure();
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);

        // Cooldown hasn't elapsed yet
        assert!(!helper.check_auto_recovery());
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);

        // Should have remaining cooldown
        let remaining = helper.recovery_cooldown_remaining();
        assert!(remaining.is_some());
        assert!(remaining.unwrap() > Duration::from_secs(299));
    }

    #[test]
    fn test_auto_recovery_triggers_after_cooldown() {
        let policy = ReconnectPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy).with_auto_recovery(AutoRecoveryPolicy {
            cooldown: Duration::from_millis(1), // Very short for testing
            max_recovery_rounds: 3,
        });

        // Push to Failed state
        helper.record_attempt();
        helper.record_failure();
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);
        assert!(helper.failed_at.is_some());

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(5));

        // Should trigger recovery
        assert!(helper.check_auto_recovery());
        assert_eq!(
            helper.context.connection_state,
            ReconnectState::Disconnected
        );
        assert_eq!(helper.recovery_rounds(), 1);
        assert_eq!(helper.context.current_attempt, 0);
    }

    #[test]
    fn test_auto_recovery_exhausts_rounds() {
        let policy = ReconnectPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy).with_auto_recovery(AutoRecoveryPolicy {
            cooldown: Duration::from_millis(1),
            max_recovery_rounds: 2,
        });

        for round in 1..=2 {
            // Push to Failed
            helper.record_attempt();
            helper.record_failure();
            assert_eq!(helper.context.connection_state, ReconnectState::Failed);

            std::thread::sleep(Duration::from_millis(5));

            // Recovery should trigger
            assert!(helper.check_auto_recovery());
            assert_eq!(helper.recovery_rounds(), round);
        }

        // Push to Failed one more time
        helper.record_attempt();
        helper.record_failure();
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);

        std::thread::sleep(Duration::from_millis(5));

        // Now recovery should NOT trigger (rounds exhausted)
        assert!(!helper.check_auto_recovery());
        assert_eq!(helper.context.connection_state, ReconnectState::Failed);
        assert!(helper.recovery_cooldown_remaining().is_none());
    }

    #[test]
    fn test_auto_recovery_resets_on_connected() {
        let policy = ReconnectPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: false,
        };

        let mut helper = ReconnectHelper::new(policy).with_auto_recovery(AutoRecoveryPolicy {
            cooldown: Duration::from_millis(1),
            max_recovery_rounds: 2,
        });

        // Use one recovery round
        helper.record_attempt();
        helper.record_failure();
        std::thread::sleep(Duration::from_millis(5));
        assert!(helper.check_auto_recovery());
        assert_eq!(helper.recovery_rounds(), 1);

        // Now mark connected — should reset recovery state
        helper.mark_connected();
        assert_eq!(helper.recovery_rounds(), 0);
        assert!(helper.failed_at.is_none());
    }

    #[test]
    fn test_auto_recovery_zero_rounds_disables() {
        let policy = ReconnectPolicy::default();
        let helper = ReconnectHelper::new(policy).with_auto_recovery(AutoRecoveryPolicy {
            cooldown: Duration::from_secs(1),
            max_recovery_rounds: 0,
        });

        // auto_recovery should be None when rounds = 0
        assert!(helper.auto_recovery.is_none());
    }
}
