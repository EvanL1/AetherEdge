//! MQTT Protocol Adapter
//!
//! Event-driven data collection from MQTT brokers with JSONPath mapping.
//!
//! ## Design Overview
//!
//! MQTT is a publish-subscribe protocol where:
//! - Devices publish JSON payloads to topics
//! - io subscribes to topics and extracts data points via JSONPath
//!
//! Unlike Modbus/IEC104, MQTT itself doesn't define the data format.
//! Each vendor has their own JSON schema. The JSONPath mapping layer
//! enables configuration-driven device integration.
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "broker": "tcp://192.168.1.50:1883",
//!   "client_id": "io_1001",
//!   "username": "admin",
//!   "password": "secret",
//!   "subscriptions": [{"topic": "device/+/telemetry", "qos": 1}],
//!   "json_mapping": {
//!     "timestamp_path": "$.ts",
//!     "timestamp_format": "unix_ms"
//!   }
//! }
//! ```

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

/// Delay between reconnection attempts in the MQTT event loop
const RECONNECT_BACKOFF_DELAY: Duration = Duration::from_secs(1);

use crate::protocols::ChannelRuntime;
use crate::protocols::core::data::DataBatch;
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::json_mapper::{JsonMapper, JsonMappingConfig};
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
};

/// MQTT subscription configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttSubscription {
    /// Topic pattern (supports wildcards: +, #)
    pub topic: String,
    /// Quality of Service (0, 1, or 2)
    #[serde(default = "default_qos")]
    pub qos: u8,
}

fn default_qos() -> u8 {
    1
}

/// MQTT channel parameters (from database config JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttParamsConfig {
    /// Broker URL (e.g., "tcp://localhost:1883" or "ssl://broker.example.com:8883")
    pub broker: String,

    /// Client ID (should be unique per channel)
    #[serde(default = "default_client_id")]
    pub client_id: String,

    /// Username for authentication (optional)
    #[serde(default)]
    pub username: Option<String>,

    /// Password for authentication (optional)
    #[serde(default)]
    pub password: Option<String>,

    /// Topics to subscribe
    #[serde(default)]
    pub subscriptions: Vec<MqttSubscription>,

    /// JSON mapping configuration
    #[serde(default)]
    pub json_mapping: JsonMappingConfig,

    /// Keep-alive interval in seconds
    #[serde(default = "default_keep_alive")]
    pub keep_alive_secs: u64,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    /// Maximum reconnect attempts (0 = infinite)
    #[serde(default)]
    pub max_reconnect_attempts: u32,

    /// Reconnect delay in milliseconds
    #[serde(default = "default_reconnect_delay_ms")]
    pub reconnect_delay_ms: u64,
}

fn default_client_id() -> String {
    format!("io_{}", uuid::Uuid::new_v4().as_simple())
}

fn default_keep_alive() -> u64 {
    30
}

fn default_connect_timeout_ms() -> u64 {
    5000
}

fn default_reconnect_delay_ms() -> u64 {
    5000
}

impl Default for MqttParamsConfig {
    fn default() -> Self {
        Self {
            broker: "tcp://localhost:1883".to_string(),
            client_id: default_client_id(),
            username: None,
            password: None,
            subscriptions: Vec::new(),
            json_mapping: JsonMappingConfig::default(),
            keep_alive_secs: default_keep_alive(),
            connect_timeout_ms: default_connect_timeout_ms(),
            max_reconnect_attempts: 0,
            reconnect_delay_ms: default_reconnect_delay_ms(),
        }
    }
}

impl MqttParamsConfig {
    /// Convert to runtime configuration
    pub fn to_config(&self) -> MqttConfig {
        MqttConfig {
            broker: self.broker.clone(),
            client_id: self.client_id.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            subscriptions: self.subscriptions.clone(),
            json_mapping: self.json_mapping.clone(),
            keep_alive: Duration::from_secs(self.keep_alive_secs),
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
            max_reconnect_attempts: self.max_reconnect_attempts,
            reconnect_delay: Duration::from_millis(self.reconnect_delay_ms),
        }
    }
}

