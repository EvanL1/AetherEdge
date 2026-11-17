//! Periodic timers that cannot panic on a zero period.
//!
//! `tokio::time::interval` panics when its period is zero, and the release
//! profile aborts on panic, so one mis-set interval used to be enough to take
//! a service down. Runtime code builds periodic timers through this module;
//! Clippy rejects direct `tokio::time::interval` calls elsewhere.

use std::time::Duration;

use tokio::time::{Instant, Interval};

/// Shortest period a ticker runs at.
///
/// This is a last-resort backstop that keeps a zero from reaching Tokio, not
/// a working floor: a timer running every millisecond can still spin a CPU or
/// flood a bus. A caller applies its own meaningful lower bound (a poll
/// floor, a refresh floor) where the configuration is loaded.
pub const MIN_TICK_PERIOD: Duration = Duration::from_millis(1);

/// Builds a periodic timer whose first tick completes immediately.
///
/// A zero `period` is raised to [`MIN_TICK_PERIOD`] and logged, instead of
/// panicking.
#[must_use]
#[track_caller]
pub fn ticker(period: Duration) -> Interval {
    #[expect(
        clippy::disallowed_methods,
        reason = "the sanctioned call: the period was clamped to non-zero just above"
    )]
    tokio::time::interval(non_zero(period))
}

/// Builds a periodic timer whose first tick completes at `start`.
///
/// A zero `period` is raised to [`MIN_TICK_PERIOD`] and logged, instead of
/// panicking.
#[must_use]
#[track_caller]
pub fn ticker_at(start: Instant, period: Duration) -> Interval {
    #[expect(
        clippy::disallowed_methods,
        reason = "the sanctioned call: the period was clamped to non-zero just above"
    )]
    tokio::time::interval_at(start, non_zero(period))
}

#[track_caller]
fn non_zero(period: Duration) -> Duration {
    if period.is_zero() {
        tracing::warn!(
            caller = %std::panic::Location::caller(),
            floor_ms = MIN_TICK_PERIOD.as_millis() as u64,
            "a periodic timer was configured with a zero period; running it at the floor instead"
        );
        MIN_TICK_PERIOD
    } else {
        period
    }
}
