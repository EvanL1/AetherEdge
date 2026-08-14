//! Acquisition commit observer and bounded UDS PointWatch publisher.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

struct PreparedDrainTarget {
    receiver: mpsc::Receiver<PointWatchEvent>,
    socket_path: PathBuf,
}

/// PointWatch drain workers constructed without starting asynchronous work.
///
/// Composition roots can publish and validate every fallible authority before
/// calling [`Self::spawn`], preventing an initialization error from detaching
/// already-running socket workers.
pub struct PreparedPointWatchDrains {
    targets: Vec<PreparedDrainTarget>,
    dropped_count: Arc<AtomicU64>,
}

impl PreparedPointWatchDrains {
    /// Starts every prepared consumer drain under one observed task.
    #[must_use]
    pub fn spawn(self, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
        let Self {
            targets,
            dropped_count,
        } = self;
        tokio::spawn(async move {
            // JoinSet owns its children: aborting this aggregate task drops
            // the set and aborts every socket drain instead of detaching it.
            let mut drains = tokio::task::JoinSet::new();
            for target in targets {
                drains.spawn(drain_target(
                    target.receiver,
                    target.socket_path,
                    Arc::clone(&dropped_count),
                    shutdown.clone(),
                ));
            }
            while drains.join_next().await.is_some() {
                // Individual drain errors are JoinError-only because the
                // worker returns (). Continue observing every owned child.
            }
        })
    }
}

/// Non-blocking fanout from committed acquisition samples to isolated
/// consumer PointWatch sockets.
pub struct PointWatchPublisher {
    targets: Vec<PublisherTarget>,
    dropped_count: Arc<AtomicU64>,
}

impl PointWatchPublisher {
    /// Creates one target per consumer bitmap/socket and starts their bounded
    /// drain tasks. Slow or absent consumers cannot block acquisition.
    #[must_use]
    pub fn new_with_fanout(
        target_configs: Vec<(Arc<SubscriptionBitmap>, PathBuf)>,
        shutdown: CancellationToken,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let (publisher, prepared) = Self::prepare_with_fanout(target_configs);
        let task = prepared.spawn(shutdown);
        (publisher, task)
    }

    /// Builds bounded fanout queues without spawning socket drain workers.
    #[must_use]
    pub fn prepare_with_fanout(
        target_configs: Vec<(Arc<SubscriptionBitmap>, PathBuf)>,
    ) -> (Arc<Self>, PreparedPointWatchDrains) {
        let dropped_count = Arc::new(AtomicU64::new(0));
        let mut targets = Vec::with_capacity(target_configs.len());
        let mut drains = Vec::with_capacity(target_configs.len());
        for (subscriptions, socket_path) in target_configs {
            let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
            targets.push(PublisherTarget {
                subscriptions,
                sender,
            });
            drains.push(PreparedDrainTarget {
                receiver,
                socket_path,
            });
        }
        let publisher = Arc::new(Self {
            targets,
            dropped_count: Arc::clone(&dropped_count),
        });
        (
            publisher,
            PreparedPointWatchDrains {
                targets: drains,
                dropped_count,
            },
        )
    }

    /// Returns the number of hints dropped before complete UDS delivery.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }
}

impl AcquisitionCommitObserver for PointWatchPublisher {
    fn point_committed(&self, slot: usize, sample: AcquiredPointSample) {
        let mut event = None;
        for target in &self.targets {
            if !target.subscriptions.is_watched(slot) {
                continue;
            }
            let hint = match event {
                Some(event) => event,
                None => {
                    let address = sample.address();
                    let Ok(created) = PointWatchEvent::new(
                        address.channel_id().get(),
                        address.kind(),
                        address.point_id().get(),
                        slot,
                    ) else {
                        self.dropped_count.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    event = Some(created);
                    created
                },
            };
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