/// MQTT runtime configuration
#[derive(Debug, Clone)]
pub struct MqttConfig {
    pub broker: String,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub subscriptions: Vec<MqttSubscription>,
    pub json_mapping: JsonMappingConfig,
    pub keep_alive: Duration,
    pub connect_timeout: Duration,
    pub max_reconnect_attempts: u32,
    pub reconnect_delay: Duration,
}

/// MQTT Channel implementation
///
/// Event-driven channel that subscribes to MQTT topics and extracts
/// data points from JSON payloads using JSONPath mappings.
pub struct MqttChannel {
    /// Channel configuration
    config: MqttConfig,
    /// Channel ID
    channel_id: u32,
    /// Channel name
    name: String,
    /// JSON mapper (loaded from database)
    mapper: Option<Arc<JsonMapper>>,
    /// MQTT client handle
    client: Option<AsyncClient>,
    /// Event loop task handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection state
    state: AtomicU8,
    /// Event broadcast sender
    event_tx: DataEventSender,
    /// Diagnostics
    diagnostics: Arc<AtomicDiagnostics>,
    /// Database pool for loading mappings
    db_pool: Option<SqlitePool>,
}

impl MqttChannel {
    /// Create a new MQTT channel
    pub fn new(config: MqttConfig, channel_id: u32, name: String) -> Self {
        let (event_tx, _) = broadcast::channel(1024);

        Self {
            config,
            channel_id,
            name,
            mapper: None,
            client: None,
            event_loop_handle: None,
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            event_tx,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            db_pool: None,
        }
    }

    /// Load JSON mappings from database
    async fn load_mapper(&mut self) -> Result<()> {
        if self.mapper.is_some() {
            return Ok(());
        }

        let pool = self.db_pool.as_ref().ok_or_else(|| {
            GatewayError::Config("Database pool not set for MQTT channel".to_string())
        })?;

        let mapper = JsonMapper::from_database(pool, self.channel_id)
            .await?
            .with_config(&self.config.json_mapping)?;

        info!(
            channel_id = self.channel_id,
            mapping_count = mapper.len(),
            "Loaded MQTT JSON mappings"
        );

        self.mapper = Some(Arc::new(mapper));
        Ok(())
    }

