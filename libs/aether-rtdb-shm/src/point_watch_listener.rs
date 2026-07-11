//! PointWatch listener — automation side
//!
//! Binds `/tmp/aether-point-watch-automation.sock` by default, accepts connections
//! from io's drain task, reads 56-byte `PointWatchEvent` frames, and
//! forwards them to the rule dispatcher via an mpsc channel.
//!
//! ## Architecture mirror
//!
//! Mirrors `ShmCommandListener` in io but for the opposite direction
//! (io → automation). One listener, one connection at a time (io is a
//! single process).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::point_watch_event::PointWatchEvent;

/// Default UDS path (matches `AUTOMATION_POINT_WATCH_UDS_PATH` in point_watch.rs).
pub const AUTOMATION_POINT_WATCH_UDS_PATH: &str = "/tmp/aether-point-watch-automation.sock";

/// Bounded channel capacity from listener to dispatcher.
const LISTENER_CHANNEL_CAPACITY: usize = 1024;

/// automation-side PointWatch listener.
///
/// Binds the socket, accepts connections, and delivers `PointWatchEvent`s to
/// the `event_tx` channel which a `PointWatchDispatcher` (or `RuleScheduler`)
/// drains.
pub struct PointWatchListener {
    socket_path: String,
    event_tx: mpsc::Sender<PointWatchEvent>,
    shutdown: watch::Receiver<bool>,
    dropped_count: Arc<AtomicU64>,
}

impl PointWatchListener {
    /// Create a new listener.
    ///
    /// Returns the listener and the `mpsc::Receiver` that delivers events
    /// to downstream consumers (typically `PointWatchDispatcher`).
    pub fn new(
        socket_path: Option<&str>,
        shutdown: watch::Receiver<bool>,
    ) -> (Self, mpsc::Receiver<PointWatchEvent>) {
        let path = socket_path
            .unwrap_or(AUTOMATION_POINT_WATCH_UDS_PATH)
            .to_string();
        let (tx, rx) = mpsc::channel(LISTENER_CHANNEL_CAPACITY);
        let this = Self {
            socket_path: path,
            event_tx: tx,
            shutdown,
            dropped_count: Arc::new(AtomicU64::new(0)),
        };
        (this, rx)
    }

    /// Number of events dropped due to channel backpressure.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Run the listener loop (blocks until shutdown).
    ///
    /// Cleans up stale socket files from previous runs, binds, and enters the
    /// accept loop. Each connection is handled in a spawned task.
    pub async fn run(self) -> std::io::Result<()> {
        let socket_path = std::path::Path::new(&self.socket_path);

        // Clean up stale socket (same pattern as ShmCommandListener)
        if socket_path.exists() {
            if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
                error!(
                    "PointWatchListener: another listener is active on {}",
                    self.socket_path
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("another listener active on {}", self.socket_path),
                ));
            }
            info!(
                "PointWatchListener: removing stale socket {}",
                self.socket_path
            );
            tokio::fs::remove_file(socket_path).await.ok();
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(|e| {
            error!(
                "PointWatchListener: bind {} failed: {}",
                self.socket_path, e
            );
            e
        })?;
        info!("PointWatchListener bound on {}", self.socket_path);

        let mut shutdown = self.shutdown.clone();

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            debug!("PointWatchListener: new connection");
                            let tx = self.event_tx.clone();
                            let drop_ctr = Arc::clone(&self.dropped_count);
                            let sd = self.shutdown.clone();
                            tokio::spawn(async move {
                                handle_connection(stream, tx, drop_ctr, sd).await;
                            });
                        }
                        Err(e) => {
                            warn!("PointWatchListener: accept error: {}", e);
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("PointWatchListener: shutdown signal received");
                        break;
                    }
                }
            }
        }

        tokio::fs::remove_file(&self.socket_path).await.ok();
        Ok(())
    }
}

async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    tx: mpsc::Sender<PointWatchEvent>,
    dropped_count: Arc<AtomicU64>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut buf = [0u8; PointWatchEvent::SIZE];
    loop {
        tokio::select! {
            result = stream.read_exact(&mut buf) => {
                match result {
                    Ok(_) => {
                        let ev = PointWatchEvent::from_bytes(&buf);
                        debug!(
                            "PointWatchListener: ch={} pt={} val={}",
                            ev.channel_id, ev.point_id, ev.value()
                        );
                        if tx.try_send(ev).is_err() {
                            dropped_count.fetch_add(1, Ordering::Relaxed);
                            warn!(
                                "PointWatchListener: event dropped (channel full), ch={} pt={}",
                                ev.channel_id, ev.point_id
                            );
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        debug!("PointWatchListener: connection closed");
                        break;
                    }
                    Err(e) => {
                        warn!("PointWatchListener: read error: {}", e);
                        break;
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::point_watch_event::PointWatchEvent;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    /// Spawn a listener, write N events via raw UDS, assert N received.
    #[tokio::test]
    async fn listener_receives_events() {
        let sock = format!("/tmp/test-pw-listener-{}.sock", std::process::id());
        let _ = std::fs::remove_file(&sock);

        let (sd_tx, sd_rx) = watch::channel(false);
        let (listener, mut rx) = PointWatchListener::new(Some(&sock), sd_rx);

        // Run listener in background
        let sock2 = sock.clone();
        let listener_task = tokio::spawn(async move {
            listener.run().await.ok();
        });

        // Give it a moment to bind
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect as "io"
        let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();

        let ev = PointWatchEvent {
            channel_id: 42,
            point_id: 7,
            point_type: 0,
            _padding: [0; 7],
            value_bits: 1.5f64.to_bits(),
            raw_bits: 15.0f64.to_bits(),
            slot_index: 10,
            timestamp_ms: 999,
            producer_id: 1,
        };

        // Send 3 events
        for _ in 0..3 {
            client.write_all(&ev.to_bytes()).await.unwrap();
        }

        // Receive them
        for _ in 0..3 {
            let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("timeout")
                .expect("channel closed");
            assert_eq!(received.channel_id, 42);
            assert_eq!(received.point_id, 7);
        }

        // Shutdown
        sd_tx.send(true).unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(500), listener_task).await;
        let _ = std::fs::remove_file(&sock2);
    }
}
