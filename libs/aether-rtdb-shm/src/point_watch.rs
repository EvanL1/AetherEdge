//! PointWatch signaler — io side
//!
//! `PointWatchSignaler` lives in the io hot path. After every
//! `UnifiedWriter::set_direct` call it checks the subscription bitmap; if the
//! slot is subscribed it pushes a `PointWatchEvent` onto a bounded `mpsc`
//! channel. One background drain per consumer writes batches to that
//! consumer's isolated UDS socket. The legacy automation socket defaults to
//! `/tmp/aether-point-watch-automation.sock`; peripheral sockets are derived beside SHM.
//!
//! ## Design properties
//!
//! * **Non-blocking hot path**: bitmap miss → 1–2 ns (Relaxed load + branch).
//! * **Backpressure via drop**: if the bounded channel is full, the event is
//!   silently dropped and `dropped_count` incremented.
//! * **Reconnect**: drain task mirrors `ShmNotifier`'s exponential backoff
//!   (1 s → 5 s) on UDS write failure.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::point_watch_event::PointWatchEvent;
use crate::reverse_index::ReverseSlotIndex;
use crate::subscription_bitmap::SubscriptionBitmap;

/// Default UDS socket path for PointWatch (io connects → automation listens).
pub const AUTOMATION_POINT_WATCH_UDS_PATH: &str = "/tmp/aether-point-watch-automation.sock";

/// Bounded channel capacity between hot path and drain task.
const DRAIN_CHANNEL_CAPACITY: usize = 2048;

/// Maximum events per UDS write batch (limits single syscall size).
const MAX_BATCH: usize = 64;

/// Minimum backoff on UDS reconnect (ms).
const MIN_BACKOFF_MS: u64 = 1_000;

/// Maximum backoff on UDS reconnect (ms).
const MAX_BACKOFF_MS: u64 = 5_000;

/// io-side PointWatch signaler.
///
/// Created once at io startup and attached to the `UnifiedWriter`.
/// `emit` is called from the hot path after every T/S slot write.
pub struct PointWatchSignaler {
    reverse_index: Arc<ReverseSlotIndex>,
    targets: Vec<SignalerTarget>,
    dropped_count: Arc<AtomicU64>,
    producer_id: u64,
}

struct SignalerTarget {
    subscriptions: Arc<SubscriptionBitmap>,
    tx: mpsc::Sender<PointWatchEvent>,
}

impl PointWatchSignaler {
    /// Create the signaler and spawn the drain task.
    ///
    /// Returns the signaler (to be attached to `UnifiedWriter`) and the
    /// drain task join handle (await on shutdown).
    pub fn new_with_drain(
        subs: Arc<SubscriptionBitmap>,
        reverse_index: Arc<ReverseSlotIndex>,
        socket_path: String,
        shutdown: CancellationToken,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        Self::new_with_fanout(vec![(subs, socket_path)], reverse_index, shutdown)
    }

    /// Creates one hot-path signaler with independently filtered UDS targets.
    ///
    /// Every consumer owns a separate subscription bitmap and socket. A slow
    /// or absent consumer therefore cannot steal events or overwrite another
    /// consumer's subscription set.
    pub fn new_with_fanout(
        target_configs: Vec<(Arc<SubscriptionBitmap>, String)>,
        reverse_index: Arc<ReverseSlotIndex>,
        shutdown: CancellationToken,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let dropped_count = Arc::new(AtomicU64::new(0));
        let producer_id = Self::new_producer_id();
        let mut targets = Vec::with_capacity(target_configs.len());
        let mut drain_handles = Vec::with_capacity(target_configs.len());

        for (subscriptions, socket_path) in target_configs {
            let (tx, rx) = mpsc::channel(DRAIN_CHANNEL_CAPACITY);
            targets.push(SignalerTarget { subscriptions, tx });
            drain_handles.push(tokio::spawn(drain_task(
                rx,
                socket_path,
                Arc::clone(&dropped_count),
                shutdown.clone(),
            )));
        }

        let signaler = Arc::new(Self {
            reverse_index,
            targets,
            dropped_count: Arc::clone(&dropped_count),
            producer_id,
        });

        let handle = tokio::spawn(async move {
            for drain_handle in drain_handles {
                if let Err(error) = drain_handle.await {
                    warn!("PointWatch fanout drain task failed: {error}");
                }
            }
        });

        (signaler, handle)
    }

