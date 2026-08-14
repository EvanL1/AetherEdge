use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use aether_shm_bridge::{
    PhysicalPointAddress, PointWatchEvent, PointWatchEventListener, SubscriptionBitmap,
    bitmap_path_for_consumer,
};
use axum::extract::ws::{Message, WebSocket};
use chrono::Utc;
use dashmap::{DashMap, mapref::entry::Entry};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use aether_domain::PointQuality;
use aether_shm_bridge::SlotSnapshot;

use crate::live_values::{GatewayValueSource, sample_age_ms};

const MAX_WS_CONNECTIONS: usize = 1_024;
const WS_CLIENT_QUEUE_CAPACITY: usize = 64;
const MAX_SUBSCRIPTION_CHANNELS: usize = 256;
const MAX_SUBSCRIPTION_DATA_TYPES: usize = 16;
const MAX_SUBSCRIPTION_TOKEN_BYTES: usize = 64;
const MIN_SUBSCRIPTION_INTERVAL_MS: u64 = 100;
const MAX_SUBSCRIPTION_INTERVAL_MS: u64 = 60_000;
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(10);

// ── Subscription State ────────────────────────────────────────────────────────

/// Cached metadata for a homepage calculated point.
#[derive(Debug, Clone)]
pub struct HomepagePoint {
    pub id: i64,
    pub name: String,
    pub unit: String,
    pub imgurl: String,
    /// Logical address resolved through the SHM routing manifest.
    pub formula: String,
}

#[derive(Debug, Clone, Default)]
pub struct Subscription {
    pub source: String,
    pub channels: Vec<i64>,
    pub data_types: Vec<String>,
    pub interval_ms: u64,
    /// Populated once on homepage subscribe; reused for every push tick.
    pub homepage_points: Vec<HomepagePoint>,
}

#[derive(Debug)]
struct ClientHandle {
    generation: u64,
    tx: mpsc::Sender<String>,
    sub: RwLock<Subscription>,
    data_type_ws: String,
    connected_at: i64,
    last_activity: Arc<AtomicI64>,
    last_push_ms: AtomicI64,
    pending_alarm_events: Mutex<HashSet<String>>,
    _connection_permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterError {
    ConnectionLimit,
    DuplicateClientId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectionIdentity {
    client_id: String,
    generation: u64,
}

struct RegisteredClient {
    identity: ConnectionIdentity,
    receiver: mpsc::Receiver<String>,
}

/// Grades every sample in a group so the payload cannot present a frozen
/// reading as a live one.
///
/// The values and their timestamps were always both on the wire, but nothing
/// said which timestamps still counted as current, so a disconnected channel
/// looked exactly like a healthy one.
fn quality_object(
    samples: &BTreeMap<String, SlotSnapshot>,
    now_ms: u64,
    stale_after_ms: u64,
) -> serde_json::Map<String, Value> {
    samples
        .iter()
        .map(|(point_id, sample)| {
            let age_ms = sample_age_ms(now_ms, sample.timestamp_ms());
            let freshness = PointQuality::for_sample_age(age_ms, stale_after_ms);
            let effective = match sample.quality() {
                PointQuality::Good => freshness,
                PointQuality::Uncertain => PointQuality::Uncertain,
                PointQuality::Bad => PointQuality::Bad,
                PointQuality::Unavailable => PointQuality::Unavailable,
            };
            let label = match effective {
                PointQuality::Good => "good",
                PointQuality::Uncertain => "uncertain",
                PointQuality::Bad => "bad",
                PointQuality::Unavailable => "unavailable",
            };
            (point_id.clone(), Value::String(label.to_owned()))
        })
        .collect()
}

/// Wall-clock milliseconds used to grade sample freshness.
fn now_ms_for_grading() -> u64 {
    u64::try_from(Utc::now().timestamp_millis()).unwrap_or(0)
}

// ── WebSocket Hub ─────────────────────────────────────────────────────────────

pub struct WsHub {
    clients: DashMap<String, Arc<ClientHandle>>,
    live_values: Arc<dyn GatewayValueSource>,
    db: SqlitePool,
    sample_stale_after_ms: u64,
    connection_slots: Arc<Semaphore>,
    next_generation: AtomicU64,
    dropped_messages: AtomicU64,
    alarm_event_delivery: tokio::sync::Mutex<()>,
}

impl WsHub {
    pub fn new(
        live_values: Arc<dyn GatewayValueSource>,
        db: SqlitePool,
        sample_stale_after_ms: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            clients: DashMap::new(),
            live_values,
            db,
            sample_stale_after_ms,
            connection_slots: Arc::new(Semaphore::new(MAX_WS_CONNECTIONS)),
            next_generation: AtomicU64::new(1),
            dropped_messages: AtomicU64::new(0),
            alarm_event_delivery: tokio::sync::Mutex::new(()),
        })
    }

    fn register(
        &self,
        client_id: String,
        data_type_ws: String,
    ) -> Result<RegisteredClient, RegisterError> {
        let permit = Arc::clone(&self.connection_slots)
            .try_acquire_owned()
            .map_err(|_| RegisterError::ConnectionLimit)?;
        let (tx, rx) = mpsc::channel(WS_CLIENT_QUEUE_CAPACITY);
        let now = Utc::now().timestamp();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);

        let handle = Arc::new(ClientHandle {
            generation,
            tx,
            sub: RwLock::new(Subscription {
                source: "inst".to_string(),
                channels: Vec::new(),
                data_types: Vec::new(),
                interval_ms: 1000,
                homepage_points: Vec::new(),
            }),
            data_type_ws,
            connected_at: now,
            last_activity: Arc::new(AtomicI64::new(now)),
            last_push_ms: AtomicI64::new(0),
            pending_alarm_events: Mutex::new(HashSet::new()),
            _connection_permit: permit,
        });

