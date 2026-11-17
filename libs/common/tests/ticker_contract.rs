//! A periodic timer must not be able to take a service down.
//!
//! `tokio::time::interval` panics on a zero period, and the release profile
//! aborts on panic. A configured interval of `0` has killed a process four
//! separate times (alarm fetch, SHM snapshot, and two CAN timers), each fixed
//! with a local clamp. These tests pin the one constructor that cannot panic.

use std::time::Duration;

use common::time::{MIN_TICK_PERIOD, ticker, ticker_at};

#[tokio::test]
async fn a_zero_period_is_clamped_instead_of_panicking() {
    let timer = ticker(Duration::ZERO);

    assert_eq!(timer.period(), MIN_TICK_PERIOD);
}

#[tokio::test]
async fn a_non_zero_period_is_used_unchanged() {
    let period = Duration::from_millis(250);

    assert_eq!(ticker(period).period(), period);
    assert_eq!(ticker(MIN_TICK_PERIOD).period(), MIN_TICK_PERIOD);
}

#[tokio::test]
async fn the_first_tick_completes_immediately_like_a_tokio_interval() {
    let mut timer = ticker(Duration::from_secs(3600));

    tokio::time::timeout(Duration::from_secs(1), timer.tick())
        .await
        .expect("the first tick of a ticker is immediate");
}

#[tokio::test]
async fn a_delayed_ticker_clamps_a_zero_period_and_waits_for_its_start() {
    let start = tokio::time::Instant::now() + Duration::from_millis(50);
    let mut timer = ticker_at(start, Duration::ZERO);

    assert_eq!(timer.period(), MIN_TICK_PERIOD);
    timer.tick().await;
    // `tick()` returns the scheduled deadline, so compare against the clock.
    assert!(
        tokio::time::Instant::now() >= start,
        "a delayed ticker must not fire before its start"
    );
}
