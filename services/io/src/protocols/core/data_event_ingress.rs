//! Bounded, non-blocking ingress for protocol data events.
//!
//! Event-driven adapters must never await the service consumer from their
//! protocol loops.  At the same time, a plain FIFO loses the newest device
//! value when it fills.  This owner-local ingress therefore keeps independent
//! bounded lanes:
//!
//! - point updates are coalesced latest-wins by `(PointType, point_id)`;
//! - connection state and heartbeat each occupy one latest-wins slot;
//! - errors retain FIFO ordering and drop the newest error when their lane is
//!   full.
//!
//! There is no forwarding task.  The sole receiver drains the shared bounded
//! state directly, so shutdown cannot leave a detached ingress worker behind.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;
use tracing::warn;
use utoipa::ToSchema;

use aether_core::PointType;

use super::data::{DataBatch, DataPoint};
use super::traits::{ConnectionState, DataEvent};

fn sample_time(point: &DataPoint) -> &chrono::DateTime<chrono::Utc> {
    point.source_timestamp.as_ref().unwrap_or(&point.timestamp)
}

/// Maximum number of non-coalescible errors retained for one channel.
const ERROR_EVENT_CAPACITY: usize = 64;
/// Maximum error payload accepted into the bounded ingress.
const MAX_ERROR_BYTES: usize = 4096;
/// Maximum number of point updates returned in one consumer turn.
const DATA_DRAIN_BATCH_SIZE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PointKey {
    point_type: PointType,
    id: u32,
}

impl From<&DataPoint> for PointKey {
    fn from(point: &DataPoint) -> Self {
        Self {
            point_type: point.point_type,
            id: point.id,
        }
    }
}

/// Result of one non-blocking event publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataEventAdmission {
    /// The event created a new pending representation.
    Accepted,
    /// At least one value was merged into an existing pending representation;
    /// sample time determines whether that representation is replaced.
    Coalesced,
    /// The corresponding bounded lane was full; the new event was rejected.
    DroppedFull,
    /// The sole receiver was already closed.
    DroppedClosed,
    /// Another producer/consumer held the bounded state; this producer did
    /// not wait and dropped newest to protect the protocol loop.
    DroppedContended,
    /// The event exceeded its per-event point or byte limit.
    Oversized,
}

/// Snapshot of one channel's data-event ingress.
///
/// The first six counters are disjoint cumulative event-submission outcomes:
/// `accepted` excludes events counted as `coalesced`.
/// `discarded_on_close` counts bounded logical records that were still pending
/// when the receiver closed.  Current pressure is represented separately so a
/// historical burst does not permanently make readiness unhealthy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DataEventIngressStats {
    pub accepted: u64,
    pub coalesced: u64,
    pub dropped_full: u64,
    pub dropped_closed: u64,
    pub dropped_contended: u64,
    pub oversized: u64,
    pub discarded_on_close: u64,
    pub pending: u64,
    pub high_watermark: u64,
    pub capacity: u64,
    pub data_pending: u64,
    pub error_pending: u64,
    pub receiver_open: bool,
    pub saturated: bool,
}

#[derive(Default)]
struct IngressMetrics {
    accepted: AtomicU64,
    coalesced: AtomicU64,
    dropped_full: AtomicU64,
    dropped_closed: AtomicU64,
    dropped_contended: AtomicU64,
    oversized: AtomicU64,
    discarded_on_close: AtomicU64,
    pending: AtomicU64,
    high_watermark: AtomicU64,
    data_pending: AtomicU64,
    error_pending: AtomicU64,
    saturated: AtomicBool,
    data_full_at: AtomicU64,
    error_full: AtomicBool,
}

