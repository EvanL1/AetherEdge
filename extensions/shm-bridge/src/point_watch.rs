//! Acquisition commit observer and bounded UDS PointWatch publisher.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_domain::AcquiredPointSample;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{AcquisitionCommitObserver, PointWatchEvent, SubscriptionBitmap};

const CHANNEL_CAPACITY: usize = 2_048;
const MAX_BATCH: usize = 64;
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

struct PublisherTarget {
    subscriptions: Arc<SubscriptionBitmap>,
    sender: mpsc::Sender<PointWatchEvent>,
}

/// Non-blocking fanout from committed acquisition samples to isolated
/// consumer PointWatch sockets.
pub struct PointWatchPublisher {
    targets: Vec<PublisherTarget>,
    dropped_count: Arc<AtomicU64>,
    producer_id: u64,
}

impl PointWatchPublisher {
    /// Creates one target per consumer bitmap/socket and starts their bounded
    /// drain tasks. Slow or absent consumers cannot block acquisition.
    #[must_use]
    pub fn new_with_fanout(
        target_configs: Vec<(Arc<SubscriptionBitmap>, PathBuf)>,
        shutdown: CancellationToken,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let dropped_count = Arc::new(AtomicU64::new(0));
        let mut targets = Vec::with_capacity(target_configs.len());
        let mut drains = Vec::with_capacity(target_configs.len());
        for (subscriptions, socket_path) in target_configs {
            let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
            targets.push(PublisherTarget {
                subscriptions,
                sender,
            });
            drains.push(tokio::spawn(drain_target(
                receiver,
                socket_path,
                Arc::clone(&dropped_count),
                shutdown.clone(),
            )));
        }
        let publisher = Arc::new(Self {
            targets,
            dropped_count,
            producer_id: new_producer_id(),
        });
        let task = tokio::spawn(async move {
            for drain in drains {
                let _ = drain.await;
            }
        });
        (publisher, task)
    }

    /// Returns the number of hints dropped before complete UDS delivery.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Returns the IO process incarnation stamped into every event.
    #[must_use]
    pub const fn producer_id(&self) -> u64 {
        self.producer_id
    }
}

impl AcquisitionCommitObserver for PointWatchPublisher {
    fn point_committed(&self, slot: usize, sample: AcquiredPointSample) {
        let mut event = None;
        for target in &self.targets {
            if !target.subscriptions.is_watched(slot) {
                continue;
            }
            let hint = *event.get_or_insert_with(|| {
                let address = sample.address();
                PointWatchEvent::new(
                    address.channel_id().get(),
                    address.kind(),
                    address.point_id().get(),
                    slot as u64,
                    sample.value(),
                    sample.raw(),
                    sample.timestamp().get(),
                    self.producer_id,
                )
            });
            if target.sender.try_send(hint).is_err() {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

async fn drain_target(
    mut receiver: mpsc::Receiver<PointWatchEvent>,
    socket_path: PathBuf,
    dropped_count: Arc<AtomicU64>,
    shutdown: CancellationToken,
) {
    let mut stream = connect(&socket_path).await;
    let mut backoff = MIN_BACKOFF;
    let mut last_attempt = None::<Instant>;
    loop {
        let event = tokio::select! {
            _ = shutdown.cancelled() => break,
            event = receiver.recv() => event,
        };
        let Some(event) = event else {
            break;
        };
        let mut batch = Vec::with_capacity(MAX_BATCH);
        batch.push(event);
        while batch.len() < MAX_BATCH {
            match receiver.try_recv() {
                Ok(event) => batch.push(event),
                Err(_) => break,
            }
        }

        if stream.is_none() {
            let may_connect = last_attempt.is_none_or(|attempt| attempt.elapsed() >= backoff);
            if may_connect {
                last_attempt = Some(Instant::now());
                stream = connect(&socket_path).await;
                if stream.is_some() {
                    backoff = MIN_BACKOFF;
                    last_attempt = None;
                } else {
                    backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
                }
            }
        }
        let Some(active_stream) = stream.as_mut() else {
            dropped_count.fetch_add(batch.len() as u64, Ordering::Relaxed);
            continue;
        };

        let mut delivered = 0_usize;
        for event in &batch {
            let bytes = event.to_bytes();
            let result = tokio::select! {
                _ = shutdown.cancelled() => return,
                result = tokio::time::timeout(WRITE_TIMEOUT, active_stream.write_all(&bytes)) => result,
            };
            match result {
                Ok(Ok(())) => delivered += 1,
                Ok(Err(_)) | Err(_) => break,
            }
        }
        if delivered != batch.len() {
            dropped_count.fetch_add((batch.len() - delivered) as u64, Ordering::Relaxed);
            stream = None;
            last_attempt = Some(Instant::now());
            backoff = MIN_BACKOFF;
        }
    }
}

async fn connect(path: &Path) -> Option<UnixStream> {
    tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(path))
        .await
        .ok()
        .and_then(Result::ok)
}

fn new_producer_id() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    (now ^ (u64::from(std::process::id()) << 32)).max(1)
}