    /// Set connection state and broadcast event
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
        let _ = self.event_tx.send(DataEvent::ConnectionChanged(state));
    }

    /// Parse broker URL into host and port
    fn parse_broker(&self) -> Result<(&str, u16)> {
        let broker = &self.config.broker;

        // Remove scheme prefix
        let without_scheme = broker
            .strip_prefix("tcp://")
            .or_else(|| broker.strip_prefix("mqtt://"))
            .or_else(|| broker.strip_prefix("ssl://"))
            .or_else(|| broker.strip_prefix("mqtts://"))
            .unwrap_or(broker);

        // Split host:port
        let parts: Vec<&str> = without_scheme.split(':').collect();
        let host = parts
            .first()
            .ok_or_else(|| GatewayError::Config(format!("Invalid broker URL: {}", broker)))?;
        let port = parts
            .get(1)
            .map(|p| p.parse::<u16>())
            .transpose()
            .map_err(|_| GatewayError::Config(format!("Invalid port in broker URL: {}", broker)))?
            .unwrap_or(1883);

        Ok((host, port))
    }

    /// Create MQTT options
    fn create_options(&self) -> Result<MqttOptions> {
        let (host, port) = self.parse_broker()?;

        let mut opts = MqttOptions::new(&self.config.client_id, host, port);
        opts.set_keep_alive(self.config.keep_alive);
        // Note: rumqttc doesn't have set_connection_timeout, using keep_alive for liveness

        // Set credentials if provided
        if let (Some(user), Some(pass)) = (&self.config.username, &self.config.password) {
            opts.set_credentials(user, pass);
        }

        // Never turn an explicitly secure URL into a plaintext connection.
        // TLS requires certificate configuration that this channel adapter does
        // not expose yet; the uplink service provides the audited TLS path.
        if self.config.broker.starts_with("ssl://") || self.config.broker.starts_with("mqtts://") {
            return Err(GatewayError::Unsupported(
                "MQTT TLS is not configured for I/O channels; use tcp:// on a trusted field \
                 network or the certificate-backed uplink adapter"
                    .to_string(),
            ));
        }

        Ok(opts)
    }

    /// Subscribe to configured topics
    async fn subscribe_topics(&self, client: &AsyncClient) -> Result<()> {
        for sub in &self.config.subscriptions {
            let qos = match sub.qos {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                _ => QoS::ExactlyOnce,
            };

            client
                .subscribe(&sub.topic, qos)
                .await
                .map_err(|e| GatewayError::Protocol(format!("Subscribe failed: {e}")))?;

            debug!(
                channel_id = self.channel_id,
                topic = %sub.topic,
                qos = sub.qos,
                "Subscribed to MQTT topic"
            );
        }

        Ok(())
    }

    /// Run the MQTT event loop
    async fn run_event_loop(
        mut event_loop: EventLoop,
        channel_id: u32,
        state: Arc<AtomicU8>,
        event_tx: DataEventSender,
        mapper: Arc<JsonMapper>,
        diagnostics: Arc<AtomicDiagnostics>,
    ) {
        info!(channel_id, "MQTT event loop started");

        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    // Process incoming message
                    let topic = &publish.topic;
                    let payload = &publish.payload;

                    match mapper.parse(payload) {
                        Ok(batch) => {
                            if !batch.is_empty() {
                                let count = batch.len();
                                diagnostics.add_read(count as u64);
                                let _ = event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));
                                debug!(
                                    channel_id,
                                    topic = %topic,
                                    points = count,
                                    "Processed MQTT message"
                                );
                            }
                        },
                        Err(e) => {
                            diagnostics.record_error(e.to_string());
                            debug!(
                                channel_id,
                                topic = %topic,
                                error = %e,
                                "Failed to parse MQTT message"
                            );
                        },
                    }
                },
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    state.store(ConnectionState::Connected as u8, Ordering::SeqCst);
                    let _ = event_tx.send(DataEvent::ConnectionChanged(ConnectionState::Connected));
                    info!(channel_id, "MQTT connected");
                },
                Ok(Event::Incoming(Packet::Disconnect)) => {
                    state.store(ConnectionState::Disconnected as u8, Ordering::SeqCst);
                    let _ =
                        event_tx.send(DataEvent::ConnectionChanged(ConnectionState::Disconnected));
                    info!(channel_id, "MQTT disconnected");
                },
                Ok(Event::Incoming(Packet::PingResp)) => {
                    let _ = event_tx.send(DataEvent::Heartbeat);
                },
                Ok(_) => {
                    // Ignore other events
                },
                Err(e) => {
                    error!(channel_id, error = %e, "MQTT connection error");
                    state.store(ConnectionState::Reconnecting as u8, Ordering::SeqCst);
                    let _ =
                        event_tx.send(DataEvent::ConnectionChanged(ConnectionState::Reconnecting));
                    let _ = event_tx.send(DataEvent::Error(e.to_string()));
                    diagnostics.record_error(e.to_string());

                    // rumqttc will automatically attempt reconnection
                    tokio::time::sleep(RECONNECT_BACKOFF_DELAY).await;
                },
            }
        }
    }
}