        match self.clients.entry(client_id) {
            Entry::Vacant(entry) => {
                let identity = ConnectionIdentity {
                    client_id: entry.key().clone(),
                    generation,
                };
                entry.insert(handle);
                Ok(RegisteredClient {
                    identity,
                    receiver: rx,
                })
            },
            Entry::Occupied(_) => Err(RegisterError::DuplicateClientId),
        }
    }

    fn current_connection(
        &self,
        client_id: &str,
    ) -> Option<(ConnectionIdentity, Arc<ClientHandle>)> {
        self.clients.get(client_id).map(|entry| {
            (
                ConnectionIdentity {
                    client_id: client_id.to_owned(),
                    generation: entry.generation,
                },
                Arc::clone(entry.value()),
            )
        })
    }

    fn current_handle(&self, identity: &ConnectionIdentity) -> Option<Arc<ClientHandle>> {
        self.clients.get(&identity.client_id).and_then(|entry| {
            (entry.generation == identity.generation).then(|| Arc::clone(entry.value()))
        })
    }

    fn deregister(&self, identity: &ConnectionIdentity) -> bool {
        let removed = self
            .clients
            .remove_if(&identity.client_id, |_, handle| {
                handle.generation == identity.generation
            })
            .is_some();
        if removed {
            info!(
                "WS client disconnected: {} generation={}",
                identity.client_id, identity.generation
            );
        }
        removed
    }

    fn update_subscription(
        &self,
        identity: &ConnectionIdentity,
        source: String,
        channels: Vec<i64>,
        data_types: Vec<String>,
        interval_ms: u64,
        homepage_points: Vec<HomepagePoint>,
    ) {
        if let Some(handle) = self.current_handle(identity)
            && let Ok(mut sub) = handle.sub.write()
        {
            sub.source = source;
            sub.channels = channels;
            sub.data_types = data_types;
            sub.interval_ms =
                interval_ms.clamp(MIN_SUBSCRIPTION_INTERVAL_MS, MAX_SUBSCRIPTION_INTERVAL_MS);
            sub.homepage_points = homepage_points;
            handle.last_push_ms.store(0, Ordering::Relaxed);
        }
    }

    fn update_activity(&self, identity: &ConnectionIdentity) {
        if let Some(handle) = self.current_handle(identity) {
            handle
                .last_activity
                .store(Utc::now().timestamp(), Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    fn send_to(&self, client_id: &str, msg: String) -> bool {
        let Some((identity, handle)) = self.current_connection(client_id) else {
            return false;
        };
        self.send_to_handle(&identity, &handle, msg)
    }

    fn send_to_connection(&self, identity: &ConnectionIdentity, msg: String) -> bool {
        let Some(handle) = self.current_handle(identity) else {
            return false;
        };
        self.send_to_handle(identity, &handle, msg)
    }

    fn send_to_handle(
        &self,
        identity: &ConnectionIdentity,
        handle: &ClientHandle,
        msg: String,
    ) -> bool {
        match handle.tx.try_send(msg) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_messages.fetch_add(1, Ordering::Relaxed);
                warn!(
                    "Disconnecting slow WS client {} generation={} after its bounded queue filled",
                    identity.client_id, identity.generation
                );
                self.deregister(identity);
                false
            },
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.deregister(identity);
                false
            },
        }
    }

    pub fn broadcast(&self, msg: &str) -> (usize, Vec<String>) {
        let mut count = 0;
        let mut ids = Vec::new();
        let mut disconnected = Vec::new();
        for entry in self.clients.iter() {
            let identity = ConnectionIdentity {
                client_id: entry.key().clone(),
                generation: entry.generation,
            };
            match entry.tx.try_send(msg.to_owned()) {
                Ok(()) => {
                    count += 1;
                    ids.push(entry.key().clone());
                },
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.dropped_messages.fetch_add(1, Ordering::Relaxed);
                    disconnected.push(identity);
                },
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    disconnected.push(identity);
                },
            }
        }
        for identity in disconnected {
            warn!(
                "Disconnecting unavailable or slow WS client {} generation={}",
                identity.client_id, identity.generation
            );
            self.deregister(&identity);
        }
        (count, ids)
    }

    /// Queues a pending event at most once for each live WebSocket connection
    /// generation until the durable ledger records `delivered`.
    ///
    /// The pending marker and queue insertion share the same per-client mutex. A
    /// concurrent retry therefore cannot enqueue twice, while a failed queue
    /// insertion never creates a marker that would suppress a later retry.
    pub fn broadcast_event_once(&self, event_id: &str, msg: &str) -> (usize, Vec<String>, usize) {
        let mut count = 0;
        let mut already_seen = 0;
        let mut ids = Vec::new();
        let mut disconnected = Vec::new();
        for entry in self.clients.iter() {
            let identity = ConnectionIdentity {
                client_id: entry.key().clone(),
                generation: entry.generation,
            };
            let Ok(mut pending) = entry.pending_alarm_events.lock() else {
                disconnected.push(identity);
                continue;
            };
            if pending.contains(event_id) {
                already_seen += 1;
                continue;
            }
            match entry.tx.try_send(msg.to_owned()) {
                Ok(()) => {
                    pending.insert(event_id.to_owned());
                    count += 1;
                    ids.push(entry.key().clone());
                },
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.dropped_messages.fetch_add(1, Ordering::Relaxed);
                    disconnected.push(identity);
                },
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    disconnected.push(identity);
                },
            }
        }
        for identity in disconnected {
            warn!(
                "Disconnecting unavailable or slow WS client {} generation={}",
                identity.client_id, identity.generation
            );
            self.deregister(&identity);
        }
        (count, ids, already_seen)
    }

    /// Releases connection-local suppression only after the durable ledger
    /// records delivery. Until then, same-process retries cannot enqueue a
    /// duplicate even if SQLite acknowledgement temporarily fails.
    pub fn finish_event_delivery(&self, event_id: &str) {
        for entry in &self.clients {
            if let Ok(mut pending) = entry.pending_alarm_events.lock() {
                pending.remove(event_id);
            }
        }
    }

    /// Serializes the SQLite pending/delivered transition with WebSocket queue
    /// insertion. Alarm event volume is low and a single delivery gate avoids
    /// unbounded per-event lock retention while providing process-local
    /// single-flight semantics.
    pub async fn alarm_event_delivery_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.alarm_event_delivery.lock().await
    }

    #[cfg(test)]
    pub(crate) fn register_test_client(&self, client_id: &str) -> mpsc::Receiver<String> {
        self.register(client_id.to_owned(), "test".to_owned())
            .expect("register test WebSocket client")
            .receiver
    }

    pub fn get_status(&self) -> Value {
        let mut connections = serde_json::Map::new();
        let mut subscriptions_map = serde_json::Map::new();

        for entry in self.clients.iter() {
            let id = entry.key();
            let Ok(sub) = entry.sub.read() else {
                continue;
            };

            connections.insert(
                id.clone(),
                json!({
                    "data_type": entry.data_type_ws,
                    "connected_at": entry.connected_at,
                    "last_activity": entry.last_activity.load(Ordering::Relaxed),
                }),
            );

            subscriptions_map.insert(
                id.clone(),
                json!({
                    "source": &sub.source,
                    "channels": &sub.channels,
                    "data_types": &sub.data_types,
                    "interval": sub.interval_ms,
                }),
            );
        }

        json!({
            "running": true,
            "connection_count": self.clients.len(),
            "connection_limit": MAX_WS_CONNECTIONS,
            "queue_capacity_per_client": WS_CLIENT_QUEUE_CAPACITY,
            "dropped_messages": self.dropped_messages.load(Ordering::Relaxed),
            "connections_info": connections,
            "subscriptions": subscriptions_map,
        })
    }

    /// Cleanup clients that have been inactive for > 5 minutes.
    pub fn cleanup_inactive(&self) {
        let now = Utc::now().timestamp();
        let inactive: Vec<ConnectionIdentity> = self
            .clients
            .iter()
            .filter(|e| now - e.last_activity.load(Ordering::Relaxed) > 300)
            .map(|entry| ConnectionIdentity {
                client_id: entry.key().clone(),
                generation: entry.generation,
            })
            .collect();

        for identity in inactive {
            warn!(
                "Removing inactive WS client: {} generation={}",
                identity.client_id, identity.generation
            );
            self.deregister(&identity);
        }
    }

    fn subscription_slots(&self, subscription: &Subscription) -> BTreeSet<usize> {
        if subscription.source == "homepage" {
            return subscription
                .homepage_points
                .iter()
                .filter(|point| !point.formula.is_empty())
                .filter_map(
                    |point| match self.live_values.watched_formula_slot(&point.formula) {
                        Ok(slot) => slot,
                        Err(error) => {
                            debug!(
                                "Cannot resolve homepage PointWatch formula '{}': {error}",
                                point.formula
                            );
                            None
                        },
                    },
                )
                .collect();
        }
        if subscription.source == "rule" {
            return BTreeSet::new();
        }
        self.live_values
            .watched_slots(
                &subscription.source,
                &subscription.channels,
                &subscription.data_types,
            )
            .unwrap_or_else(|error| {
                debug!(
                    "Cannot resolve PointWatch subscription '{}': {error}",
                    subscription.source
                );
                BTreeSet::new()
            })
    }

    fn all_subscription_slots(&self) -> BTreeSet<usize> {
        self.clients
            .iter()
            .filter_map(|client| {
                client
                    .sub
                    .read()
                    .ok()
                    .map(|sub| self.subscription_slots(&sub))
            })
            .flatten()
            .collect()
    }

    fn subscription_addresses(&self, subscription: &Subscription) -> HashSet<PhysicalPointAddress> {
        if subscription.source == "homepage" {
            return subscription
                .homepage_points
                .iter()
                .filter(|point| !point.formula.is_empty())
                .filter_map(|point| {
                    match self.live_values.watched_formula_address(&point.formula) {
                        Ok(address) => address,
                        Err(error) => {
                            debug!(
                                "Cannot resolve homepage PointWatch formula '{}': {error}",
                                point.formula
                            );
                            None
                        },
                    }
                })
                .collect();
        }
        if subscription.source == "rule" {
            return HashSet::new();
        }
        self.live_values
            .watched_addresses(
                &subscription.source,
                &subscription.channels,
                &subscription.data_types,
            )
            .unwrap_or_else(|error| {
                debug!(
                    "Cannot resolve PointWatch subscription '{}': {error}",
                    subscription.source
                );
                HashSet::new()
            })
    }

    fn clients_watching(&self, changed_addresses: &HashSet<PhysicalPointAddress>) -> Vec<String> {
        self.clients
            .iter()
            .filter_map(|client| {
                let subscription = client.sub.read().ok()?;
                self.subscription_addresses(&subscription)
                    .iter()
                    .any(|address| changed_addresses.contains(address))
                    .then(|| client.key().clone())
            })
            .collect()
    }

    fn mark_push_if_due(&self, handle: &ClientHandle, interval_ms: u64) -> bool {
        let now_ms = Utc::now().timestamp_millis();
        let interval_ms = i64::try_from(interval_ms).unwrap_or(i64::MAX);
        let mut last = handle.last_push_ms.load(Ordering::Relaxed);
        loop {
            if last > 0 && now_ms.saturating_sub(last) < interval_ms {
                return false;
            }
            match handle.last_push_ms.compare_exchange_weak(
                last,
                now_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => last = current,
            }
        }
    }
}

