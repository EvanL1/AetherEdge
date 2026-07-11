use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aether_shm_bridge::{
    PointWatchEvent, PointWatchEventListener, SubscriptionBitmap, bitmap_path_for_consumer,
};
use axum::extract::ws::{Message, WebSocket};
use chrono::Utc;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::live_values::GatewayValueSource;

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
    tx: mpsc::UnboundedSender<String>,
    sub: RwLock<Subscription>,
    data_type_ws: String,
    connected_at: i64,
    last_activity: Arc<AtomicI64>,
}

// ── WebSocket Hub ─────────────────────────────────────────────────────────────

pub struct WsHub {
    clients: DashMap<String, Arc<ClientHandle>>,
    live_values: Arc<dyn GatewayValueSource>,
    db: SqlitePool,
}

impl WsHub {
    pub fn new(live_values: Arc<dyn GatewayValueSource>, db: SqlitePool) -> Arc<Self> {
        Arc::new(Self {
            clients: DashMap::new(),
            live_values,
            db,
        })
    }

    pub fn register(
        &self,
        client_id: String,
        data_type_ws: String,
    ) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let now = Utc::now().timestamp();

        let handle = Arc::new(ClientHandle {
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
        });

        self.clients.insert(client_id, handle);
        rx
    }

    pub fn deregister(&self, client_id: &str) {
        self.clients.remove(client_id);
        info!("WS client disconnected: {}", client_id);
    }

    pub fn update_subscription(
        &self,
        client_id: &str,
        source: String,
        channels: Vec<i64>,
        data_types: Vec<String>,
        interval_ms: u64,
        homepage_points: Vec<HomepagePoint>,
    ) {
        if let Some(handle) = self.clients.get(client_id)
            && let Ok(mut sub) = handle.sub.write()
        {
            sub.source = source;
            sub.channels = channels;
            sub.data_types = data_types;
            sub.interval_ms = interval_ms;
            sub.homepage_points = homepage_points;
        }
    }

    pub fn update_activity(&self, client_id: &str) {
        if let Some(handle) = self.clients.get(client_id) {
            handle
                .last_activity
                .store(Utc::now().timestamp(), Ordering::Relaxed);
        }
    }

    pub fn send_to(&self, client_id: &str, msg: String) -> bool {
        match self.clients.get(client_id) {
            Some(handle) => handle.tx.send(msg).is_ok(),
            _ => false,
        }
    }

    pub fn broadcast(&self, msg: &str) -> (usize, Vec<String>) {
        let mut count = 0;
        let mut ids = Vec::new();
        for entry in self.clients.iter() {
            if entry.tx.send(msg.to_string()).is_ok() {
                count += 1;
                ids.push(entry.key().clone());
            }
        }
        (count, ids)
    }

    #[allow(dead_code)]
    pub fn connection_count(&self) -> usize {
        self.clients.len()
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
            "connections_info": connections,
            "subscriptions": subscriptions_map,
        })
    }

    /// Cleanup clients that have been inactive for > 5 minutes.
    pub fn cleanup_inactive(&self) {
        let now = Utc::now().timestamp();
        let inactive: Vec<String> = self
            .clients
            .iter()
            .filter(|e| now - e.last_activity.load(Ordering::Relaxed) > 300)
            .map(|e| e.key().clone())
            .collect();

        for id in inactive {
            warn!("Removing inactive WS client: {}", id);
            self.clients.remove(&id);
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

    fn clients_watching(&self, changed_slots: &HashSet<usize>) -> Vec<String> {
        self.clients
            .iter()
            .filter_map(|client| {
                let subscription = client.sub.read().ok()?;
                self.subscription_slots(&subscription)
                    .iter()
                    .any(|slot| changed_slots.contains(slot))
                    .then(|| client.key().clone())
            })
            .collect()
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
                reconcile_point_watch_subscriptions(&hub, &bitmap_path);
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
        let (source, channels, data_types) = {
            let Some(handle) = hub.clients.get(&client_id) else {
                continue;
            };
            let Ok(sub) = handle.sub.read() else {
                continue;
            };
            if sub.channels.is_empty() && sub.source != "homepage" {
                continue;
            }
            (
                sub.source.clone(),
                sub.channels.clone(),
                sub.data_types.clone(),
            )
        };

        if source == "rule" {
            if let Some(rule_id) = channels.first() {
                push_rule_data(hub, &client_id, *rule_id).await;
            }
            continue;
        }

        if source == "homepage" {
            push_homepage_data(hub, &client_id).await;
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

                all_updates.push(json!({
                    "source": source,
                    "channel_id": channel_id,
                    "data_type": dt,
                    "values": values_obj,
                    "ts": ts_obj,
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
            hub.send_to(&client_id, msg);
        }
    }
}

fn reconcile_point_watch_subscriptions(hub: &WsHub, bitmap_path: &Path) {
    let bitmap = match SubscriptionBitmap::open(bitmap_path) {
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
        bitmap.set_watched(slot);
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
    let mut changed_slots = HashSet::new();
    if let Ok(slot) = usize::try_from(first.slot_index()) {
        changed_slots.insert(slot);
    }
    tokio::select! {
        _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_millis(debounce_ms)) => {}
    }
    while let Ok(event) = event_rx.try_recv() {
        if let Ok(slot) = usize::try_from(event.slot_index()) {
            changed_slots.insert(slot);
        }
    }
    if changed_slots.is_empty() {
        return;
    }
    let clients = hub.clients_watching(&changed_slots);
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
async fn push_homepage_data(hub: &Arc<WsHub>, client_id: &str) {
    let points = {
        let Some(handle) = hub.clients.get(client_id) else {
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
    hub.send_to(client_id, msg);
}

async fn push_rule_data(hub: &Arc<WsHub>, client_id: &str, rule_id: i64) {
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

            hub.send_to(client_id, msg);
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
    let mut rx = hub.register(client_id.clone(), data_type_ws);

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

    if ws_sender.send(Message::Text(welcome.into())).await.is_err() {
        hub.deregister(&client_id);
        return;
    }

    info!("WS client connected: {}", client_id);

    // Forward from channel to WebSocket.
    // WS_PING_SENTINEL is converted to a protocol-level Ping frame so the
    // browser handles it transparently without triggering application logic.
    let client_id_send = client_id.clone();
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let frame = if msg == WS_PING_SENTINEL {
                Message::Ping(bytes::Bytes::new())
            } else {
                Message::Text(msg.into())
            };
            if ws_sender.send(frame).await.is_err() {
                break;
            }
        }
        let _ = ws_sender.close().await;
        client_id_send
    });

    // Handle incoming messages
    let hub_recv = hub.clone();
    let client_id_recv = client_id.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    hub_recv.update_activity(&client_id_recv);
                    handle_client_message(&hub_recv, &client_id_recv, &text).await;
                },
                Message::Ping(data) => {
                    // Axum handles pong automatically, just update activity
                    hub_recv.update_activity(&client_id_recv);
                    let _ = data;
                },
                Message::Close(_) => break,
                _ => {},
            }
        }
        client_id_recv
    });

    tokio::select! {
        _ = &mut send_task => { recv_task.abort(); }
        _ = &mut recv_task => { send_task.abort(); }
    }

    hub.deregister(&client_id);
}

