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
//!     "timestamp_format": "unix_millis"
//!   }
//! }
//! ```

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::core::channels::RuntimeChannelConfig;
use crate::protocols::ChannelRuntime;
use crate::protocols::adapters::json_mapper::{JsonMapper, JsonMappingConfig};
use crate::protocols::core::data::DataBatch;
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
    data_event_channel,
};

/// MQTT subscription configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MqttSubscription {
    /// Topic pattern (supports wildcards: +, #)
    topic: String,
    /// Quality of Service (0, 1, or 2)
    #[serde(default = "default_qos")]
    qos: u8,
}

fn default_qos() -> u8 {
    1
}

/// MQTT channel parameters (from database config JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MqttParamsConfig {
    /// Broker URL (e.g., "tcp://localhost:1883" or "ssl://broker.example.com:8883")
    broker: String,

    /// Client ID (should be unique per channel)
    #[serde(default = "default_client_id")]
    client_id: String,

    /// Username for authentication (optional)
    #[serde(default)]
    username: Option<String>,

    /// Password for authentication (optional)
    #[serde(default)]
    password: Option<String>,

    /// Topics to subscribe
    #[serde(default)]
    subscriptions: Vec<MqttSubscription>,

    /// JSON mapping configuration
    #[serde(default)]
    json_mapping: JsonMappingConfig,

    /// Keep-alive interval in seconds
    #[serde(default = "default_keep_alive")]
    keep_alive_secs: u64,
}

fn default_client_id() -> String {
    format!("io_{}", uuid::Uuid::new_v4().as_simple())
}

fn default_keep_alive() -> u64 {
    30
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
        }
    }
}

impl MqttParamsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        self.broker_endpoint()?;
        if self.client_id.trim().is_empty() {
            return Err(GatewayError::Config(
                "MQTT client_id must be a non-empty string".to_string(),
            ));
        }
        if self.keep_alive_secs == 0 || self.keep_alive_secs > u16::MAX.into() {
            return Err(GatewayError::Config(
                "MQTT keep_alive_secs must be within 1..=65535".to_string(),
            ));
        }
        if self.username.is_some() != self.password.is_some() {
            return Err(GatewayError::Config(
                "MQTT username and password must be configured together".to_string(),
            ));
        }
        for subscription in &self.subscriptions {
            if subscription.topic.trim().is_empty() {
                return Err(GatewayError::Config(
                    "MQTT subscription topic must be a non-empty string".to_string(),
                ));
            }
            if subscription.qos > 2 {
                return Err(GatewayError::Config(format!(
                    "MQTT subscription QoS {} is outside 0..=2",
                    subscription.qos
                )));
            }
        }
        Ok(())
    }

    fn broker_endpoint(&self) -> Result<(String, u16)> {
        let parsed = url::Url::parse(self.broker.trim())
            .map_err(|error| GatewayError::Config(format!("Invalid MQTT broker URL: {error}")))?;
        if !matches!(parsed.scheme(), "tcp" | "mqtt") {
            return Err(GatewayError::Config(
                "MQTT broker must use the tcp:// or mqtt:// scheme; TLS is not configured for IO channels"
                    .to_string(),
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(GatewayError::Config(
                "MQTT broker credentials must use the username/password fields".to_string(),
            ));
        }
        if !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(GatewayError::Config(
                "MQTT broker URL must contain only a host and optional port".to_string(),
            ));
        }
        let host = parsed
            .host_str()
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| GatewayError::Config("MQTT broker URL has no host".to_string()))?;
        let port = parsed.port().unwrap_or(1883);
        if port == 0 {
            return Err(GatewayError::Config(
                "MQTT broker port must be greater than zero".to_string(),
            ));
        }
        Ok((host.to_string(), port))
    }
}

