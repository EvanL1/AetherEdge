//! Graceful shutdown utilities
//!
//! Provides unified shutdown signal handling for all services.

use tracing::warn;

const HTTP_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
/// Every exit path, including bind/listen/server errors, cancels `shutdown` for
/// the service's background tasks and stops the logging maintenance tasks.
/// Graceful shutdown gives HTTP connections a bounded drain window.
#[cfg(feature = "axum")]
pub async fn serve_with_shutdown(
    addr: std::net::SocketAddr,
    app: axum::Router,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let result = match bind_http_listener(addr) {
        Ok(listener) => serve_bound_listener(listener, addr, app, shutdown.clone()).await,
        Err(error) => Err(anyhow::Error::from(error)),
    };

    shutdown.cancel();
    crate::logging::shutdown_logging_tasks().await;
    result
}

/// Binds the HTTP socket without starting request processing. Composition roots
/// use this before activating background work so an occupied port cannot be
/// discovered only after control loops have already produced side effects.
#[cfg(feature = "axum")]
pub fn bind_http_listener(addr: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let socket = if addr.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(1024)
}

/// Serves an already-bound listener until shutdown.
#[cfg(feature = "axum")]
async fn serve_bound_listener(
    listener: tokio::net::TcpListener,
    addr: std::net::SocketAddr,
    app: axum::Router,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    tracing::info!("Listening on {}", addr);

    let graceful_shutdown = shutdown.clone();
    let drain_signal = shutdown.clone();
    let mut server = std::pin::pin!(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = wait_for_shutdown() => {
                        tracing::info!("Shutdown signal received");
                        graceful_shutdown.cancel();
                    },
                    _ = graceful_shutdown.cancelled() => {
                        tracing::warn!("Internal service shutdown requested");
                    },
                }
            })
            .await
    });

    tokio::select! {
        result = &mut server => result.map_err(anyhow::Error::from),
        _ = drain_signal.cancelled() => {
            match tokio::time::timeout(HTTP_DRAIN_TIMEOUT, &mut server).await {
                Ok(result) => result.map_err(anyhow::Error::from),
                Err(_) => {
                    tracing::warn!(
                        timeout_ms = HTTP_DRAIN_TIMEOUT.as_millis(),
                        "HTTP connections exceeded the graceful-drain deadline; closing them"
                    );
                    Ok(())
                },
            }
        },
    }
}

/// Applies the same cancellation and logging cleanup contract when the caller
/// reserved its listener before activating background work.
#[cfg(feature = "axum")]
pub async fn serve_prebound_with_shutdown(
    listener: tokio::net::TcpListener,
    addr: std::net::SocketAddr,
    app: axum::Router,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let result = serve_bound_listener(listener, addr, app, shutdown.clone()).await;
    shutdown.cancel();
    crate::logging::shutdown_logging_tasks().await;
    result
}

#[cfg(all(test, feature = "axum"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_failure_still_cancels_service_shutdown() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy local port");
        let addr = occupied.local_addr().expect("occupied address");
        let shutdown = tokio_util::sync::CancellationToken::new();

        let result = serve_with_shutdown(addr, axum::Router::new(), shutdown.clone()).await;

        assert!(result.is_err());
        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn listener_can_be_reserved_before_background_work_starts() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve probe port");
        let addr = probe.local_addr().expect("probe address");
        drop(probe);

        let listener = bind_http_listener(addr).expect("pre-bind listener");
        assert!(
            bind_http_listener(addr).is_err(),
            "a second composition cannot pass startup while the first owns the port"
        );
        drop(listener);
    }
}