async fn handle_client_message(hub: &WsHub, client_id: &str, text: &str) {
    let data: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            let err = error_msg("INVALID_JSON", "Invalid JSON format", None);
            hub.send_to(client_id, err);
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
            hub.send_to(client_id, pong);
        },

        "subscribe" => {
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
                client_id,
                source,
                channels,
                data_types,
                interval_ms,
                homepage_points,
            );
            hub.send_to(client_id, ack.to_string());
        },

        "unsubscribe" => {
            let source = data["data"]["source"].as_str().unwrap_or("inst");
            hub.update_subscription(
                client_id,
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
            hub.send_to(client_id, ack);
        },

        "control" => {
            handle_control(hub, client_id, &data).await;
        },

        _ => {
            debug!("Unknown WS message type '{}' from {}", msg_type, client_id);
        },
    }
}

async fn handle_control(hub: &WsHub, client_id: &str, data: &Value) {
    let control = &data["data"];
    let channel_id = control["channel_id"].as_i64();
    let point_id = control["point_id"].as_i64();
    let command_type = control["command_type"].as_str();
    let value = &control["value"];

    let (Some(channel_id), Some(point_id), Some(_command_type)) =
        (channel_id, point_id, command_type)
    else {
        let err = error_msg(
            "CONTROL_ERROR",
            "Missing required control parameters",
            data["id"].as_str(),
        );
        hub.send_to(client_id, err);
        return;
    };

    if value.is_null() {
        let err = error_msg(
            "CONTROL_ERROR",
            "Missing required control parameters",
            data["id"].as_str(),
        );
        hub.send_to(client_id, err);
        return;
    }

    warn!(
        "Rejected legacy WS control request from {} for ch{} pt{} command_type={}",
        client_id,
        channel_id,
        point_id,
        command_type.unwrap_or("<missing>")
    );
    hub.send_to(client_id, control_unsupported_msg(data["id"].as_str()));
}

const CONTROL_UNSUPPORTED_MESSAGE: &str =
    "WebSocket control is disabled; use automation execute_action or io channel control API";

fn control_unsupported_msg(request_id: Option<&str>) -> String {
    error_msg(
        "CONTROL_UNSUPPORTED",
        CONTROL_UNSUPPORTED_MESSAGE,
        request_id,
    )
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

    #[test]
    fn control_unsupported_msg_returns_error_frame() {
        let msg = control_unsupported_msg(Some("cmd-1"));
        let parsed: Value = serde_json::from_str(&msg).expect("valid json");

        assert_eq!(parsed["type"], "error");
        assert_eq!(parsed["data"]["code"], "CONTROL_UNSUPPORTED");
        assert_eq!(parsed["data"]["message"], CONTROL_UNSUPPORTED_MESSAGE);
        assert_eq!(parsed["data"]["request_id"], "cmd-1");
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
