//! Local trusted clock implementations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aether_domain::TimestampMs;
use aether_ports::{Clock, PortError, PortErrorKind, PortResult};

/// Production UTC clock backed by the operating system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> PortResult<TimestampMs> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "system clock is before Unix epoch",
            )
        })?;
        let milliseconds = u64::try_from(elapsed.as_millis()).map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "system clock exceeds u64 milliseconds",
            )
        })?;
        Ok(TimestampMs::new(milliseconds))
    }
}

/// Mutable deterministic clock for tests and replay compositions.
#[derive(Debug)]
pub struct ManualClock {
    milliseconds: AtomicU64,
}

impl ManualClock {
    /// Creates a clock at an explicit timestamp.
    #[must_use]
    pub const fn new(timestamp: TimestampMs) -> Self {
        Self {
            milliseconds: AtomicU64::new(timestamp.get()),
        }
    }

    /// Advances or rewinds the clock explicitly.
    pub fn set(&self, timestamp: TimestampMs) {
        self.milliseconds.store(timestamp.get(), Ordering::Release);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> PortResult<TimestampMs> {
        Ok(TimestampMs::new(self.milliseconds.load(Ordering::Acquire)))
    }
}