// ── Background Tasks ──────────────────────────────────────────────────────────

/// Sentinel string sent via the text channel to trigger a WebSocket Ping frame.
/// The send_task converts this to `Message::Ping` so the browser WebSocket
/// library handles keepalive natively without surfacing an "unknown message
/// type" warning in application-level code.
const WS_PING_SENTINEL: &str = "\x00__ping__\x00";

/// Periodic heartbeat: sends a native WebSocket Ping frame to every client.
/// The browser responds automatically with a Pong; no application-level
/// handler is needed on the frontend.
pub async fn run_heartbeat(hub: Arc<WsHub>, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => {
                hub.broadcast(WS_PING_SENTINEL);
                hub.cleanup_inactive();
            }
        }
    }
}

/// Event-assisted data push to subscribed clients with periodic reconciliation.
#[allow(clippy::too_many_arguments)]
pub async fn run_data_push(
    hub: Arc<WsHub>,
    shutdown: CancellationToken,
    interval_secs: u64,
    shm_path: &str,
    point_watch_socket: &str,
    debounce_ms: u64,
    point_watch_capacity: usize,
) {
    let (listener, mut event_rx) =
        PointWatchEventListener::new(point_watch_socket, shutdown.clone());
    let listener_task = tokio::spawn(async move {
        if let Err(error) = listener.run().await {
            warn!(
                "API Gateway PointWatch listener unavailable; polling fallback remains active: {error}"
            );
        }
    });
    let bitmap_path = bitmap_path_for_consumer(Path::new(shm_path), "api");
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut events_open = true;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => {
                reconcile_point_watch_subscriptions(
                    &hub,
                    &bitmap_path,
                    point_watch_capacity,
                );
                push_subscribed_data(&hub).await;
            }
            event = event_rx.recv(), if events_open => {
                match event {
                    Some(event) => {
                        push_point_watch_batch(
                            &hub,
                            &mut event_rx,
                            event,
                            debounce_ms,
                            &shutdown,
                        ).await;
                    },
                    None => events_open = false,
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), listener_task).await;
}

