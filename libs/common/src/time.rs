use std::time::Duration;

use tokio::time::{Instant, Interval};

pub const MIN_TICK_PERIOD: Duration = Duration::from_millis(1);

#[must_use]
#[track_caller]
pub fn ticker(period: Duration) -> Interval {
    #[expect(
        clippy::disallowed_methods,
        reason = "the sanctioned call: the period was clamped to non-zero just above"
    )]
    tokio::time::interval(non_zero(period))
}

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