/// MQTT Channel implementation
///
/// Event-driven channel that subscribes to MQTT topics and extracts
/// data points from JSON payloads using JSONPath mappings.
pub(crate) struct MqttChannel {
    /// Channel configuration
    config: MqttParamsConfig,
    /// Channel ID
    channel_id: u32,
    /// JSON mapper compiled from the immutable runtime snapshot
    mapper: Arc<JsonMapper>,
    /// MQTT client handle
    client: Option<AsyncClient>,
    /// Event loop established by connect() and started by start_events().
    event_loop: Mutex<Option<EventLoop>>,
    /// Event loop task handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection state
    state: Arc<AtomicU8>,
    /// Event sender for the unified channel task.
    event_tx: DataEventSender,
    /// Sole event receiver, taken once by the unified channel task.
    event_rx: Option<DataEventReceiver>,
    /// Diagnostics
    diagnostics: Arc<AtomicDiagnostics>,
}

impl MqttChannel {
    /// Create a new MQTT channel from one complete runtime snapshot.
    pub(crate) fn new(config: MqttParamsConfig, runtime: &RuntimeChannelConfig) -> Result<Self> {
        config.validate()?;
        let (event_tx, event_rx) = data_event_channel();
        let mapper =
            Arc::new(JsonMapper::from_runtime_config(runtime)?.with_config(&config.json_mapping)?);
        if !mapper.is_empty() && config.subscriptions.is_empty() {
            return Err(GatewayError::Config(
                "MQTT channels with point mappings require at least one subscription".to_string(),
            ));
        }

        info!(
            channel_id = runtime.id(),
            mapping_count = mapper.len(),
            "Compiled MQTT JSON mappings"
        );

        Ok(Self {
            config,
            channel_id: runtime.id(),
            mapper,
            client: None,
            event_loop: Mutex::new(None),
            event_loop_handle: None,
            state: Arc::new(AtomicU8::new(ConnectionState::Disconnected as u8)),
            event_tx,
            event_rx: Some(event_rx),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
        })
    }

    /// Set connection state and queue an event.
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
        let _ = self.event_tx.try_send(DataEvent::ConnectionChanged(state));
    }

    /// Create MQTT options
    fn create_options(&self) -> Result<MqttOptions> {
        let (host, port) = self.config.broker_endpoint()?;

        let mut opts = MqttOptions::new(&self.config.client_id, host, port);
        opts.set_keep_alive(Duration::from_secs(self.config.keep_alive_secs));
        // Note: rumqttc doesn't have set_connection_timeout, using keep_alive for liveness

        // Set credentials if provided
        if let (Some(user), Some(pass)) = (&self.config.username, &self.config.password) {
            opts.set_credentials(user, pass);
        }

        Ok(opts)
    }

    /// Subscribe to configured topics
    async fn subscribe_topics(&self, client: &AsyncClient) -> Result<()> {
        for sub in &self.config.subscriptions {
            let qos = match sub.qos {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                2 => QoS::ExactlyOnce,
                value => {
                    return Err(GatewayError::Config(format!(
                        "MQTT subscription QoS {value} is outside 0..=2"
                    )));
                },
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
                                let _ = event_tx.try_send(DataEvent::DataUpdate(batch));
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
                    let _ =
                        event_tx.try_send(DataEvent::ConnectionChanged(ConnectionState::Connected));
                    info!(channel_id, "MQTT connected");
                },
                Ok(Event::Incoming(Packet::Disconnect)) => {
                    state.store(ConnectionState::Disconnected as u8, Ordering::SeqCst);
                    let _ = event_tx
                        .try_send(DataEvent::ConnectionChanged(ConnectionState::Disconnected));
                    info!(channel_id, "MQTT disconnected");
                    break;
                },
                Ok(Event::Incoming(Packet::PingResp)) => {
                    let _ = event_tx.try_send(DataEvent::Heartbeat);
                },
                Ok(_) => {
                    // Ignore other events
                },
                Err(e) => {
                    error!(channel_id, error = %e, "MQTT connection error");
                    state.store(ConnectionState::Error as u8, Ordering::SeqCst);
                    let _ = event_tx.try_send(DataEvent::ConnectionChanged(ConnectionState::Error));
                    let _ = event_tx.try_send(DataEvent::Error(e.to_string()));
                    diagnostics.record_error(e.to_string());
                    break;
                },
            }
        }
    }
}