async fn push_subscribed_data(hub: &Arc<WsHub>) {
    let client_ids: Vec<String> = hub.clients.iter().map(|e| e.key().clone()).collect();
    push_subscribed_data_to(hub, client_ids).await;
}

async fn push_subscribed_data_to(hub: &Arc<WsHub>, client_ids: Vec<String>) {
    for client_id in client_ids {
        let (connection, source, channels, data_types) = {
            let Some((connection, handle)) = hub.current_connection(&client_id) else {
                continue;
            };
            let Ok(sub) = handle.sub.read() else {
                continue;
            };
            if sub.channels.is_empty() && sub.source != "homepage" {
                continue;
            }
            if !hub.mark_push_if_due(&handle, sub.interval_ms) {
                continue;
            }
            (
                connection,
                sub.source.clone(),
                sub.channels.clone(),
                sub.data_types.clone(),
            )
        };

        if source == "rule" {
            if let Some(rule_id) = channels.first() {
                push_rule_data(hub, &connection, *rule_id).await;
            }
            continue;
        }

        if source == "homepage" {
            push_homepage_data(hub, &connection).await;
            continue;
        }

        // Standard source:channel_id:data_type subscriptions
        let mut all_updates = Vec::new();
        for channel_id in &channels {
            for dt in &data_types {
                let samples = match hub.live_values.read_group(&source, *channel_id, dt) {
                    Ok(samples) if !samples.is_empty() => samples,
                    Err(error) => {
                        debug!(
                            "SHM group read {}:{}:{} failed: {}",
                            source, channel_id, dt, error
                        );
                        continue;
                    },
                    _ => continue,
                };

                let values_obj: serde_json::Map<String, Value> = samples
                    .iter()
                    .map(|(point_id, sample)| {
                        let value = serde_json::Number::from_f64(sample.value())
                            .map(Value::Number)
                            .unwrap_or(Value::Null);
                        (point_id.clone(), value)
                    })
                    .collect();

                let ts_obj: serde_json::Map<String, Value> = samples
                    .iter()
                    .map(|(point_id, sample)| {
                        (
                            point_id.clone(),
                            Value::Number(serde_json::Number::from(sample.timestamp_ms())),
                        )
                    })
                    .collect();

                let quality_obj =
                    quality_object(&samples, now_ms_for_grading(), hub.sample_stale_after_ms);

                all_updates.push(json!({
                    "source": source,
                    "channel_id": channel_id,
                    "data_type": dt,
                    "values": values_obj,
                    "ts": ts_obj,
                    "quality": quality_obj,
                }));
            }
        }

        if !all_updates.is_empty() {
            let now = Utc::now().timestamp();
            let msg = json!({
                "type": "data_batch",
                "id": format!("batch_{}", now),
                "timestamp": now,
                "data": { "updates": all_updates },
            })
            .to_string();
            hub.send_to_connection(&connection, msg);
        }
    }
}

