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

/// Bind `addr` with `SO_REUSEADDR` and serve `app` until shutdown.
///
/// The socket family follows `addr` so an IPv6 bind address stays usable.
/// On shutdown the graceful-shutdown future cancels `shutdown` for the
/// service's background tasks and then drains the logging writers.
#[cfg(feature = "axum")]
pub async fn serve_with_shutdown(
    addr: std::net::SocketAddr,
    app: axum::Router,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let socket = if addr.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    let listener = socket.listen(1024)?;

    tracing::info!("Listening on {}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            wait_for_shutdown().await;
            tracing::info!("Shutdown signal received");
            shutdown.cancel();
        })
        .await?;

    crate::logging::shutdown_logging_tasks().await;
    Ok(())
}