#[async_trait]
impl ChannelRuntime for MqttChannel {
    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        let event_loop_running = self
            .event_loop_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished());
        if self.client.is_some()
            && (self.event_loop.get_mut().is_some() || event_loop_running)
            && matches!(
                self.connection_state(),
                ConnectionState::Connecting | ConnectionState::Connected
            )
        {
            return Ok(());
        }
        if let Some(handle) = self.event_loop_handle.take() {
            handle.abort();
        }
        if let Some(client) = self.client.take() {
            let _ = client.disconnect().await;
        }
        *self.event_loop.get_mut() = None;

        self.set_state(ConnectionState::Connecting);

        // Create MQTT client
        let opts = self.create_options()?;
        let (client, event_loop) = AsyncClient::new(opts, 100);

        // Subscribe to topics
        self.subscribe_topics(&client).await?;

        // Store the transport generation. The unified channel lifecycle starts
        // its event stream only after connect() succeeds.
        self.client = Some(client);
        *self.event_loop.get_mut() = Some(event_loop);

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
        *self.event_loop.get_mut() = None;

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

    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        self.event_rx.take()
    }

    async fn start_events(&mut self) -> Result<()> {
        if self
            .event_loop_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Ok(());
        }
        if let Some(handle) = self.event_loop_handle.take() {
            handle.abort();
        }
        let event_loop = self
            .event_loop
            .get_mut()
            .take()
            .ok_or(GatewayError::NotConnected)?;
        let mapper = Arc::clone(&self.mapper);
        let channel_id = self.channel_id;
        let state = Arc::clone(&self.state);
        let event_tx = self.event_tx.clone();
        let diagnostics = Arc::clone(&self.diagnostics);

        self.event_loop_handle = Some(tokio::spawn(async move {
            Self::run_event_loop(event_loop, channel_id, state, event_tx, mapper, diagnostics)
                .await;
        }));
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        if let Some(handle) = self.event_loop_handle.take() {
            handle.abort();
        }
        Ok(())
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
            .field("broker", &self.config.broker)
            .field("state", &self.connection_state())
            .finish()
    }
}

use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

impl HasMetadata for MqttChannel {
    #[allow(clippy::disallowed_methods)]
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "mqtt",
            display_name: "MQTT JSON",
            description: "Event-driven MQTT subscriber with point-owned JSONPath mappings.",
            is_recommended: true,
            example_config: serde_json::json!({
                "broker": "tcp://192.168.1.50:1883",
                "client_id": "aether-io",
                "subscriptions": [{"topic": "device/+/telemetry", "qos": 1}],
                "keep_alive_secs": 30
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "broker",
                    "Broker",
                    "MQTT broker URL using the tcp:// or mqtt:// scheme",
                    ParameterType::String,
                )
                .with_min_length(1),
                ParameterMetadata::optional(
                    "client_id",
                    "Client ID",
                    "MQTT client identifier; generated when omitted",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "username",
                    "Username",
                    "Broker username; password must be configured with it",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "password",
                    "Password",
                    "Broker password; username must be configured with it",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "subscriptions",
                    "Subscriptions",
                    "MQTT topic and QoS subscriptions",
                    ParameterType::Array,
                    serde_json::json!([]),
                ),
                ParameterMetadata::optional(
                    "json_mapping",
                    "JSON Mapping",
                    "Optional source timestamp JSONPath settings",
                    ParameterType::Object,
                    serde_json::json!({}),
                ),
                ParameterMetadata::optional(
                    "keep_alive_secs",
                    "Keep Alive (s)",
                    "MQTT keep-alive interval in seconds",
                    ParameterType::Integer,
                    serde_json::json!(30),
                )
                .with_integer_range(1, u64::from(u16::MAX)),
            ],
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::core::config::{
        ChannelConfig, ChannelCore, ChannelLoggingConfig, Point, TelemetryPoint,
    };
    use std::collections::HashMap;