fn reconcile_point_watch_subscriptions(hub: &WsHub, bitmap_path: &Path, capacity: usize) {
    let bitmap = match SubscriptionBitmap::open_or_create(bitmap_path, capacity) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            debug!(
                "API Gateway PointWatch bitmap unavailable at {}: {error}",
                bitmap_path.display()
            );
            return;
        },
    };
    bitmap.clear_all();
    for slot in hub.all_subscription_slots() {
        if let Err(error) = bitmap.set_watched(slot) {
            warn!("Cannot subscribe API PointWatch slot {slot}: {error}");
        }
    }
    debug!(
        "API Gateway PointWatch subscriptions reconciled: {} slot(s)",
        bitmap.subscription_count()
    );
}

async fn push_point_watch_batch(
    hub: &Arc<WsHub>,
    event_rx: &mut mpsc::Receiver<PointWatchEvent>,
    first: PointWatchEvent,
    debounce_ms: u64,
    shutdown: &CancellationToken,
) {
    let mut events = vec![first];
    tokio::select! {
        _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_millis(debounce_ms)) => {}
    }
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    let mut changed_addresses = HashSet::new();
    for event in events {
        match hub.live_values.validate_point_watch(event) {
            Ok(Some(validated)) => {
                changed_addresses.insert(validated.address());
            },
            Ok(None) => {},
            Err(error) => {
                debug!(
                    channel_id = event.channel_id(),
                    point_id = event.point_id(),
                    slot = event.slot_index(),
                    "API PointWatch SHM re-read rejected: {error}"
                );
            },
        }
    }
    if changed_addresses.is_empty() {
        return;
    }
    let clients = hub.clients_watching(&changed_addresses);
    if !clients.is_empty() {
        debug!(
            "PointWatch woke {} API Gateway client subscription(s)",
            clients.len()
        );
        push_subscribed_data_to(hub, clients).await;
    }
}

/// Load calculated_points from SQLite once at subscribe time.
async fn load_homepage_points(db: &SqlitePool) -> Vec<HomepagePoint> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        name: String,
        formula: Option<String>,
        unit: Option<String>,
        imgurl: Option<String>,
    }

    match sqlx::query_as::<_, Row>(
        "SELECT id, name, formula, unit, imgurl FROM calculated_points ORDER BY id",
    )
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|r| HomepagePoint {
                id: r.id,
                name: r.name,
                unit: r.unit.unwrap_or_default(),
                imgurl: r.imgurl.unwrap_or_default(),
                formula: r.formula.unwrap_or_default(),
            })
            .collect(),
        Err(e) => {
            error!("Failed to load calculated_points: {}", e);
            Vec::new()
        },
    }
}

/// Push homepage_batch to a subscribed client.
/// Uses the point list cached at subscribe time and reads current values from SHM.
///
/// Formula uses a logical point key (for example `inst:42:M:7`) that resolves
/// to a physical SHM slot.
/// Empty formula → value is null.
async fn push_homepage_data(hub: &Arc<WsHub>, connection: &ConnectionIdentity) {
    let points = {
        let Some(handle) = hub.current_handle(connection) else {
            return;
        };
        let Ok(sub) = handle.sub.read() else { return };
        sub.homepage_points.clone()
    };

    if points.is_empty() {
        return;
    }

    let mut updates = Vec::with_capacity(points.len());
    for pt in &points {
        let value = if !pt.formula.is_empty() {
            match hub.live_values.read_formula(&pt.formula) {
                Ok(Some(sample)) => serde_json::Number::from_f64(sample.value())
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                Ok(None) => Value::Null,
                Err(error) => {
                    debug!("Homepage SHM formula '{}' failed: {error}", pt.formula);
                    Value::Null
                },
            }
        } else {
            Value::Null
        };

        updates.push(json!({
            "id": pt.id,
            "name": pt.name,
            "values": value,
            "unit": pt.unit,
            "imgurl": pt.imgurl,
        }));
    }

    let now = Utc::now().timestamp();
    let msg = json!({
        "type": "homepage_batch",
        "id": format!("homepage_batch_{}", now),
        "timestamp": now,
        "data": { "updates": updates }
    })
    .to_string();
    hub.send_to_connection(connection, msg);
}

async fn push_rule_data(hub: &Arc<WsHub>, connection: &ConnectionIdentity, rule_id: i64) {
    match load_rule_execution(&hub.db, rule_id).await {
        Ok(Some(execution)) => {
            let msg = json!({
                "type": "data_batch",
                "timestamp": Utc::now().timestamp(),
                "data": {
                    "rule_id": rule_id,
                    "rule_name": execution.rule_name,
                    "variables": {},
                    "last_execution": {
                        "success": execution.success,
                        "timestamp": execution.timestamp,
                        "error": execution.error,
                        "execution_path": execution.execution_path,
                        "variable_values": execution.variable_values,
                        "node_details": execution.node_details,
                    }
                }
            })
            .to_string();

            hub.send_to_connection(connection, msg);
        },
        Err(error) => debug!("Rule history query failed rule={rule_id}: {error}"),
        _ => {},
    }
}

#[derive(Debug, PartialEq)]
struct RuleExecutionView {
    rule_name: String,
    timestamp: i64,
    success: bool,
    error: Option<String>,
    execution_path: Value,
    variable_values: Value,
    node_details: Value,
}