    /// Create a no-op signaler for tests (no drain task, no UDS).
    #[cfg(test)]
    pub fn new_for_test(
        subs: Arc<SubscriptionBitmap>,
        reverse_index: Arc<ReverseSlotIndex>,
        tx: mpsc::Sender<PointWatchEvent>,
    ) -> Self {
        Self::new_for_test_targets(vec![(subs, tx)], reverse_index)
    }

    /// Creates a multi-target signaler without UDS drains for unit tests.
    #[cfg(test)]
    pub fn new_for_test_targets(
        targets: Vec<(Arc<SubscriptionBitmap>, mpsc::Sender<PointWatchEvent>)>,
        reverse_index: Arc<ReverseSlotIndex>,
    ) -> Self {
        Self {
            reverse_index,
            targets: targets
                .into_iter()
                .map(|(subscriptions, tx)| SignalerTarget { subscriptions, tx })
                .collect(),
            dropped_count: Arc::new(AtomicU64::new(0)),
            producer_id: 0xDEAD_CAFE_0000_0001,
        }
    }

    /// Hot-path emit — called after `set_direct` completes the seqlock write.
    ///
    /// **Must not block.** Returns immediately whether or not the event was sent.
    #[inline]
    pub fn emit(&self, slot: usize, value: f64, raw: f64, timestamp_ms: u64) {
        let Some(origin) = self.reverse_index.get(slot) else {
            return;
        };
        self.emit_known_origin(
            slot,
            origin.channel_id,
            origin.point_type as u8,
            origin.point_id,
            value,
            raw,
            timestamp_ms,
        );
    }