#[async_trait]
impl ChannelRuntime for MqttChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "mqtt"
    }

    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        if self.client.is_some() {
            return Ok(());
        }

        self.set_state(ConnectionState::Connecting);

        // Load JSON mappings if not already loaded
        self.load_mapper().await?;

        // Create MQTT client
        let opts = self.create_options()?;
        let (client, event_loop) = AsyncClient::new(opts, 100);

        // Subscribe to topics
        self.subscribe_topics(&client).await?;

        // Store client
        self.client = Some(client);

        // Spawn event loop task
        let mapper = self
            .mapper
            .clone()
            .ok_or_else(|| GatewayError::Config("MQTT mapper not loaded".into()))?;
        let channel_id = self.channel_id;
        let state = Arc::new(AtomicU8::new(ConnectionState::Connecting as u8));
        let state_clone = state.clone();
        let event_tx = self.event_tx.clone();
        let diagnostics = self.diagnostics.clone();

        let handle = tokio::spawn(async move {
            Self::run_event_loop(
                event_loop,
                channel_id,
                state_clone,
                event_tx,
                mapper,
                diagnostics,
            )
            .await;
        });

        self.event_loop_handle = Some(handle);

        info!(
            channel_id = self.channel_id,
            broker = %self.config.broker,
            "MQTT channel connecting"
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Abort event loop
        if let Some(handle) = self.event_loop_handle.take() {
            handle.abort();
        }

        // Disconnect client
        if let Some(client) = self.client.take() {
            let _ = client.disconnect().await;
        }

        self.set_state(ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "MQTT channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        // Event-driven protocol - return empty batch
        // Data is delivered via subscribe()
        PollResult::success(DataBatch::new())
    }

    async fn write_control(&mut self, _commands: &[(u32, f64)]) -> Result<usize> {
        // MQTT is typically read-only for data collection
        // Control would require publishing to device-specific topics
        Err(GatewayError::Protocol(
            "MQTT channel does not support control commands".to_string(),
        ))
    }

    async fn write_adjustment(&mut self, _adjustments: &[(u32, f64)]) -> Result<usize> {
        Err(GatewayError::Protocol(
            "MQTT channel does not support adjustment commands".to_string(),
        ))
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(self.event_tx.subscribe())
    }

    async fn start_events(&mut self) -> Result<()> {
        // Connect if not already connected
        if self.client.is_none() {
            self.connect().await?;
        }
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        self.disconnect().await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "mqtt".to_string(),
            connection_state: self.connection_state(),
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: Default::default(),
        })
    }

    fn connection_state(&self) -> ConnectionState {
        ConnectionState::from(self.state.load(Ordering::SeqCst))
    }
}

impl std::fmt::Debug for MqttChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MqttChannel")
            .field("channel_id", &self.channel_id)
            .field("name", &self.name)
            .field("broker", &self.config.broker)
            .field("state", &self.connection_state())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_mqtt_params_default() {
        let params = MqttParamsConfig::default();
        assert_eq!(params.broker, "tcp://localhost:1883");
        assert!(params.username.is_none());
        assert!(params.subscriptions.is_empty());
    }

    #[test]
    fn test_mqtt_params_deserialize() {
        let json = r#"{
            "broker": "tcp://192.168.1.50:1883",
            "client_id": "test_client",
            "username": "admin",
            "password": "secret",
            "subscriptions": [
                {"topic": "device/+/telemetry", "qos": 1}
            ]
        }"#;

        let params: MqttParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.broker, "tcp://192.168.1.50:1883");
        assert_eq!(params.client_id, "test_client");
        assert_eq!(params.username, Some("admin".to_string()));
        assert_eq!(params.subscriptions.len(), 1);
        assert_eq!(params.subscriptions[0].topic, "device/+/telemetry");
    }

    #[test]
    fn test_parse_broker_url() {
        let config = MqttConfig {
            broker: "tcp://192.168.1.50:1883".to_string(),
            client_id: "test".to_string(),
            username: None,
            password: None,
            subscriptions: Vec::new(),
            json_mapping: JsonMappingConfig::default(),
            keep_alive: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            max_reconnect_attempts: 0,
            reconnect_delay: Duration::from_secs(5),
        };

        let channel = MqttChannel::new(config, 1, "test".to_string());
        let (host, port) = channel.parse_broker().unwrap();
        assert_eq!(host, "192.168.1.50");
        assert_eq!(port, 1883);
    }

    #[test]
    fn tls_scheme_never_silently_downgrades_to_plaintext() {
        let config = MqttConfig {
            broker: "mqtts://broker.example.com:8883".to_string(),
            client_id: "test".to_string(),
            username: None,
            password: None,
            subscriptions: Vec::new(),
            json_mapping: JsonMappingConfig::default(),
            keep_alive: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            max_reconnect_attempts: 0,
            reconnect_delay: Duration::from_secs(5),
        };

        let channel = MqttChannel::new(config, 1, "test".to_string());
        assert!(channel.create_options().is_err());
    }
}