async fn load_rule_execution(
    db: &SqlitePool,
    rule_id: i64,
) -> sqlx::Result<Option<RuleExecutionView>> {
    let row = sqlx::query_as::<_, (String, i64, Option<String>, Option<String>)>(
        "SELECT COALESCE(r.name, ''), \
                COALESCE(CAST(strftime('%s', h.triggered_at) AS INTEGER), 0), \
                h.execution_result, h.error \
         FROM rule_history h \
         LEFT JOIN rules r ON r.id = h.rule_id \
         WHERE h.rule_id = ? \
         ORDER BY h.id DESC LIMIT 1",
    )
    .bind(rule_id)
    .fetch_optional(db)
    .await?;

    Ok(
        row.map(|(rule_name, timestamp, execution_result, stored_error)| {
            let payload = execution_result
                .as_deref()
                .and_then(|json| serde_json::from_str::<Value>(json).ok())
                .unwrap_or_else(|| json!({}));
            let error = stored_error.filter(|error| !error.is_empty()).or_else(|| {
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .map(String::from)
            });
            RuleExecutionView {
                rule_name,
                timestamp,
                success: payload
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| error.is_none()),
                error,
                execution_path: payload
                    .get("execution_path")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
                variable_values: payload
                    .get("variable_values")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                node_details: payload
                    .get("node_details")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            }
        }),
    )
}

// ── WebSocket Connection Handler ──────────────────────────────────────────────

pub async fn handle_socket(
    socket: WebSocket,
    client_id: String,
    data_type_ws: String,
    hub: Arc<WsHub>,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let registered = match hub.register(client_id.clone(), data_type_ws) {
        Ok(registered) => registered,
        Err(RegisterError::ConnectionLimit) => {
            let busy = error_msg(
                "CONNECTION_LIMIT",
                "WebSocket connection limit reached",
                None,
            );
            let _ = ws_sender.send(Message::Text(busy.into())).await;
            let _ = ws_sender.close().await;
            return;
        },
        Err(RegisterError::DuplicateClientId) => {
            let conflict = error_msg(
                "DUPLICATE_CLIENT_ID",
                "WebSocket client_id is already connected",
                None,
            );
            let _ = ws_sender.send(Message::Text(conflict.into())).await;
            let _ = ws_sender.close().await;
            return;
        },
    };
    let connection = registered.identity;
    let mut rx = registered.receiver;

    // Send welcome message
    let welcome = json!({
        "type": "connection_established",
        "id": format!("welcome_{}", client_id),
        "timestamp": Utc::now().timestamp(),
        "data": {
            "client_id": client_id,
            "message": "Connected. Subscribe to a data channel to receive real-time data"
        }
    })
    .to_string();

    if !matches!(
        tokio::time::timeout(
            WS_SEND_TIMEOUT,
            ws_sender.send(Message::Text(welcome.into()))
        )
        .await,
        Ok(Ok(()))
    ) {
        hub.deregister(&connection);
        return;
    }

    info!("WS client connected: {}", client_id);

    // Forward from channel to WebSocket.
    // WS_PING_SENTINEL is converted to a protocol-level Ping frame so the
    // browser handles it transparently without triggering application logic.
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let frame = if msg == WS_PING_SENTINEL {
                Message::Ping(bytes::Bytes::new())
            } else {
                Message::Text(msg.into())
            };
            if !matches!(
                tokio::time::timeout(WS_SEND_TIMEOUT, ws_sender.send(frame)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        let _ = tokio::time::timeout(WS_SEND_TIMEOUT, ws_sender.close()).await;
    });

    // Handle incoming messages
    let hub_recv = hub.clone();
    let connection_recv = connection.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    hub_recv.update_activity(&connection_recv);
                    handle_client_message(&hub_recv, &connection_recv, &text).await;
                },
                Message::Ping(data) => {
                    // Axum handles pong automatically, just update activity
                    hub_recv.update_activity(&connection_recv);
                    let _ = data;
                },
                Message::Pong(_) => {
                    hub_recv.update_activity(&connection_recv);
                },
                Message::Close(_) => break,
                _ => {},
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => { recv_task.abort(); }
        _ = &mut recv_task => { send_task.abort(); }
    }

    hub.deregister(&connection);
}