    /// Emits a committed typed acquisition sample without consulting the
    /// generation-specific legacy reverse index.
    #[inline]
    pub fn emit_acquired(&self, slot: usize, sample: aether_domain::AcquiredPointSample) {
        let address = sample.address();
        let point_type = match address.kind() {
            aether_domain::PointKind::Telemetry => 0,
            aether_domain::PointKind::Status => 1,
            aether_domain::PointKind::Command => 2,
            aether_domain::PointKind::Action => 3,
        };
        self.emit_known_origin(
            slot,
            address.channel_id().get(),
            point_type,
            address.point_id().get(),
            sample.value(),
            sample.raw(),
            sample.timestamp().get(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_known_origin(
        &self,
        slot: usize,
        channel_id: u32,
        point_type: u8,
        point_id: u32,
        value: f64,
        raw: f64,
        timestamp_ms: u64,
    ) {
        let mut event = None;
        for target in &self.targets {
            if !target.subscriptions.is_watched(slot) {
                continue;
            }
            let event = match event {
                Some(event) => event,
                None => {
                    let created = PointWatchEvent {
                        channel_id,
                        point_id,
                        point_type,
                        _padding: [0; 7],
                        value_bits: value.to_bits(),
                        raw_bits: raw.to_bits(),
                        slot_index: slot as u64,
                        timestamp_ms,
                        producer_id: self.producer_id,
                    };
                    event = Some(created);
                    created
                },
            };
            if target.tx.try_send(event).is_err() {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Number of events dropped due to channel backpressure.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Producer incarnation ID (changes every io restart).
    pub fn producer_id(&self) -> u64 {
        self.producer_id
    }

    fn new_producer_id() -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        now ^ ((std::process::id() as u64) << 32)
    }
}

impl aether_shm_bridge::AcquisitionCommitObserver for PointWatchSignaler {
    fn point_committed(&self, slot: usize, sample: aether_domain::AcquiredPointSample) {
        self.emit_acquired(slot, sample);
    }
}

/// Background drain task: reads from the mpsc and writes batches to UDS.
pub(crate) async fn drain_task(
    mut rx: mpsc::Receiver<PointWatchEvent>,
    socket_path: String,
    dropped_count: Arc<AtomicU64>,
    shutdown: CancellationToken,
) {
    let mut backoff_ms: u64 = MIN_BACKOFF_MS;
    let mut last_connect_attempt: Option<Instant> = None;
    let mut reconnect_attempts: u32 = 0;

    info!("PointWatch drain task started (socket={})", socket_path);

    // Initial connection attempt (automation may not be up yet — that's OK)
    let mut stream = try_connect(&socket_path).await;
    if stream.is_some() {
        info!("PointWatch drain: connected to {}", socket_path);
    }

    loop {
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                info!("PointWatch drain task: shutdown received");
                break;
            }

            event = rx.recv() => {
                let Some(event) = event else {
                    // Channel closed — signaler dropped
                    break;
                };

                // Drain additional buffered events (batch up to MAX_BATCH)
                let mut batch = vec![event];
                while batch.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(e) => batch.push(e),
                        Err(_) => break,
                    }
                }

                // Attempt reconnect if disconnected and backoff elapsed
                if stream.is_none() {
                    let should_retry = last_connect_attempt
                        .map(|t| t.elapsed().as_millis() >= backoff_ms as u128)
                        .unwrap_or(true);

                    if should_retry {
                        stream = try_connect(&socket_path).await;
                        if stream.is_some() {
                            info!(
                                "PointWatch drain: reconnected to {} (after {} attempts)",
                                socket_path, reconnect_attempts
                            );
                            backoff_ms = MIN_BACKOFF_MS;
                            reconnect_attempts = 0;
                            last_connect_attempt = None;
                        } else {
                            reconnect_attempts += 1;
                            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                            last_connect_attempt = Some(Instant::now());
                            if reconnect_attempts.is_multiple_of(10) || reconnect_attempts <= 3 {
                                warn!(
                                    "PointWatch drain: {} reconnect attempt(s) failed (backoff={}ms)",
                                    reconnect_attempts, backoff_ms
                                );
                            }
                            // Events dropped while disconnected — count them
                            dropped_count.fetch_add(batch.len() as u64, Ordering::Relaxed);
                            continue;
                        }
                    } else {
                        // Still in backoff — drop batch
                        dropped_count.fetch_add(batch.len() as u64, Ordering::Relaxed);
                        continue;
                    }
                }

                // Send batch over UDS
                if let Some(ref mut s) = stream {
                    let mut failed = false;
                    for ev in &batch {
                        let bytes = ev.to_bytes();
                        if let Err(e) = s.write_all(&bytes).await {
                            warn!("PointWatch drain: write failed: {}", e);
                            failed = true;
                            break;
                        }
                    }
                    if failed {
                        stream = None;
                        backoff_ms = MIN_BACKOFF_MS;
                        last_connect_attempt = Some(Instant::now());
                        reconnect_attempts = 0;
                    } else {
                        debug!("PointWatch drain: sent {} event(s)", batch.len());
                    }
                }
            }
        }
    }

    info!("PointWatch drain task stopped");
}

async fn try_connect(path: &str) -> Option<UnixStream> {
    UnixStream::connect(path).await.ok()
}

// ========== Drain task delay helper (avoids sleep in the main loop) ==========

/// Yields to the tokio scheduler for `ms` milliseconds.
/// Used only during test/retry sequences outside the hot path.
#[allow(dead_code)]
async fn sleep_ms(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::reverse_index::ReverseSlotIndex;
    use crate::shared_config::ChannelToSlotIndex;
    use crate::subscription_bitmap::SubscriptionBitmap;
    use aether_model::PointType;

    fn make_reverse_index() -> Arc<ReverseSlotIndex> {
        // Slot 5 → channel 1001, Telemetry, point 0
        // Slot 6 → channel 1001, Signal, point 1
        let mut fwd = ChannelToSlotIndex::new_empty();
        fwd.insert(1001, PointType::Telemetry, 0, 5);
        fwd.insert(1001, PointType::Signal, 1, 6);
        Arc::new(ReverseSlotIndex::from_forward(&fwd, 64))
    }

    #[test]
    fn emit_hit_sends_event() {
        let (tx, mut rx) = mpsc::channel(16);
        let bm = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        bm.set_watched(5);
        let sig = PointWatchSignaler::new_for_test(Arc::clone(&bm), make_reverse_index(), tx);

        sig.emit(5, 220.0, 2200.0, 1_000);

        let ev = rx.try_recv().expect("event should be in channel");
        assert_eq!(ev.channel_id, 1001);
        assert_eq!(ev.point_id, 0);
        assert_eq!(ev.point_type, 0); // Telemetry
        assert!((ev.value() - 220.0).abs() < f64::EPSILON);
        assert!((ev.raw() - 2200.0).abs() < f64::EPSILON);
        assert_eq!(ev.slot_index, 5);
        assert_eq!(ev.timestamp_ms, 1_000);
    }

    #[test]
    fn emit_miss_sends_nothing() {
        let (tx, mut rx) = mpsc::channel(16);
        let bm = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        // slot 5 NOT subscribed
        let sig = PointWatchSignaler::new_for_test(Arc::clone(&bm), make_reverse_index(), tx);

        sig.emit(5, 220.0, 2200.0, 1_000);

        assert!(
            rx.try_recv().is_err(),
            "channel should be empty on bitmap miss"
        );
        assert_eq!(sig.dropped_count(), 0);
    }

    #[test]
    fn emit_overflow_increments_dropped() {
        let (tx, _rx) = mpsc::channel(1); // capacity = 1
        let bm = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        bm.set_watched(5);
        let sig = PointWatchSignaler::new_for_test(Arc::clone(&bm), make_reverse_index(), tx);

        sig.emit(5, 220.0, 2200.0, 1_000); // fills the channel
        sig.emit(5, 221.0, 2210.0, 2_000); // overflows → dropped

        assert_eq!(sig.dropped_count(), 1);
    }

    #[test]
    fn emit_unknown_slot_sends_nothing() {
        let (tx, mut rx) = mpsc::channel(16);
        let bm = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        bm.set_watched(99); // subscribed but no reverse-index entry
        let sig = PointWatchSignaler::new_for_test(Arc::clone(&bm), make_reverse_index(), tx);

        sig.emit(99, 0.0, 0.0, 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn emit_fanout_is_filtered_per_consumer_bitmap() {
        let (automation_tx, mut automation_rx) = mpsc::channel(16);
        let (alarm_tx, mut alarm_rx) = mpsc::channel(16);
        let automation = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        let alarm = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        automation.set_watched(5);
        alarm.set_watched(6);
        let signaler = PointWatchSignaler::new_for_test_targets(
            vec![(automation, automation_tx), (alarm, alarm_tx)],
            make_reverse_index(),
        );

        signaler.emit(5, 10.0, 10.0, 1_000);
        assert_eq!(automation_rx.try_recv().unwrap().slot_index, 5);
        assert!(alarm_rx.try_recv().is_err());

        signaler.emit(6, 20.0, 20.0, 2_000);
        assert_eq!(alarm_rx.try_recv().unwrap().slot_index, 6);
        assert!(automation_rx.try_recv().is_err());
    }

    /// Integration test: spawn a UDS listener, create a signaler with drain
    /// task, emit events, and verify they arrive.
    #[tokio::test]
    async fn drain_sends_events_over_uds() {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        let socket_path = format!("/tmp/test-pw-drain-{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path); // clean up stale

        // Bind listener (automation side)
        let listener = UnixListener::bind(&socket_path).unwrap();

        // Create bitmap + reverse index
        let bm = Arc::new(SubscriptionBitmap::new_in_memory().unwrap());
        bm.set_watched(5);
        let rev = make_reverse_index();

        let shutdown = CancellationToken::new();
        let (sig, drain_handle) = PointWatchSignaler::new_with_drain(
            Arc::clone(&bm),
            Arc::clone(&rev),
            socket_path.clone(),
            shutdown.clone(),
        );

        // Accept connection from drain task
        let accept_timeout = tokio::time::Duration::from_secs(3);
        let (mut conn, _) = tokio::time::timeout(accept_timeout, listener.accept())
            .await
            .expect("accept timed out")
            .expect("accept failed");

        // Emit one event
        sig.emit(5, 220.0, 2200.0, 42_000);

        // Read one event back
        let mut buf = [0u8; PointWatchEvent::SIZE];
        tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            conn.read_exact(&mut buf),
        )
        .await
        .expect("read timed out")
        .expect("read failed");

        let ev = PointWatchEvent::from_bytes(&buf);
        assert_eq!(ev.channel_id, 1001);
        assert!((ev.value() - 220.0).abs() < f64::EPSILON);

        // Cleanup
        shutdown.cancel();
        let _ = tokio::time::timeout(tokio::time::Duration::from_millis(500), drain_handle).await;
        let _ = std::fs::remove_file(&socket_path);
    }
}