    fn runtime_snapshot_with_parameters(
        parameters: HashMap<String, serde_json::Value>,
    ) -> RuntimeChannelConfig {
        RuntimeChannelConfig::from_base(ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "test".to_string(),
                description: None,
                protocol: "mqtt".to_string(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        })
    }

    fn runtime_snapshot() -> RuntimeChannelConfig {
        runtime_snapshot_with_parameters(HashMap::new())
    }

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
        let config = MqttParamsConfig {
            broker: "tcp://192.168.1.50:1883".to_string(),
            client_id: "test".to_string(),
            username: None,
            password: None,
            subscriptions: Vec::new(),
            json_mapping: JsonMappingConfig::default(),
            keep_alive_secs: 30,
        };

        let (host, port) = config.broker_endpoint().unwrap();
        assert_eq!(host, "192.168.1.50");
        assert_eq!(port, 1883);
    }

    #[test]
    fn tls_scheme_never_silently_downgrades_to_plaintext() {
        let config = MqttParamsConfig {
            broker: "mqtts://broker.example.com:8883".to_string(),
            client_id: "test".to_string(),
            username: None,
            password: None,
            subscriptions: Vec::new(),
            json_mapping: JsonMappingConfig::default(),
            keep_alive_secs: 30,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn broker_endpoint_is_strict_and_supports_ipv6() {
        let ipv6 = MqttParamsConfig {
            broker: "mqtt://[2001:db8::10]:1884".to_string(),
            ..MqttParamsConfig::default()
        };
        assert_eq!(
            ipv6.broker_endpoint().unwrap(),
            ("[2001:db8::10]".to_string(), 1884)
        );

        for broker in [
            "broker.example.com:1883",
            "http://broker.example.com:1883",
            "mqtts://broker.example.com:8883",
            "mqtt://:1883",
            "mqtt://user:secret@broker.example.com:1883",
            "mqtt://broker.example.com:1883/path",
            "mqtt://broker.example.com:0",
        ] {
            let config = MqttParamsConfig {
                broker: broker.to_string(),
                ..MqttParamsConfig::default()
            };
            assert!(config.validate().is_err(), "unexpectedly accepted {broker}");
        }
    }

    #[test]
    fn retired_adapter_reconnect_fields_are_rejected() {
        for retired in [
            "connect_timeout_ms",
            "max_reconnect_attempts",
            "reconnect_delay_ms",
        ] {
            let mut parameters = serde_json::json!({
                "broker": "tcp://192.168.1.50:1883"
            });
            parameters
                .as_object_mut()
                .unwrap()
                .insert(retired.to_string(), serde_json::json!(5_000));
            assert!(serde_json::from_value::<MqttParamsConfig>(parameters).is_err());
        }
    }

    #[test]
    fn channel_creation_rejects_invalid_subscription_qos() {
        let config = MqttParamsConfig {
            broker: "tcp://192.168.1.50:1883".to_string(),
            client_id: "test".to_string(),
            username: None,
            password: None,
            subscriptions: vec![MqttSubscription {
                topic: "device/data".to_string(),
                qos: 3,
            }],
            json_mapping: JsonMappingConfig::default(),
            keep_alive_secs: 30,
        };

        let error = MqttChannel::new(config, &runtime_snapshot())
            .expect_err("invalid MQTT QoS must fail before connect");
        assert!(error.to_string().contains("outside 0..=2"));
    }

    #[test]
    fn mapped_channel_requires_a_subscription() {
        let mut runtime = runtime_snapshot();
        runtime.telemetry_points.push(TelemetryPoint {
            base: Point {
                point_id: 1,
                signal_name: "temperature".to_string(),
                description: None,
                unit: Some("C".to_string()),
                protocol_mappings: Some(r#"{"json_path":"$.temperature"}"#.to_string()),
            },
            scale: 1.0,
            offset: 0.0,
            data_type: "float64".to_string(),
            reverse: false,
        });

        let error = MqttChannel::new(MqttParamsConfig::default(), &runtime)
            .expect_err("mapped MQTT channels need an input topic");

        assert!(error.to_string().contains("at least one subscription"));
    }
}