async fn handle_client_message(hub: &WsHub, connection: &ConnectionIdentity, text: &str) {
    let data: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            let err = error_msg("INVALID_JSON", "Invalid JSON format", None);
            hub.send_to_connection(connection, err);
            return;
        },
    };

    let msg_type = data["type"].as_str().unwrap_or("");

    match msg_type {
        "ping" => {
            let pong = json!({
                "type": "pong",
                "id": data["id"],
                "timestamp": Utc::now().timestamp(),
                "data": { "latency_ms": 0 }
            })
            .to_string();
            hub.send_to_connection(connection, pong);
        },

        "subscribe" => {
            let Some(payload) = data["data"].as_object() else {
                hub.send_to_connection(
                    connection,
                    error_msg(
                        "INVALID_SUBSCRIPTION",
                        "Missing subscription data",
                        data["id"].as_str(),
                    ),
                );
                return;
            };
            let source_value = payload
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("inst");
            let channel_values = payload
                .get("channels")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let data_type_values = payload
                .get("data_types")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if source_value.len() > MAX_SUBSCRIPTION_TOKEN_BYTES
                || channel_values.len() > MAX_SUBSCRIPTION_CHANNELS
                || data_type_values.len() > MAX_SUBSCRIPTION_DATA_TYPES
                || data_type_values.iter().any(|value| {
                    value
                        .as_str()
                        .is_none_or(|token| token.len() > MAX_SUBSCRIPTION_TOKEN_BYTES)
                })
            {
                hub.send_to_connection(
                    connection,
                    error_msg(
                        "SUBSCRIPTION_LIMIT",
                        "Subscription exceeds the connection limits",
                        data["id"].as_str(),
                    ),
                );
                return;
            }
            let source = data["data"]["source"]
                .as_str()
                .unwrap_or("inst")
                .to_string();
            let channels: Vec<i64> = data["data"]["channels"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
                .unwrap_or_default();
            let data_types: Vec<String> = data["data"]["data_types"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| vec!["T".to_string()]);
            let interval_ms: u64 = data["data"]["interval"].as_u64().unwrap_or(1000);
            let is_homepage = source == "homepage";

            // Load homepage points once from DB when subscribing to "homepage" source.
            let homepage_points = if is_homepage {
                load_homepage_points(&hub.db).await
            } else {
                Vec::new()
            };

            let ack = if is_homepage {
                json!({
                    "type": "subscribe_ack",
                    "id": format!("{}_ack", data["id"].as_str().unwrap_or("sub")),
                    "timestamp": Utc::now().timestamp(),
                    "data": { "source": "homepage", "message": "Homepage data subscription active" }
                })
            } else {
                json!({
                    "type": "subscribe_ack",
                    "id": format!("{}_ack", data["id"].as_str().unwrap_or("sub")),
                    "timestamp": Utc::now().timestamp(),
                    "data": { "subscribed": &channels, "failed": [] }
                })
            };
            hub.update_subscription(
                connection,
                source,
                channels,
                data_types,
                interval_ms,
                homepage_points,
            );
            hub.send_to_connection(connection, ack.to_string());
        },

        "unsubscribe" => {
            let source = data["data"]["source"].as_str().unwrap_or("inst");
            hub.update_subscription(
                connection,
                source.to_string(),
                Vec::new(),
                Vec::new(),
                1000,
                Vec::new(),
            );
            let ack = json!({
                "type": "unsubscribe_ack",
                "id": format!("{}_ack", data["id"].as_str().unwrap_or("unsub")),
                "timestamp": Utc::now().timestamp(),
                "data": { "unsubscribed": [], "failed": [] }
            })
            .to_string();
            hub.send_to_connection(connection, ack);
        },

        _ => {
            debug!(
                "Unknown WS message type '{}' from {}",
                msg_type, connection.client_id
            );
            hub.send_to_connection(
                connection,
                error_msg(
                    "UNKNOWN_MESSAGE_TYPE",
                    "Unsupported WebSocket message type",
                    data["id"].as_str(),
                ),
            );
        },
    }
}

