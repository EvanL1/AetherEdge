//! Graceful shutdown utilities
//!
//! Provides unified shutdown signal handling for all services.

use tracing::warn;

/// Wait for shutdown signal (Ctrl+C or SIGTERM on Unix)
///
/// This function blocks until a shutdown signal is received:
/// - On Unix: Ctrl+C (SIGINT) or SIGTERM
/// - On Windows: Ctrl+C only
///
/// # Example
///
/// ```ignore
/// tokio::select! {
///     _ = common::shutdown::wait_for_shutdown() => {
///         info!("Shutdown signal received");
///     }
///     // ... other tasks
/// }
/// ```
pub async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let term_signal = match signal(SignalKind::terminate()) {
            Ok(sig) => Some(sig),
            Err(e) => {
                warn!(
                    "Failed to install SIGTERM handler: {}. Service will only respond to Ctrl+C",
                    e
                );
                None
            },
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = async {
                if let Some(mut sig) = term_signal {
                    sig.recv().await;
                } else {
                    // If SIGTERM handler failed, wait forever (only Ctrl+C will work)
                    std::future::pending::<()>().await
                }
            } => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