impl IngressMetrics {
    fn snapshot(&self, receiver_open: bool, capacity: usize) -> DataEventIngressStats {
        DataEventIngressStats {
            accepted: self.accepted.load(Ordering::Relaxed),
            coalesced: self.coalesced.load(Ordering::Relaxed),
            dropped_full: self.dropped_full.load(Ordering::Relaxed),
            dropped_closed: self.dropped_closed.load(Ordering::Relaxed),
            dropped_contended: self.dropped_contended.load(Ordering::Relaxed),
            oversized: self.oversized.load(Ordering::Relaxed),
            discarded_on_close: self.discarded_on_close.load(Ordering::Relaxed),
            pending: self.pending.load(Ordering::Relaxed),
            high_watermark: self.high_watermark.load(Ordering::Relaxed),
            capacity: capacity as u64,
            data_pending: self.data_pending.load(Ordering::Relaxed),
            error_pending: self.error_pending.load(Ordering::Relaxed),
            receiver_open,
            saturated: self.saturated.load(Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
struct IngressState {
    points: HashMap<PointKey, DataPoint>,
    point_order: VecDeque<PointKey>,
    errors: VecDeque<String>,
    connection: Option<ConnectionState>,
    heartbeat: bool,
    next_lane: usize,
}

impl IngressState {
    fn pending(&self) -> usize {
        self.points.len()
            + self.errors.len()
            + usize::from(self.connection.is_some())
            + usize::from(self.heartbeat)
    }

    fn clear(&mut self) -> usize {
        let pending = self.pending();
        self.points.clear();
        self.point_order.clear();
        self.errors.clear();
        self.connection = None;
        self.heartbeat = false;
        pending
    }

    fn pop_lane(&mut self, lane: usize) -> Option<DataEvent> {
        match lane {
            0 if !self.points.is_empty() => {
                let take = self.points.len().min(DATA_DRAIN_BATCH_SIZE);
                let mut points = Vec::with_capacity(take);
                for _ in 0..take {
                    let Some(key) = self.point_order.pop_front() else {
                        break;
                    };
                    if let Some(point) = self.points.remove(&key) {
                        points.push(point);
                    }
                }
                if points.is_empty() {
                    None
                } else {
                    Some(DataEvent::DataUpdate(DataBatch::from_points(points)))
                }
            },
            1 => self.connection.take().map(DataEvent::ConnectionChanged),
            2 => self.errors.pop_front().map(DataEvent::Error),
            3 if self.heartbeat => {
                self.heartbeat = false;
                Some(DataEvent::Heartbeat)
            },
            _ => None,
        }
    }

    fn pop_fair(&mut self) -> Option<DataEvent> {
        for offset in 0..4 {
            let lane = (self.next_lane + offset) % 4;
            if let Some(event) = self.pop_lane(lane) {
                self.next_lane = (lane + 1) % 4;
                return Some(event);
            }
        }
        None
    }
}

struct SharedIngress {
    state: Mutex<IngressState>,
    notify: Notify,
    metrics: IngressMetrics,
    sender_count: AtomicUsize,
    receiver_open: AtomicBool,
    point_capacity: usize,
    #[cfg(test)]
    recv_wait_hook: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
}

impl SharedIngress {
    fn lock_state(&self) -> MutexGuard<'_, IngressState> {
        // Recover the bounded state after an unrelated task panic.  Ingress
        // methods themselves contain no user callbacks or fallible indexing.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn try_lock_state(&self) -> Option<MutexGuard<'_, IngressState>> {
        match self.state.try_lock() {
            Ok(state) => Some(state),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    fn update_pressure(&self, state: &IngressState) {
        let pending = state.pending() as u64;
        self.metrics.pending.store(pending, Ordering::Relaxed);
        self.metrics
            .data_pending
            .store(state.points.len() as u64, Ordering::Relaxed);
        self.metrics
            .error_pending
            .store(state.errors.len() as u64, Ordering::Relaxed);
        // A safely coalescible lane at capacity is normal. Saturation begins
        // only with an actual rejection and recovers only when pressure in the
        // rejected lane decreases (unrelated control traffic cannot mask it).
        let data_full_at = self.metrics.data_full_at.load(Ordering::Relaxed);
        if data_full_at > 0 && state.points.len() < data_full_at as usize {
            self.metrics.data_full_at.store(0, Ordering::Relaxed);
        }
        if self.metrics.error_full.load(Ordering::Relaxed)
            && state.errors.len() < ERROR_EVENT_CAPACITY
        {
            self.metrics.error_full.store(false, Ordering::Relaxed);
        }
        self.metrics.saturated.store(
            self.metrics.data_full_at.load(Ordering::Relaxed) > 0
                || self.metrics.error_full.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.metrics
            .high_watermark
            .fetch_max(pending, Ordering::Relaxed);
    }

    fn record_drop(&self, admission: DataEventAdmission) {
        let count = match admission {
            DataEventAdmission::DroppedFull => {
                self.metrics.dropped_full.fetch_add(1, Ordering::Relaxed) + 1
            },
            DataEventAdmission::DroppedClosed => {
                self.metrics.dropped_closed.fetch_add(1, Ordering::Relaxed) + 1
            },
            DataEventAdmission::DroppedContended => {
                self.metrics
                    .dropped_contended
                    .fetch_add(1, Ordering::Relaxed)
                    + 1
            },
            DataEventAdmission::Oversized => {
                self.metrics.oversized.fetch_add(1, Ordering::Relaxed) + 1
            },
            DataEventAdmission::Accepted | DataEventAdmission::Coalesced => return,
        };

        // A counter is the durable signal.  Power-of-two sampling makes drops
        // visible in logs without allowing a bad peer to create a log storm.
        if count.is_power_of_two() {
            warn!(
                ?admission,
                count, "protocol data-event ingress rejected an event"
            );
        }
    }
}

/// Cloneable, non-blocking producer handle used by adapter tasks.
pub struct DataEventSink {
    shared: Arc<SharedIngress>,
}

impl Clone for DataEventSink {
    fn clone(&self) -> Self {
        self.shared.sender_count.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for DataEventSink {
    fn drop(&mut self) {
        if self.shared.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            // `notify_one` stores a permit if recv() has created but not yet
            // polled its `Notified` future. `notify_waiters` would lose that
            // final-sender transition in the check-to-await window.
            self.shared.notify.notify_one();
        }
    }
}

impl std::fmt::Debug for DataEventSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DataEventSink")
            .field("stats", &self.stats())
            .finish()
    }
}

impl DataEventSink {
    /// Publish one event without awaiting the consumer or blocking on I/O.
    pub fn publish(&self, event: DataEvent) -> DataEventAdmission {
        if !self.shared.receiver_open.load(Ordering::Acquire) {
            self.shared.record_drop(DataEventAdmission::DroppedClosed);
            return DataEventAdmission::DroppedClosed;
        }

        let (event, duplicate_in_batch) = match event {
            DataEvent::DataUpdate(batch) => {
                let initial_capacity = batch.len().min(self.shared.point_capacity);
                let mut deduplicated = HashMap::with_capacity(initial_capacity);
                let mut order = Vec::with_capacity(initial_capacity);
                let mut duplicate = false;
                for point in batch {
                    let key = PointKey::from(&point);
                    match deduplicated.entry(key) {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            if order.len() == self.shared.point_capacity {
                                self.shared.record_drop(DataEventAdmission::Oversized);
                                return DataEventAdmission::Oversized;
                            }
                            entry.insert(point);
                            order.push(key);
                        },
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            duplicate = true;
                            // Late/out-of-order packets must not overwrite a
                            // newer sample already present in this batch.
                            if sample_time(&point) >= sample_time(entry.get()) {
                                entry.insert(point);
                            }
                        },
                    }
                }
                (
                    PreparedEvent::Data {
                        deduplicated,
                        order,
                    },
                    duplicate,
                )
            },
            DataEvent::Error(message) if message.len() > MAX_ERROR_BYTES => {
                self.shared.record_drop(DataEventAdmission::Oversized);
                return DataEventAdmission::Oversized;
            },
            DataEvent::Error(message) => (PreparedEvent::Error(message), false),
            DataEvent::ConnectionChanged(state) => (PreparedEvent::Connection(state), false),
            DataEvent::Heartbeat => (PreparedEvent::Heartbeat, false),
        };

        let Some(mut state) = self.shared.try_lock_state() else {
            self.shared
                .record_drop(DataEventAdmission::DroppedContended);
            return DataEventAdmission::DroppedContended;
        };
        if !self.shared.receiver_open.load(Ordering::Acquire) {
            drop(state);
            self.shared.record_drop(DataEventAdmission::DroppedClosed);
            return DataEventAdmission::DroppedClosed;
        }

        let mut coalesced = duplicate_in_batch;
        let was_empty = state.pending() == 0;
        let first_lane = event.lane();
        match event {
            PreparedEvent::Data {
                mut deduplicated,
                order,
            } => {
                let new_keys = order
                    .iter()
                    .filter(|key| !state.points.contains_key(key))
                    .count();
                if new_keys
                    > self
                        .shared
                        .point_capacity
                        .saturating_sub(state.points.len())
                {
                    self.shared
                        .metrics
                        .data_full_at
                        .store(state.points.len().max(1) as u64, Ordering::Relaxed);
                    self.shared.metrics.saturated.store(true, Ordering::Relaxed);
                    self.shared.update_pressure(&state);
                    drop(state);
                    self.shared.record_drop(DataEventAdmission::DroppedFull);
                    return DataEventAdmission::DroppedFull;
                }

                for key in order {
                    let Some(point) = deduplicated.remove(&key) else {
                        continue;
                    };
                    if let Some(pending) = state.points.get_mut(&key) {
                        coalesced = true;
                        // Prefer device source time when present, then the
                        // gateway receive time. Equal timestamps retain
                        // normal latest-arrival-wins behavior.
                        if sample_time(&point) >= sample_time(pending) {
                            *pending = point;
                        }
                    } else {
                        state.points.insert(key, point);
                        state.point_order.push_back(key);
                    }
                }
            },
            PreparedEvent::Error(message) => {
                if state.errors.len() == ERROR_EVENT_CAPACITY {
                    self.shared
                        .metrics
                        .error_full
                        .store(true, Ordering::Relaxed);
                    self.shared.metrics.saturated.store(true, Ordering::Relaxed);
                    self.shared.update_pressure(&state);
                    drop(state);
                    self.shared.record_drop(DataEventAdmission::DroppedFull);
                    return DataEventAdmission::DroppedFull;
                }
                state.errors.push_back(message);
            },
            PreparedEvent::Connection(connection) => {
                coalesced = state.connection.replace(connection).is_some();
            },
            PreparedEvent::Heartbeat => {
                coalesced = state.heartbeat;
                state.heartbeat = true;
            },
        }
        if was_empty && state.pending() > 0 {
            // Preserve the causal order of the first event in a new burst;
            // subsequent consumer turns rotate across lanes for fairness.
            state.next_lane = first_lane;
        }

        let admission = if coalesced {
            self.shared
                .metrics
                .coalesced
                .fetch_add(1, Ordering::Relaxed);
            DataEventAdmission::Coalesced
        } else {
            self.shared.metrics.accepted.fetch_add(1, Ordering::Relaxed);
            DataEventAdmission::Accepted
        };
        self.shared.update_pressure(&state);
        drop(state);
        self.shared.notify.notify_one();
        admission
    }

    pub fn stats(&self) -> DataEventIngressStats {
        self.shared.metrics.snapshot(
            self.shared.receiver_open.load(Ordering::Acquire),
            self.shared
                .point_capacity
                .saturating_add(ERROR_EVENT_CAPACITY + 2),
        )
    }
}

enum PreparedEvent {
    Data {
        deduplicated: HashMap<PointKey, DataPoint>,
        order: Vec<PointKey>,
    },
    Error(String),
    Connection(ConnectionState),
    Heartbeat,
}

impl PreparedEvent {
    const fn lane(&self) -> usize {
        match self {
            Self::Data { .. } => 0,
            Self::Connection(_) => 1,
            Self::Error(_) => 2,
            Self::Heartbeat => 3,
        }
    }
}

/// Read-only metrics handle retained by channel status after the receiver exits.
#[derive(Clone)]
pub struct DataEventIngressObserver {
    shared: Arc<SharedIngress>,
}

impl DataEventIngressObserver {
    pub fn stats(&self) -> DataEventIngressStats {
        self.shared.metrics.snapshot(
            self.shared.receiver_open.load(Ordering::Acquire),
            self.shared
                .point_capacity
                .saturating_add(ERROR_EVENT_CAPACITY + 2),
        )
    }
}

impl std::fmt::Debug for DataEventIngressObserver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DataEventIngressObserver")
            .field("stats", &self.stats())
            .finish()
    }
}

/// Sole event consumer owned by the unified channel task.
pub struct DataEventReceiver {
    shared: Arc<SharedIngress>,
}

impl std::fmt::Debug for DataEventReceiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DataEventReceiver")
            .field("stats", &self.stats())
            .finish()
    }
}

