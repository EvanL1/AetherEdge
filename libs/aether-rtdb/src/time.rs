//! Time provider abstraction for RTDB operations
//!
//! This module separates time acquisition from storage operations,
//! allowing for better testability and cleaner abstractions.

use std::time::{SystemTime, UNIX_EPOCH};

/// Time provider trait for generating timestamps
///
/// This trait abstracts time acquisition, allowing:
/// - System time for production use
/// - Fixed/mock time for testing
/// - Redis server time for distributed scenarios
pub trait TimeProvider: Send + Sync + 'static {
    /// Get current timestamp in milliseconds since Unix epoch
    fn now_millis(&self) -> i64;
}

/// System time provider using local clock
///
/// This is the default implementation suitable for most use cases.
/// For distributed systems requiring synchronized time, consider
/// using a custom implementation that queries a central time source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTimeProvider;

impl TimeProvider for SystemTimeProvider {
    fn now_millis(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(_) => {
                // Clock before UNIX epoch (NTP fault, VM snapshot, etc.) — return 0
                // rather than panicking, since panic=abort would kill the process.
                0
            },
        }
    }
}

/// Fixed time provider for testing
///
/// Returns a predetermined timestamp, useful for deterministic tests.
#[derive(Clone, Copy, Debug)]
pub struct FixedTimeProvider {
    timestamp_ms: i64,
}

impl FixedTimeProvider {
    /// Create a new fixed time provider with the given timestamp
    pub fn new(timestamp_ms: i64) -> Self {
        Self { timestamp_ms }
    }
}

impl TimeProvider for FixedTimeProvider {
    fn now_millis(&self) -> i64 {
        self.timestamp_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_time_provider() {
        let provider = SystemTimeProvider;
        let time1 = provider.now_millis();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let time2 = provider.now_millis();

        assert!(time2 >= time1);
        assert!(time2 - time1 >= 10);
    }

    #[test]
    fn test_fixed_time_provider() {
        let fixed_time = 1700000000000_i64;
        let provider = FixedTimeProvider::new(fixed_time);

        assert_eq!(provider.now_millis(), fixed_time);
        assert_eq!(provider.now_millis(), fixed_time); // Always returns same value
    }
}
