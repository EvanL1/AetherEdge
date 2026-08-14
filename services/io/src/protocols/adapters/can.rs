//! CAN Protocol Implementation (LYNK Protocol)
//!
//! Implements CAN bus communication for Discover LYNK Serial CAN interface.

#[cfg(any(target_os = "linux", test))]
use std::sync::Arc;
#[cfg(any(target_os = "linux", test))]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

#[cfg(any(target_os = "linux", test))]
use arc_swap::ArcSwapOption;

#[cfg(any(target_os = "linux", test))]
use crate::protocols::core::traits::{ConnectionState, DataEvent, DataEventSink};

/// Publishes a terminal background-worker failure to both the synchronous
/// runtime state and the event-driven channel task.
#[cfg(any(target_os = "linux", test))]
fn fail_worker(
    connection_state: &AtomicU8,
    is_connected: &AtomicBool,
    error_count: &AtomicU64,
    last_error: &ArcSwapOption<String>,
    event_tx: &DataEventSink,
    message: String,
) {
    last_error.store(Some(Arc::new(message.clone())));
    error_count.fetch_add(1, Ordering::Relaxed);
    is_connected.store(false, Ordering::Release);
    connection_state.store(ConnectionState::Error.into(), Ordering::Release);
    event_tx.publish(DataEvent::Error(message));
    event_tx.publish(DataEvent::ConnectionChanged(ConnectionState::Error));
}

#[cfg(target_os = "linux")]
mod client;
pub mod config;
pub mod decoder;

#[cfg(feature = "j1939")]
pub mod j1939;

// Re-export the Linux socket client and cross-platform configuration types.
#[cfg(target_os = "linux")]
pub use client::CanClient;
pub use config::{CanChannelParamsConfig, CanConfig, CanDataType, CanPoint, LynkCanId};

#[cfg(all(feature = "j1939", target_os = "linux"))]
pub use j1939::{J1939Client, J1939Config, J1939PointConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::core::traits::data_event_channel_with_capacity;

    #[tokio::test]
    async fn worker_failure_is_visible_without_a_socketcan_runtime() {
        let connection_state = AtomicU8::new(ConnectionState::Connected.into());
        let is_connected = AtomicBool::new(true);
        let error_count = AtomicU64::new(0);
        let last_error = ArcSwapOption::empty();
        let (event_tx, mut event_rx) = data_event_channel_with_capacity(1);

        fail_worker(
            &connection_state,
            &is_connected,
            &error_count,
            &last_error,
            &event_tx,
            "CAN receive worker failed".to_string(),
        );

        assert!(!is_connected.load(Ordering::Acquire));
        assert_eq!(
            ConnectionState::from(connection_state.load(Ordering::Acquire)),
            ConnectionState::Error
        );
        assert_eq!(error_count.load(Ordering::Relaxed), 1);
        assert_eq!(
            last_error.load_full().as_deref().map(String::as_str),
            Some("CAN receive worker failed")
        );
        assert!(matches!(event_rx.recv().await, Some(DataEvent::Error(_))));
        assert!(matches!(
            event_rx.recv().await,
            Some(DataEvent::ConnectionChanged(ConnectionState::Error))
        ));
    }
}