impl DataEventReceiver {
    pub async fn recv(&mut self) -> Option<DataEvent> {
        loop {
            let notified = self.shared.notify.notified();
            {
                let mut state = self.shared.lock_state();
                if let Some(event) = state.pop_fair() {
                    self.shared.update_pressure(&state);
                    return Some(event);
                }
                if !self.shared.receiver_open.load(Ordering::Acquire)
                    || self.shared.sender_count.load(Ordering::Acquire) == 0
                {
                    return None;
                }
            }
            #[cfg(test)]
            if let Some((reached, release)) = self
                .shared
                .recv_wait_hook
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                reached.wait();
                release.wait();
            }
            notified.await;
        }
    }

    pub fn observer(&self) -> DataEventIngressObserver {
        DataEventIngressObserver {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn stats(&self) -> DataEventIngressStats {
        self.observer().stats()
    }

    /// Stop new admission, drain at most `event_limit` fair consumer events,
    /// and synchronously discard the bounded remainder.
    ///
    /// Each data event contains at most [`DATA_DRAIN_BATCH_SIZE`] points. The
    /// caller therefore controls the shutdown processing bound and no
    /// forwarding task survives this call.
    pub fn close_and_drain(&mut self, event_limit: usize) -> (Vec<DataEvent>, usize) {
        if !self.shared.receiver_open.swap(false, Ordering::AcqRel) {
            return (Vec::new(), 0);
        }

        let mut state = self.shared.lock_state();
        let mut drained = Vec::with_capacity(event_limit.min(state.pending()));
        for _ in 0..event_limit {
            let Some(event) = state.pop_fair() else {
                break;
            };
            drained.push(event);
        }
        let discarded = state.clear();
        self.shared
            .metrics
            .discarded_on_close
            .fetch_add(discarded as u64, Ordering::Relaxed);
        self.shared.update_pressure(&state);
        drop(state);
        self.shared.notify.notify_waiters();
        (drained, discarded)
    }

    #[cfg(test)]
    fn install_wait_hook(
        &self,
        reached: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        *self
            .shared
            .recv_wait_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((reached, release));
    }

    /// Stop admission and synchronously discard the bounded remainder.
    ///
    /// The return value is the number of logical records discarded.  Work is
    /// bounded by the ingress capacities and no task or thread is detached.
    pub fn close(&mut self) -> usize {
        self.close_and_drain(0).1
    }
}

impl Drop for DataEventReceiver {
    fn drop(&mut self) {
        self.close();
    }
}

/// Construct an ingress sized from a runtime's validated acquisition points.
///
/// Zero-point event runtimes retain one defensive point slot. Configured
/// runtimes use the actual validated point count, including deployments whose
/// `shared_memory.max_slots` exceeds the default.
pub fn data_event_channel_with_capacity(
    configured_point_count: usize,
) -> (DataEventSink, DataEventReceiver) {
    let point_capacity = configured_point_count.max(1);
    let shared = Arc::new(SharedIngress {
        state: Mutex::new(IngressState::default()),
        notify: Notify::new(),
        metrics: IngressMetrics::default(),
        sender_count: AtomicUsize::new(1),
        receiver_open: AtomicBool::new(true),
        point_capacity,
        #[cfg(test)]
        recv_wait_hook: Mutex::new(None),
    });
    (
        DataEventSink {
            shared: Arc::clone(&shared),
        },
        DataEventReceiver { shared },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::core::data::Value;
    use chrono::{Duration as ChronoDuration, Utc};

    fn point(id: u32, value: i64) -> DataPoint {
        DataPoint::telemetry(id, value)
    }

    #[tokio::test]
    async fn burst_keeps_the_latest_value_for_each_stable_point_key() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        for value in 0..10_000 {
            assert!(matches!(
                sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![point(
                    7, value
                )]))),
                DataEventAdmission::Accepted | DataEventAdmission::Coalesced
            ));
        }

        let DataEvent::DataUpdate(batch) = receiver.recv().await.expect("latest value") else {
            panic!("expected data update");
        };
        let latest = batch.iter().next().expect("one point");
        assert_eq!(latest.value, Value::Integer(9_999));
        let stats = receiver.stats();
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.coalesced, 9_999);
        assert_eq!(stats.high_watermark, 1);
    }

    #[tokio::test]
    async fn late_older_samples_do_not_replace_a_newer_pending_value() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        let now = Utc::now();
        let mut newest = point(7, 2);
        newest.timestamp = now;
        newest.source_timestamp = Some(now + ChronoDuration::seconds(10));
        let mut late = point(7, 1);
        late.timestamp = now + ChronoDuration::seconds(20);
        late.source_timestamp = Some(now + ChronoDuration::seconds(5));

        assert_eq!(
            sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![newest]))),
            DataEventAdmission::Accepted
        );
        assert_eq!(
            sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![late]))),
            DataEventAdmission::Coalesced
        );

        let Some(DataEvent::DataUpdate(batch)) = receiver.recv().await else {
            panic!("expected data update");
        };
        assert_eq!(
            batch.iter().next().map(|point| point.value),
            Some(Value::Integer(2))
        );
    }

    #[tokio::test]
    async fn full_data_lane_does_not_starve_control_lanes() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1024);
        for id in 0..1024 {
            assert_eq!(
                sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![point(
                    id, 1
                )]))),
                DataEventAdmission::Accepted
            );
        }
        assert!(!receiver.stats().saturated);
        assert_eq!(
            sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![point(
                1024, 1,
            )]))),
            DataEventAdmission::DroppedFull
        );
        assert!(receiver.stats().saturated);
        assert_eq!(
            sink.publish(DataEvent::ConnectionChanged(ConnectionState::Connected)),
            DataEventAdmission::Accepted
        );
        assert_eq!(
            sink.publish(DataEvent::Error("visible".to_string())),
            DataEventAdmission::Accepted
        );
        assert_eq!(
            sink.publish(DataEvent::Heartbeat),
            DataEventAdmission::Accepted
        );

        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::DataUpdate(_))
        ));
        assert!(!receiver.stats().saturated);
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::ConnectionChanged(ConnectionState::Connected))
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::Error(message)) if message == "visible"
        ));
        assert!(matches!(receiver.recv().await, Some(DataEvent::Heartbeat)));
    }

    #[tokio::test]
    async fn a_full_mixed_batch_is_rejected_atomically() {
        let (sink, mut receiver) = data_event_channel_with_capacity(2);
        let _ = sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![
            point(1, 1),
            point(2, 2),
        ])));

        assert_eq!(
            sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![
                point(1, 99),
                point(3, 3),
            ]))),
            DataEventAdmission::DroppedFull
        );

        let Some(DataEvent::DataUpdate(batch)) = receiver.recv().await else {
            panic!("expected original batch");
        };
        let values: HashMap<_, _> = batch.iter().map(|point| (point.id, point.value)).collect();
        assert_eq!(values.get(&1), Some(&Value::Integer(1)));
        assert_eq!(values.get(&2), Some(&Value::Integer(2)));
        assert!(!values.contains_key(&3));
    }

    #[test]
    fn configured_capacity_is_preserved_exactly() {
        const CONFIGURED_POINTS: usize = 123_457;
        let (_sink, receiver) = data_event_channel_with_capacity(CONFIGURED_POINTS);

        assert_eq!(
            receiver.stats().capacity,
            (CONFIGURED_POINTS + ERROR_EVENT_CAPACITY + 2) as u64
        );
    }

    #[tokio::test]
    async fn lane_rotation_prevents_a_data_burst_from_starving_errors() {
        let (sink, mut receiver) = data_event_channel_with_capacity(600);
        for id in 0..600 {
            let _ = sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![point(
                id, 1,
            )])));
        }
        let _ = sink.publish(DataEvent::Error("must advance".to_string()));

        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::DataUpdate(_))
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::Error(message)) if message == "must advance"
        ));
    }

    #[tokio::test]
    async fn first_event_in_a_new_burst_keeps_its_cross_lane_causal_order() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        sink.publish(DataEvent::Error("first".to_string()));
        sink.publish(DataEvent::ConnectionChanged(ConnectionState::Error));
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::Error(message)) if message == "first"
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::ConnectionChanged(ConnectionState::Error))
        ));

        sink.publish(DataEvent::ConnectionChanged(ConnectionState::Connected));
        sink.publish(DataEvent::Error("second".to_string()));
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::ConnectionChanged(ConnectionState::Connected))
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::Error(message)) if message == "second"
        ));
    }

    #[tokio::test]
    async fn close_is_bounded_visible_and_rejects_late_publishers() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        let _ = sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![point(
            1, 1,
        )])));
        let _ = sink.publish(DataEvent::Error("pending".to_string()));

        assert_eq!(receiver.close(), 2);
        assert_eq!(
            sink.publish(DataEvent::Heartbeat),
            DataEventAdmission::DroppedClosed
        );
        assert!(receiver.recv().await.is_none());
        let stats = receiver.stats();
        assert!(!stats.receiver_open);
        assert_eq!(stats.discarded_on_close, 2);
        assert_eq!(stats.dropped_closed, 1);
        assert_eq!(stats.pending, 0);
    }

    #[test]
    fn shutdown_drain_obeys_the_event_limit_and_accounts_for_the_remainder() {
        let (sink, mut receiver) = data_event_channel_with_capacity(300);
        for id in 0..300 {
            let _ = sink.publish(DataEvent::DataUpdate(DataBatch::from_points(vec![point(
                id, 1,
            )])));
        }
        let _ = sink.publish(DataEvent::Error("pending error".to_string()));

        let (drained, discarded) = receiver.close_and_drain(1);

        assert_eq!(drained.len(), 1);
        assert!(matches!(&drained[0], DataEvent::DataUpdate(batch) if batch.len() == 256));
        assert_eq!(discarded, 45);
        assert_eq!(receiver.stats().discarded_on_close, 45);
        assert_eq!(receiver.stats().pending, 0);
    }

    #[test]
    fn oversized_payloads_are_rejected_without_consuming_capacity() {
        let (sink, receiver) = data_event_channel_with_capacity(1024);
        let points = (0..=1024).map(|id| point(id, 1)).collect();
        assert_eq!(
            sink.publish(DataEvent::DataUpdate(DataBatch::from_points(points))),
            DataEventAdmission::Oversized
        );
        assert_eq!(
            sink.publish(DataEvent::Error("x".repeat(MAX_ERROR_BYTES + 1))),
            DataEventAdmission::Oversized
        );
        let stats = receiver.stats();
        assert_eq!(stats.oversized, 2);
        assert_eq!(stats.pending, 0);
    }

    #[tokio::test]
    async fn receiver_drains_bounded_remainder_after_all_senders_exit() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        let _ = sink.publish(DataEvent::Heartbeat);
        drop(sink);
        assert!(matches!(receiver.recv().await, Some(DataEvent::Heartbeat)));
        assert!(receiver.recv().await.is_none());
    }

    #[test]
    fn a_contended_sink_drops_newest_instead_of_waiting() {
        let (sink, receiver) = data_event_channel_with_capacity(1);
        let guard = sink.shared.lock_state();

        assert_eq!(
            sink.publish(DataEvent::Heartbeat),
            DataEventAdmission::DroppedContended
        );
        drop(guard);

        let stats = receiver.stats();
        assert_eq!(stats.dropped_contended, 1);
        assert_eq!(stats.pending, 0);
    }

    #[tokio::test]
    async fn a_sender_clone_keeps_the_receiver_open_until_the_last_drop() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        let clone = sink.clone();
        drop(sink);
        assert_eq!(
            clone.publish(DataEvent::Heartbeat),
            DataEventAdmission::Accepted
        );
        assert!(matches!(receiver.recv().await, Some(DataEvent::Heartbeat)));
        drop(clone);
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_sender_drop_cannot_be_lost_before_notified_is_polled() {
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        let reached = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        receiver.install_wait_hook(Arc::clone(&reached), Arc::clone(&release));

        let waiter = tokio::spawn(async move { receiver.recv().await });
        tokio::task::spawn_blocking(move || reached.wait())
            .await
            .expect("reach recv wait window");
        drop(sink);
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release recv wait window");

        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("recv must observe final sender drop")
                .expect("receiver task")
                .is_none()
        );
    }

    #[test]
    fn close_racing_publishers_is_fail_closed_and_accounted() {
        const THREADS: usize = 4;
        const EVENTS_PER_THREAD: usize = 1_000;
        let (sink, mut receiver) = data_event_channel_with_capacity(1);
        let barrier = Arc::new(std::sync::Barrier::new(THREADS + 1));
        let mut workers = Vec::new();
        for _ in 0..THREADS {
            let sink = sink.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..EVENTS_PER_THREAD {
                    let _ = sink.publish(DataEvent::Heartbeat);
                }
            }));
        }
        barrier.wait();
        receiver.close();
        for worker in workers {
            worker.join().expect("publisher thread");
        }

        let stats = receiver.stats();
        assert_eq!(
            stats.accepted + stats.coalesced + stats.dropped_closed + stats.dropped_contended,
            (THREADS * EVENTS_PER_THREAD) as u64
        );
        assert_eq!(stats.pending, 0);
        assert!(!stats.receiver_open);
    }
}