fn error_msg(code: &str, message: &str, request_id: Option<&str>) -> String {
    json!({
        "type": "error",
        "timestamp": Utc::now().timestamp(),
        "data": {
            "code": code,
            "message": message,
            "request_id": request_id,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_ports::PortResult;
    use sqlx::sqlite::SqlitePoolOptions;

    struct EmptyValueSource;

    impl GatewayValueSource for EmptyValueSource {
        fn read_group(
            &self,
            _source: &str,
            _owner_id: i64,
            _data_type: &str,
        ) -> PortResult<BTreeMap<String, SlotSnapshot>> {
            Ok(BTreeMap::new())
        }

        fn read_formula(&self, _formula: &str) -> PortResult<Option<SlotSnapshot>> {
            Ok(None)
        }

        fn watched_slots(
            &self,
            _source: &str,
            _owner_ids: &[i64],
            _data_types: &[String],
        ) -> PortResult<BTreeSet<usize>> {
            Ok(BTreeSet::new())
        }

        fn watched_formula_slot(&self, _formula: &str) -> PortResult<Option<usize>> {
            Ok(None)
        }
    }

    async fn test_hub() -> Arc<WsHub> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open WebSocket test database");
        WsHub::new(Arc::new(EmptyValueSource), pool, 30_000)
    }

    #[test]
    fn a_group_payload_grades_every_sample_it_reports() {
        // A channel that stops responding leaves its last value in the slot.
        // Without this map the frozen reading is indistinguishable from a live
        // one, which is exactly how a disconnected device kept looking healthy.
        let mut samples = BTreeMap::new();
        samples.insert(
            "1".to_owned(),
            SlotSnapshot::new(384.3, 1_000, PointQuality::Good),
        );
        samples.insert(
            "2".to_owned(),
            SlotSnapshot::new(12.5, 95_000, PointQuality::Good),
        );

        let graded = quality_object(&samples, 100_000, 30_000);

        assert_eq!(graded["1"], "uncertain");
        assert_eq!(graded["2"], "good");
    }

    async fn rule_history_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::query(
            "CREATE TABLE rules (id INTEGER PRIMARY KEY, name TEXT NOT NULL);\
             CREATE TABLE rule_history (\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\
                 rule_id INTEGER NOT NULL,\
                 triggered_at TIMESTAMP NOT NULL,\
                 execution_result TEXT,\
                 error TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create rule history schema");
        pool
    }

    #[tokio::test]
    async fn retired_control_message_has_no_dedicated_protocol_branch() {
        let hub = test_hub().await;
        let mut registered = hub
            .register("unknown-type".to_owned(), "general".to_owned())
            .expect("connection slot available");
        let request = json!({
            "type": "control",
            "id": "cmd-1",
            "data": {
                "channel_id": 1,
                "point_id": 2,
                "command_type": "write",
                "value": 3
            }
        });

        handle_client_message(&hub, &registered.identity, &request.to_string()).await;

        let response: Value = serde_json::from_str(
            &registered
                .receiver
                .recv()
                .await
                .expect("unknown-message response"),
        )
        .expect("valid JSON response");
        assert_eq!(response["type"], "error");
        assert_eq!(response["data"]["code"], "UNKNOWN_MESSAGE_TYPE");
        assert_eq!(response["data"]["request_id"], "cmd-1");
    }

    #[tokio::test]
    async fn slow_client_is_disconnected_when_bounded_queue_fills() {
        let hub = test_hub().await;
        let _registered = hub
            .register("slow".to_owned(), "general".to_owned())
            .expect("connection slot available");

        for sequence in 0..WS_CLIENT_QUEUE_CAPACITY {
            assert!(hub.send_to("slow", format!("message-{sequence}")));
        }
        assert!(!hub.send_to("slow", "overflow".to_owned()));

        let status = hub.get_status();
        assert_eq!(status["connection_count"], 0);
        assert_eq!(status["dropped_messages"], 1);
    }

    #[tokio::test]
    async fn duplicate_client_id_cannot_replace_an_active_connection() {
        let hub = test_hub().await;
        let mut original = hub
            .register("same-id".to_owned(), "general".to_owned())
            .expect("first connection");

        assert!(
            matches!(
                hub.register("same-id".to_owned(), "replacement".to_owned()),
                Err(RegisterError::DuplicateClientId)
            ),
            "a duplicate socket must not steal another socket's registry entry"
        );
        assert!(hub.send_to("same-id", "still-original".to_owned()));
        assert_eq!(
            original.receiver.recv().await.as_deref(),
            Some("still-original"),
            "the original socket remains registered"
        );
    }

    #[tokio::test]
    async fn stale_connection_cannot_mutate_respond_to_or_remove_a_replacement() {
        let hub = test_hub().await;
        let original = hub
            .register("reused-id".to_owned(), "original".to_owned())
            .expect("original connection");
        let original_identity = original.identity.clone();
        let original_handle = hub
            .current_handle(&original_identity)
            .expect("original handle");
        assert!(hub.deregister(&original_identity));

        let mut replacement = hub
            .register("reused-id".to_owned(), "replacement".to_owned())
            .expect("replacement connection");
        let replacement_identity = replacement.identity.clone();

        hub.update_subscription(
            &original_identity,
            "rule".to_owned(),
            vec![42],
            vec!["T".to_owned()],
            100,
            Vec::new(),
        );
        hub.update_activity(&original_identity);
        assert!(!hub.send_to_connection(&original_identity, "stale-response".to_owned()));
        assert!(!hub.deregister(&original_identity));

        for sequence in 0..WS_CLIENT_QUEUE_CAPACITY {
            assert!(hub.send_to_handle(
                &original_identity,
                &original_handle,
                format!("stale-{sequence}")
            ));
        }
        assert!(!hub.send_to_handle(
            &original_identity,
            &original_handle,
            "stale-overflow".to_owned()
        ));

        let replacement_handle = hub
            .current_handle(&replacement_identity)
            .expect("replacement remains current");
        {
            let replacement_subscription = replacement_handle
                .sub
                .read()
                .expect("replacement subscription lock");
            assert_eq!(replacement_subscription.source, "inst");
            assert!(replacement_subscription.channels.is_empty());
        }

        assert!(matches!(
            replacement.receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(hub.send_to("reused-id", "current-message".to_owned()));
        assert_eq!(
            replacement.receiver.recv().await.as_deref(),
            Some("current-message")
        );
    }

    #[tokio::test]
    async fn oversized_subscription_is_rejected_without_replacing_current_state() {
        let hub = test_hub().await;
        let mut registered = hub
            .register("bounded".to_owned(), "general".to_owned())
            .expect("connection slot available");
        let channels: Vec<i64> = (0..=MAX_SUBSCRIPTION_CHANNELS as i64).collect();
        let request = json!({
            "type": "subscribe",
            "id": "too-many",
            "data": {
                "source": "inst",
                "channels": channels,
                "data_types": ["T"]
            }
        });

        handle_client_message(&hub, &registered.identity, &request.to_string()).await;

        let response: Value = serde_json::from_str(
            &registered
                .receiver
                .recv()
                .await
                .expect("subscription error response"),
        )
        .expect("valid JSON response");
        assert_eq!(response["data"]["code"], "SUBSCRIPTION_LIMIT");
        assert_eq!(
            hub.get_status()["subscriptions"]["bounded"]["channels"],
            json!([])
        );
    }

    #[tokio::test]
    async fn latest_rule_execution_is_loaded_from_local_history() {
        let pool = rule_history_pool().await;
        sqlx::query("INSERT INTO rules (id, name) VALUES (7, 'peak shave')")
            .execute(&pool)
            .await
            .expect("insert rule");
        sqlx::query(
            "INSERT INTO rule_history \
             (rule_id, triggered_at, execution_result, error) \
             VALUES (7, '2026-07-10 08:09:10', ?, NULL)",
        )
        .bind(
            json!({
                "success": true,
                "execution_path": ["start", "end"],
                "variable_values": {"soc": 52.5},
                "node_details": {"end": {"status": "ok"}}
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .expect("insert history");

        let execution = load_rule_execution(&pool, 7)
            .await
            .expect("query history")
            .expect("latest execution");

        assert_eq!(execution.rule_name, "peak shave");
        assert!(execution.success);
        assert_eq!(execution.execution_path, json!(["start", "end"]));
        assert_eq!(execution.variable_values["soc"], 52.5);
        assert!(execution.timestamp > 0);
    }
}
