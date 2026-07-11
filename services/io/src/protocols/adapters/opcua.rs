//! OPC UA protocol adapter.
//!
//! This module provides the `OpcUaChannel` adapter that integrates
//! `async-opcua` with the protocol layer's `Protocol`, `ProtocolClient`, and `EventDrivenProtocol` traits.
//!
//! OPC UA supports both polling and subscription (event-driven) modes.
//! This adapter primarily uses the subscription mode for real-time data updates.
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::protocols::prelude::*;
//! use crate::protocols::adapters::opcua::{OpcUaChannel, OpcUaChannelConfig, SubscriptionConfig};
//!
//! let config = OpcUaChannelConfig::new("opc.tcp://192.168.1.100:4840")
//!     .with_subscription(SubscriptionConfig::default())
//!     .with_points(points);
//!
//! let mut channel = OpcUaChannel::new(config, store, 1);
//! channel.connect().await?;
//! channel.start_polling(PollingConfig::default()).await?;
//!
//! // Receive events via subscription
//! let mut rx = channel.subscribe();
//! while let Some(event) = rx.recv().await {
//!     match event {
//!         DataEvent::DataUpdate(batch) => { /* process data */ }
//!         _ => {}
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use opcua::client::{ClientBuilder, DataChangeCallback, IdentityToken, MonitoredItem, Session};
use opcua::crypto::SecurityPolicy;
use opcua::types::{
    AttributeId, DataValue, Identifier, MessageSecurityMode, MonitoredItemCreateRequest, NodeId,
    StatusCode, TimestampsToReturn, UAString, UserTokenPolicy, Variant, WriteValue,
};
use tokio::sync::broadcast;

use async_trait::async_trait;

// ============================================================================
// Default timeout constants for OPC UA
// ============================================================================
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_SESSION_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::{PointConfig, ProtocolAddress};
use crate::protocols::core::quality::Quality;
use crate::protocols::core::traits::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, DataEvent,
    DataEventHandler, DataEventReceiver, DataEventSender, Diagnostics, EventDrivenProtocol,
    PollResult, Protocol, ProtocolCapabilities, ProtocolClient, WriteResult,
};
use crate::protocols::gateway::ChannelRuntime;
use aether_model::PointType;

/// OPC UA security policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpcUaSecurityPolicy {
    /// No security (development/testing only).
    #[default]
    None,
    /// Basic128Rsa15.
    Basic128Rsa15,
    /// Basic256.
    Basic256,
    /// Basic256Sha256.
    Basic256Sha256,
    /// Aes128Sha256RsaOaep.
    Aes128Sha256RsaOaep,
    /// Aes256Sha256RsaPss.
    Aes256Sha256RsaPss,
}

impl OpcUaSecurityPolicy {
    /// Get the security policy URI string.
    fn to_uri(self) -> &'static str {
        match self {
            Self::None => SecurityPolicy::None.to_uri(),
            Self::Basic128Rsa15 => SecurityPolicy::Basic128Rsa15.to_uri(),
            Self::Basic256 => SecurityPolicy::Basic256.to_uri(),
            Self::Basic256Sha256 => SecurityPolicy::Basic256Sha256.to_uri(),
            Self::Aes128Sha256RsaOaep => SecurityPolicy::Aes128Sha256RsaOaep.to_uri(),
            Self::Aes256Sha256RsaPss => SecurityPolicy::Aes256Sha256RsaPss.to_uri(),
        }
    }
}

/// OPC UA message security mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpcUaMessageSecurityMode {
    /// No security.
    #[default]
    None,
    /// Sign only.
    Sign,
    /// Sign and encrypt.
    SignAndEncrypt,
}

impl OpcUaMessageSecurityMode {
    /// Convert to opcua MessageSecurityMode.
    fn to_message_security_mode(self) -> MessageSecurityMode {
        match self {
            Self::None => MessageSecurityMode::None,
            Self::Sign => MessageSecurityMode::Sign,
            Self::SignAndEncrypt => MessageSecurityMode::SignAndEncrypt,
        }
    }
}

/// OPC UA identity/authentication configuration.
#[derive(Debug, Clone, Default)]
pub enum OpcUaIdentity {
    /// Anonymous authentication.
    #[default]
    Anonymous,
    /// Username/password authentication.
    UserName {
        /// Username.
        username: String,
        /// Password.
        password: String,
    },
}

impl OpcUaIdentity {
    /// Convert to opcua IdentityToken.
    fn to_identity_token(&self) -> IdentityToken {
        match self {
            Self::Anonymous => IdentityToken::Anonymous,
            Self::UserName { username, password } => {
                IdentityToken::UserName(username.clone(), password.clone())
            },
        }
    }
}

/// Enforce the temporary boundary documented for RUSTSEC-2023-0071.
///
/// `async-opcua` currently brings in an RSA implementation with a known timing
/// side channel. The dependency is retained so anonymous, unsecured field
/// devices remain usable, but every path that would create or use RSA private
/// key material is rejected before the client is built.
fn validate_dependency_exception(config: &OpcUaChannelConfig) -> Result<()> {
    if config.security_policy != OpcUaSecurityPolicy::None
        || config.message_security_mode != OpcUaMessageSecurityMode::None
    {
        return Err(GatewayError::Unsupported(
            "OPC UA signing and encryption are temporarily disabled because the upstream RSA \
             dependency is affected by RUSTSEC-2023-0071"
                .to_string(),
        ));
    }

    if !matches!(&config.identity, OpcUaIdentity::Anonymous) {
        return Err(GatewayError::Unsupported(
            "OPC UA username/password authentication is disabled while only unsecured \
             sessions are permitted; use anonymous access or a separately audited adapter"
                .to_string(),
        ));
    }

    Ok(())
}

/// Subscription configuration for OPC UA.
#[derive(Debug, Clone)]
pub struct SubscriptionConfig {
    /// Publishing interval in milliseconds.
    pub publishing_interval_ms: u64,

    /// Lifetime count (multiplier of publishing interval).
    pub lifetime_count: u32,

    /// Keep-alive count.
    pub keep_alive_count: u32,

    /// Maximum notifications per publish (0 = unlimited).
    pub max_notifications_per_publish: u32,

    /// Priority (0-255).
    pub priority: u8,

    /// Whether publishing is enabled.
    pub publishing_enabled: bool,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            publishing_interval_ms: 1000,
            lifetime_count: 30,
            keep_alive_count: 10,
            max_notifications_per_publish: 0,
            priority: 0,
            publishing_enabled: true,
        }
    }
}

/// Monitored item configuration.
#[derive(Debug, Clone)]
pub struct MonitoredItemConfig {
    /// Sampling interval in milliseconds (-1 = use subscription publishing interval).
    pub sampling_interval_ms: i64,

    /// Queue size for buffering values.
    pub queue_size: u32,

    /// Whether to discard oldest values when queue is full.
    pub discard_oldest: bool,

    /// Deadband for data change filtering (optional).
    pub deadband: Option<f64>,
}

impl Default for MonitoredItemConfig {
    fn default() -> Self {
        Self {
            sampling_interval_ms: -1,
            queue_size: 10,
            discard_oldest: true,
            deadband: None,
        }
    }
}

/// OPC UA channel configuration.
#[derive(Debug, Clone)]
pub struct OpcUaChannelConfig {
    /// Server endpoint URL (e.g., "opc.tcp://192.168.1.100:4840").
    pub endpoint_url: String,

    /// Application name.
    pub application_name: String,

    /// Application URI.
    pub application_uri: String,

    /// Security policy.
    pub security_policy: OpcUaSecurityPolicy,

    /// Message security mode.
    pub message_security_mode: OpcUaMessageSecurityMode,

    /// Identity/authentication.
    pub identity: OpcUaIdentity,

    /// Connection timeout.
    pub connect_timeout: Duration,

    /// Session timeout.
    pub session_timeout: Duration,

    /// Request timeout.
    pub request_timeout: Duration,

    /// Subscription configuration.
    pub subscription: SubscriptionConfig,

    /// Monitored item configuration.
    pub monitored_item: MonitoredItemConfig,

    /// Whether to automatically trust server certificates (development only).
    pub trust_server_certs: bool,

    /// Session retry limit.
    pub session_retry_limit: u32,

    /// PKI directory path (for certificate storage).
    pub pki_dir: Option<String>,

    /// Point configurations.
    pub points: Vec<PointConfig>,

    /// NodeID to point_id mapping (built from points).
    /// Split by identifier type for O(1) lookup without format! allocation.
    /// Numeric identifiers (i=123) are most common and get fastest lookup.
    numeric_mapping: HashMap<(u16, u32), u32>,
    /// String identifiers (s=Temperature)
    string_mapping: HashMap<(u16, String), u32>,
    /// Fallback for guid/bytestring identifiers (rarely used)
    other_mapping: HashMap<String, u32>,
    /// Point ID -> index lookup for O(1) access (mirrors iec104 pattern)
    point_index: HashMap<u32, usize>,
}

impl OpcUaChannelConfig {
    /// Create a new configuration with the given endpoint URL.
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            application_name: "Aether OPC UA Client".to_string(),
            application_uri: "urn:aether:opcua:client".to_string(),
            security_policy: OpcUaSecurityPolicy::None,
            message_security_mode: OpcUaMessageSecurityMode::None,
            identity: OpcUaIdentity::Anonymous,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            session_timeout: DEFAULT_SESSION_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            subscription: SubscriptionConfig::default(),
            monitored_item: MonitoredItemConfig::default(),
            trust_server_certs: true,
            session_retry_limit: 3,
            pki_dir: None,
            points: Vec::new(),
            numeric_mapping: HashMap::new(),
            string_mapping: HashMap::new(),
            other_mapping: HashMap::new(),
            point_index: HashMap::new(),
        }
    }

    /// Set application name.
    pub fn with_application_name(mut self, name: impl Into<String>) -> Self {
        self.application_name = name.into();
        self
    }

    /// Set security policy and message security mode.
    pub fn with_security(
        mut self,
        policy: OpcUaSecurityPolicy,
        mode: OpcUaMessageSecurityMode,
    ) -> Self {
        self.security_policy = policy;
        self.message_security_mode = mode;
        self
    }

    /// Set username/password identity.
    pub fn with_user_identity(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.identity = OpcUaIdentity::UserName {
            username: username.into(),
            password: password.into(),
        };
        self
    }

    /// Set connection timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set session timeout.
    pub fn with_session_timeout(mut self, timeout: Duration) -> Self {
        self.session_timeout = timeout;
        self
    }

    /// Set subscription configuration.
    pub fn with_subscription(mut self, config: SubscriptionConfig) -> Self {
        self.subscription = config;
        self
    }

    /// Set monitored item configuration.
    pub fn with_monitored_item(mut self, config: MonitoredItemConfig) -> Self {
        self.monitored_item = config;
        self
    }

    /// Set whether to trust server certificates.
    pub fn with_trust_server_certs(mut self, trust: bool) -> Self {
        self.trust_server_certs = trust;
        self
    }

    /// Add point configurations.
    pub fn with_points(mut self, points: Vec<PointConfig>) -> Self {
        // Build NodeID mapping and point_index from point configs
        for (i, point) in points.iter().enumerate() {
            self.point_index.insert(point.id, i);
            if let ProtocolAddress::OpcUa(addr) = &point.address {
                let ns = addr.namespace_index;
                let node_id = &addr.node_id;

                // Parse identifier type and store in appropriate map
                if let Some(num_str) = node_id.strip_prefix("i=") {
                    // Numeric identifier (most common)
                    if let Ok(num) = num_str.parse::<u32>() {
                        self.numeric_mapping.insert((ns, num), point.id);
                    }
                } else if let Some(s) = node_id.strip_prefix("s=") {
                    // String identifier
                    self.string_mapping.insert((ns, s.to_string()), point.id);
                } else {
                    // Guid or ByteString - use full key format
                    let key = make_node_id_key(ns, node_id);
                    self.other_mapping.insert(key, point.id);
                }
            }
        }
        self.points = points;
        self
    }

    /// Find point config by ID (O(1) lookup).
    pub fn find_point_config(&self, point_id: u32) -> Option<&PointConfig> {
        self.point_index
            .get(&point_id)
            .map(|&idx| &self.points[idx])
    }

    /// Find point_id by NodeId directly (O(1) lookup without allocation for numeric IDs).
    ///
    /// This is the hot-path function called on every data change notification.
    /// Numeric identifiers (most common) use direct HashMap lookup without format!.
    pub fn find_point_id_by_node_id(&self, node_id: &NodeId) -> Option<u32> {
        let ns = node_id.namespace;
        match &node_id.identifier {
            Identifier::Numeric(n) => {
                // Most common case: O(1) lookup, no allocation
                self.numeric_mapping.get(&(ns, *n)).copied()
            },
            Identifier::String(s) => {
                // String lookup - need to convert UAString to String for HashMap key
                self.string_mapping
                    .get(&(ns, s.as_ref().to_string()))
                    .copied()
            },
            Identifier::Guid(g) => {
                // Rare: fallback to formatted key
                let key = format!("ns={};g={}", ns, g);
                self.other_mapping.get(&key).copied()
            },
            Identifier::ByteString(b) => {
                // Rare: fallback to formatted key
                let key = format!("ns={};b={:?}", ns, b.value.as_deref().unwrap_or(&[]));
                self.other_mapping.get(&key).copied()
            },
        }
    }
}

/// OPC UA channel parameters for JSON configuration.
///
/// This is a serde-friendly version of the configuration that can be
/// deserialized from JSON and converted to `OpcUaChannelConfig`.
///
/// # Example JSON
///
/// ```json
/// {
///     "endpoint_url": "opc.tcp://192.168.1.100:4840",
///     "username": "user",
///     "password": "pass",
///     "trust_server_certs": true
/// }
/// ```
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OpcUaParamsConfig {
    /// OPC UA server endpoint URL
    pub endpoint_url: String,

    /// Application name (optional)
    #[serde(default = "default_app_name")]
    pub application_name: String,

    /// Username for authentication (optional, for username/password mode)
    #[serde(default)]
    pub username: Option<String>,

    /// Password for authentication (optional)
    #[serde(default)]
    pub password: Option<String>,

    /// Connection timeout in milliseconds
    #[serde(default = "default_opcua_connect_timeout")]
    pub connect_timeout_ms: u64,

    /// Session timeout in milliseconds
    #[serde(default = "default_session_timeout")]
    pub session_timeout_ms: u64,

    /// Whether to trust server certificates
    #[serde(default = "default_trust_certs")]
    pub trust_server_certs: bool,

    /// Publishing interval in milliseconds for subscription
    #[serde(default = "default_publishing_interval")]
    pub publishing_interval_ms: u64,

    /// Sampling interval in milliseconds for monitored items
    #[serde(default = "default_sampling_interval")]
    pub sampling_interval_ms: u64,
}

fn default_app_name() -> String {
    "Aether OPC UA Client".to_string()
}

fn default_opcua_connect_timeout() -> u64 {
    DEFAULT_CONNECT_TIMEOUT.as_millis() as u64
}

fn default_session_timeout() -> u64 {
    DEFAULT_SESSION_TIMEOUT.as_millis() as u64
}

fn default_trust_certs() -> bool {
    true
}

fn default_publishing_interval() -> u64 {
    1000
}

fn default_sampling_interval() -> u64 {
    500
}

impl OpcUaParamsConfig {
    /// Convert to OpcUaChannelConfig.
    ///
    /// Note: Points must be set separately via `with_points()`.
    pub fn to_config(&self) -> OpcUaChannelConfig {
        let mut config = OpcUaChannelConfig::new(&self.endpoint_url)
            .with_application_name(&self.application_name)
            .with_connect_timeout(Duration::from_millis(self.connect_timeout_ms))
            .with_session_timeout(Duration::from_millis(self.session_timeout_ms))
            .with_trust_server_certs(self.trust_server_certs)
            .with_subscription(SubscriptionConfig {
                publishing_interval_ms: self.publishing_interval_ms,
                ..SubscriptionConfig::default()
            })
            .with_monitored_item(MonitoredItemConfig {
                sampling_interval_ms: self.sampling_interval_ms as i64,
                ..MonitoredItemConfig::default()
            });

        // Set identity based on username/password
        if let (Some(username), Some(password)) = (&self.username, &self.password) {
            config = config.with_user_identity(username, password);
        }

        config
    }
}

/// Create a key for NodeID mapping.
fn make_node_id_key(namespace_index: u16, identifier: &str) -> String {
    format!("ns={};{}", namespace_index, identifier)
}

/// OPC UA channel adapter.
///
/// This struct wraps an `async-opcua` client and implements
/// the protocol layer's `Protocol`, `ProtocolClient`, and `EventDrivenProtocol` traits.
///
/// Note: This adapter follows the "protocol layer separated from storage" design.
/// The channel emits DataEvent::DataUpdate events; the service layer handles persistence.
pub struct OpcUaChannel {
    /// Channel unique identifier.
    channel_id: u32,
    /// Channel instance name.
    name: String,
    /// Configuration.
    config: OpcUaChannelConfig,
    /// opcua session.
    session: Option<Arc<Session>>,
    /// Connection state.
    state: Arc<std::sync::RwLock<ConnectionState>>,
    /// Diagnostics (lock-free).
    diagnostics: Arc<AtomicDiagnostics>,
    /// OPC UA specific: Current subscription ID (0 = none).
    diag_subscription_id: AtomicU32,
    /// OPC UA specific: Number of monitored items.
    diag_monitored_items_count: AtomicUsize,
    /// OPC UA specific: Last data received timestamp (Unix millis, 0 = never).
    diag_last_data_received_ms: Arc<AtomicU64>,
    /// Broadcast sender for event-driven subscribers (multiple subscribers supported).
    event_tx: DataEventSender,
    /// Event handler.
    event_handler: Option<Arc<dyn DataEventHandler>>,
    /// Current subscription ID.
    subscription_id: Option<u32>,
}

impl OpcUaChannel {
    /// Create a new OPC UA channel.
    pub fn new(config: OpcUaChannelConfig, channel_id: u32, name: String) -> Self {
        // Use broadcast channel for multiple subscribers
        let (event_tx, _) = broadcast::channel(1024);

        Self {
            channel_id,
            name,
            config,
            session: None,
            state: Arc::new(std::sync::RwLock::new(ConnectionState::Disconnected)),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            diag_subscription_id: AtomicU32::new(0),
            diag_monitored_items_count: AtomicUsize::new(0),
            diag_last_data_received_ms: Arc::new(AtomicU64::new(0)),
            event_tx,
            event_handler: None,
            subscription_id: None,
        }
    }

    /// Set connection state.
    fn set_state(&self, state: ConnectionState) {
        if let Ok(mut s) = self.state.write() {
            *s = state;
        }
    }

    /// Get connection state.
    fn get_state(&self) -> ConnectionState {
        self.state
            .read()
            .map(|s| *s)
            .unwrap_or(ConnectionState::Error)
    }

    /// Find point config by ID (O(1) via config's point_index).
    fn find_point(&self, id: u32) -> Option<&PointConfig> {
        self.config.find_point_config(id)
    }

    /// Resolve point ID to OPC UA address. Returns error tuple for failures vec.
    fn resolve_opcua_addr(
        &self,
        id: u32,
    ) -> std::result::Result<
        (&PointConfig, &crate::protocols::core::point::OpcUaAddress),
        (u32, String),
    > {
        let point = self
            .find_point(id)
            .ok_or_else(|| (id, "Point not found".to_string()))?;
        match &point.address {
            ProtocolAddress::OpcUa(addr) => Ok((point, addr)),
            _ => Err((id, "Invalid address type".to_string())),
        }
    }

    /// Create subscription and add monitored items.
    pub async fn create_subscription(&mut self) -> Result<u32> {
        let session = self.session.as_ref().ok_or(GatewayError::NotConnected)?;

        let sub_config = &self.config.subscription;

        // Create subscription with data change callback
        // Use Arc to avoid cloning the entire config on each callback invocation
        let event_tx = self.event_tx.clone();
        let diagnostics = self.diagnostics.clone();
        let last_data_received_ms = self.diag_last_data_received_ms.clone();
        let config = Arc::new(self.config.clone()); // Clone once, wrap in Arc
        let event_handler = self.event_handler.clone();

        let subscription_id = session
            .create_subscription(
                Duration::from_millis(sub_config.publishing_interval_ms),
                sub_config.lifetime_count,
                sub_config.keep_alive_count,
                sub_config.max_notifications_per_publish,
                sub_config.priority,
                sub_config.publishing_enabled,
                DataChangeCallback::new(move |data_value: DataValue, item: &MonitoredItem| {
                    // Clone Arc (cheap reference count increment) instead of full config
                    let event_tx = event_tx.clone();
                    let diagnostics = diagnostics.clone();
                    let last_data_received_ms = last_data_received_ms.clone();
                    let config = Arc::clone(&config);
                    let event_handler = event_handler.clone();

                    // Collect the data we need before spawning
                    let node_id = item.item_to_monitor().node_id.clone();
                    // Use stack-allocated array instead of Vec (single element)
                    let item_data = [(node_id, data_value)];

                    tokio::spawn(async move {
                        handle_data_change(
                            &config,
                            &item_data,
                            &event_tx,
                            &diagnostics,
                            &last_data_received_ms,
                            event_handler.as_ref(),
                        )
                        .await;
                    });
                }),
            )
            .await
            .map_err(|e| GatewayError::Protocol(format!("Failed to create subscription: {}", e)))?;

        self.subscription_id = Some(subscription_id);

        // Update diagnostics (lock-free)
        self.diag_subscription_id
            .store(subscription_id, Ordering::Relaxed);

        Ok(subscription_id)
    }

    /// Add monitored items to the subscription.
    pub async fn add_monitored_items(&mut self) -> Result<usize> {
        let session = self.session.as_ref().ok_or(GatewayError::NotConnected)?;

        let subscription_id = self
            .subscription_id
            .ok_or_else(|| GatewayError::Protocol("No active subscription".into()))?;

        // Build monitored item requests with pre-allocated capacity
        let mut items_to_create: Vec<MonitoredItemCreateRequest> =
            Vec::with_capacity(self.config.points.len());
        for point in &self.config.points {
            if let ProtocolAddress::OpcUa(addr) = &point.address {
                let node_id = parse_node_id(&addr.node_id, addr.namespace_index);
                items_to_create.push(node_id.into());
            }
        }

        if items_to_create.is_empty() {
            return Ok(0);
        }

        let count = items_to_create.len();

        // Create monitored items
        session
            .create_monitored_items(subscription_id, TimestampsToReturn::Both, items_to_create)
            .await
            .map_err(|e| {
                GatewayError::Protocol(format!("Failed to create monitored items: {}", e))
            })?;

        // Update diagnostics (lock-free)
        self.diag_monitored_items_count
            .store(count, Ordering::Relaxed);

        Ok(count)
    }

    /// Write node values.
    async fn write_nodes(&self, write_values: &[WriteValue]) -> Result<Vec<StatusCode>> {
        let session = self.session.as_ref().ok_or(GatewayError::NotConnected)?;

        session
            .write(write_values)
            .await
            .map_err(|e| GatewayError::Protocol(format!("Write failed: {}", e)))
    }

    /// Write a single OPC UA value. Returns Ok(()) on success or error message.
    async fn write_single_value(
        &self,
        node_id_str: &str,
        namespace_index: u16,
        variant: Variant,
    ) -> std::result::Result<(), String> {
        let node_id = parse_node_id(node_id_str, namespace_index);
        let write_value = WriteValue {
            node_id,
            attribute_id: AttributeId::Value as u32,
            index_range: UAString::null(),
            value: DataValue::new_now(variant),
        };
        let results = self
            .write_nodes(std::slice::from_ref(&write_value))
            .await
            .map_err(|e| e.to_string())?;
        if results.first().map(|s| s.is_good()).unwrap_or(false) {
            Ok(())
        } else {
            Err(format!("Write failed: {:?}", results.first()))
        }
    }
}

impl ProtocolCapabilities for OpcUaChannel {
    fn name(&self) -> &'static str {
        "OPC UA"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::EventDriven, CommunicationMode::Hybrid]
    }

    fn version(&self) -> &'static str {
        "1.0"
    }
}

impl Protocol for OpcUaChannel {
    fn connection_state(&self) -> ConnectionState {
        self.get_state()
    }

    #[allow(clippy::disallowed_methods)] // json! macro
    async fn diagnostics(&self) -> Result<Diagnostics> {
        let state = self.get_state();

        // Calculate last data received time (lock-free)
        let last_data_received_secs = {
            let ms = self.diag_last_data_received_ms.load(Ordering::Relaxed);
            if ms == 0 {
                None
            } else {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                Some((now_ms.saturating_sub(ms)) / 1000)
            }
        };

        // Get subscription ID (0 = none)
        let subscription_id = match self.diag_subscription_id.load(Ordering::Relaxed) {
            0 => None,
            id => Some(id),
        };

        Ok(Diagnostics {
            protocol: ProtocolCapabilities::name(self).to_string(),
            connection_state: state,
            read_count: self.diagnostics.read_count(),
            write_count: self.diagnostics.write_count(),
            error_count: self.diagnostics.error_count(),
            last_error: self.diagnostics.last_error(),
            extra: serde_json::json!({
                "endpoint_url": self.config.endpoint_url,
                "application_name": self.config.application_name,
                "security_policy": format!("{:?}", self.config.security_policy),
                "subscription_id": subscription_id,
                "monitored_items_count": self.diag_monitored_items_count.load(Ordering::Relaxed),
                "points_configured": self.config.points.len(),
                "last_data_received_secs_ago": last_data_received_secs,
            }),
        })
    }
}

impl ProtocolClient for OpcUaChannel {
    async fn connect(&mut self) -> Result<()> {
        validate_dependency_exception(&self.config)?;
        self.set_state(ConnectionState::Connecting);

        // Build client
        let mut builder = ClientBuilder::new()
            .application_name(&self.config.application_name)
            .application_uri(&self.config.application_uri)
            .session_retry_limit(self.config.session_retry_limit as i32)
            .create_sample_keypair(false);

        if self.config.trust_server_certs {
            builder = builder.trust_server_certs(true);
        }

        if let Some(pki_dir) = &self.config.pki_dir {
            builder = builder.pki_dir(pki_dir);
        }

        let mut client = builder
            .client()
            .map_err(|e| GatewayError::Config(e.join(", ")))?;

        // Prepare endpoint and identity
        let identity = self.config.identity.to_identity_token();
        let security_policy = self.config.security_policy.to_uri();
        let message_mode = self.config.message_security_mode.to_message_security_mode();

        // Connect to server
        let (session, event_loop) = client
            .connect_to_matching_endpoint(
                (
                    self.config.endpoint_url.as_str(),
                    security_policy,
                    message_mode,
                    UserTokenPolicy::anonymous(),
                ),
                identity,
            )
            .await
            .map_err(|e| {
                self.set_state(ConnectionState::Error);
                GatewayError::Connection(e.to_string())
            })?;

        // Wait for connection to be established
        session.wait_for_connection().await;

        self.session = Some(session);
        self.set_state(ConnectionState::Connected);

        // Spawn event loop in background
        let state = self.state.clone();
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            let _handle = event_loop.spawn();

            // Note: The spawned event loop will run until the session is closed.
            // When the connection is lost, the state will be updated via callbacks.
            // For now, we just keep running. In a production implementation,
            // we'd monitor the handle for completion.
            let _ = state;
            let _ = event_tx;
        });

        // Send connection event
        let _ = self
            .event_tx
            .send(DataEvent::ConnectionChanged(ConnectionState::Connected));

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Delete subscription
        if let (Some(session), Some(sub_id)) = (&self.session, self.subscription_id) {
            let _ = session.delete_subscription(sub_id).await;
        }

        // Disconnect session
        if let Some(session) = self.session.take() {
            let _ = session.disconnect().await;
        }

        self.subscription_id = None;
        self.set_state(ConnectionState::Disconnected);

        // Send disconnect event
        let _ = self
            .event_tx
            .send(DataEvent::ConnectionChanged(ConnectionState::Disconnected));

        Ok(())
    }

    async fn write_control(&mut self, commands: &[ControlCommand]) -> Result<WriteResult> {
        let mut success_count = 0;
        let mut failures = Vec::with_capacity(commands.len());

        for cmd in commands {
            let (point, opc_addr) = match self.resolve_opcua_addr(cmd.id) {
                Ok(v) => v,
                Err(e) => {
                    failures.push(e);
                    continue;
                },
            };
            let value = point.transform.apply_bool(cmd.value);
            let (nid, ns) = (opc_addr.node_id.clone(), opc_addr.namespace_index);
            match self
                .write_single_value(&nid, ns, Variant::Boolean(value))
                .await
            {
                Ok(()) => success_count += 1,
                Err(e) => failures.push((cmd.id, e)),
            }
        }

        self.diagnostics.add_write(success_count as u64);
        Ok(WriteResult {
            success_count,
            failures,
        })
    }

    async fn write_adjustment(&mut self, adjustments: &[AdjustmentCommand]) -> Result<WriteResult> {
        let mut success_count = 0;
        let mut failures = Vec::with_capacity(adjustments.len());

        for adj in adjustments {
            let (point, opc_addr) = match self.resolve_opcua_addr(adj.id) {
                Ok(v) => v,
                Err(e) => {
                    failures.push(e);
                    continue;
                },
            };
            let raw_value = match point.transform.reverse_apply(adj.value) {
                Ok(v) => v,
                Err(e) => {
                    failures.push((adj.id, e.to_string()));
                    continue;
                },
            };
            let (nid, ns) = (opc_addr.node_id.clone(), opc_addr.namespace_index);
            match self
                .write_single_value(&nid, ns, Variant::Double(raw_value))
                .await
            {
                Ok(()) => success_count += 1,
                Err(e) => failures.push((adj.id, e)),
            }
        }

        self.diagnostics.add_write(success_count as u64);
        Ok(WriteResult {
            success_count,
            failures,
        })
    }

    async fn poll_once(&mut self) -> PollResult {
        // OPC UA is event-driven via subscriptions.
        // Data changes are received asynchronously through the subscription callback.
        // For poll_once, we check for any pending session events but typically
        // return an empty batch since real data comes through DataChange callbacks.
        //
        // In a true polling scenario, you would call session.poll() here,
        // but the OPC UA SDK handles this internally via async tasks.

        if !self.get_state().is_connected() {
            // Return empty result with no failure tracking (connection-level issue)
            return PollResult::success(DataBatch::new());
        }

        // Return empty batch - data comes through subscription callbacks
        PollResult::success(DataBatch::new())
    }
}

impl EventDrivenProtocol for OpcUaChannel {
    fn subscribe(&self) -> DataEventReceiver {
        // Broadcast channel supports multiple subscribers
        // Each call to subscribe() returns a new receiver that gets all future events
        self.event_tx.subscribe()
    }

    fn set_event_handler(&mut self, handler: Arc<dyn DataEventHandler>) {
        self.event_handler = Some(handler);
    }

    async fn start(&mut self) -> Result<()> {
        // For OPC UA, start means creating a subscription with monitored items
        self.create_subscription().await?;
        self.add_monitored_items().await?;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        // Delete subscription
        if let (Some(session), Some(sub_id)) = (&self.session, self.subscription_id.take()) {
            session
                .delete_subscription(sub_id)
                .await
                .map_err(|e| GatewayError::Protocol(e.to_string()))?;
        }

        // Clear diagnostics (lock-free)
        self.diag_subscription_id.store(0, Ordering::Relaxed);
        self.diag_monitored_items_count.store(0, Ordering::Relaxed);

        Ok(())
    }
}

// ==================== Helper Functions ====================

/// Parse NodeId from string.
fn parse_node_id(identifier: &str, namespace_index: u16) -> NodeId {
    // Support multiple formats:
    // - "i=1234" -> Numeric identifier
    // - "s=Temperature" -> String identifier
    // - Just a string -> String identifier

    if let Some(id_str) = identifier.strip_prefix("i=") {
        if let Ok(id) = id_str.parse::<u32>() {
            return NodeId::new(namespace_index, id);
        }
    } else if let Some(s_str) = identifier.strip_prefix("s=") {
        return NodeId::new(namespace_index, s_str.to_string());
    }

    // Default to string identifier
    NodeId::new(namespace_index, identifier.to_string())
}

/// Handle data change callback.
///
/// Processes OPC UA data change notifications and emits events.
/// The service layer (io) is responsible for persistence.
async fn handle_data_change(
    config: &OpcUaChannelConfig,
    items: &[(NodeId, DataValue)],
    event_tx: &DataEventSender,
    diagnostics: &Arc<AtomicDiagnostics>,
    last_data_received_ms: &Arc<AtomicU64>,
    event_handler: Option<&Arc<dyn DataEventHandler>>,
) {
    let mut batch = DataBatch::new();

    for (node_id, data_value) in items {
        // Find mapped point_id directly from NodeId (O(1), no allocation for numeric IDs)
        let point_id = match config.find_point_id_by_node_id(node_id) {
            Some(id) => id,
            None => continue, // Skip unconfigured nodes
        };

        // Find point config (O(1) via point_index)
        let point_config = config.find_point_config(point_id);

        if let Some(dp) = convert_data_value_with_id(point_id, point_config, data_value) {
            batch.add(dp);
        }
    }

    if batch.is_empty() {
        return;
    }

    // Send event (service layer handles storage)
    // Arc wrap for zero-copy sharing between event_tx and handler
    let batch_arc = Arc::new(batch);
    let _ = event_tx.send(DataEvent::DataUpdate(Arc::clone(&batch_arc)));

    // Call event handler
    if let Some(handler) = event_handler {
        handler.on_data_update(batch_arc).await;
    }

    // Update diagnostics (lock-free)
    diagnostics.inc_read();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    last_data_received_ms.store(now_ms, Ordering::Relaxed);
}

/// Convert DataValue to DataPoint with explicit ID.
fn convert_data_value_with_id(
    point_id: u32,
    config: Option<&PointConfig>,
    dv: &DataValue,
) -> Option<DataPoint> {
    // Get value
    let value = dv.value.as_ref()?;
    let converted_value = convert_variant_to_value(value);

    // Convert quality
    let quality = dv
        .status
        .map(convert_status_code_to_quality)
        .unwrap_or(Quality::Good);

    // Get timestamp
    let timestamp = Utc::now();
    let source_timestamp = dv
        .source_timestamp
        .as_ref()
        .and_then(opcua_datetime_to_chrono);

    // Get point_type from config or default to Telemetry
    let point_type = config
        .map(|cfg| cfg.point_type)
        .unwrap_or(PointType::Telemetry);

    // Apply transform if config is available
    let final_value = if let Some(cfg) = config {
        if let Some(f) = converted_value.as_f64() {
            Value::Float(cfg.transform.apply(f))
        } else if let Some(b) = converted_value.as_bool() {
            Value::Bool(cfg.transform.apply_bool(b))
        } else {
            converted_value
        }
    } else {
        converted_value
    };

    Some(DataPoint {
        id: point_id,
        point_type,
        value: final_value,
        quality,
        timestamp,
        source_timestamp,
    })
}

/// Convert OPC UA Variant to Value.
fn convert_variant_to_value(variant: &Variant) -> Value {
    match variant {
        Variant::Boolean(v) => Value::Bool(*v),
        Variant::SByte(v) => Value::Integer(*v as i64),
        Variant::Byte(v) => Value::Integer(*v as i64),
        Variant::Int16(v) => Value::Integer(*v as i64),
        Variant::UInt16(v) => Value::Integer(*v as i64),
        Variant::Int32(v) => Value::Integer(*v as i64),
        Variant::UInt32(v) => Value::Integer(*v as i64),
        Variant::Int64(v) => Value::Integer(*v),
        Variant::UInt64(v) => Value::Integer(*v as i64),
        Variant::Float(v) => Value::Float(*v as f64),
        Variant::Double(v) => Value::Float(*v),
        Variant::String(v) => Value::String(v.as_ref().to_string()),
        _ => Value::Null,
    }
}

/// Convert OPC UA StatusCode to Quality.
fn convert_status_code_to_quality(status: StatusCode) -> Quality {
    if status.is_good() {
        Quality::Good
    } else if status.is_bad() {
        Quality::Bad
    } else {
        Quality::Uncertain
    }
}

/// Convert OPC UA DateTime to chrono DateTime.
fn opcua_datetime_to_chrono(dt: &opcua::types::DateTime) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(dt.as_chrono().timestamp_millis())
}

// ============================================================================
// HasMetadata Implementation
// ============================================================================

use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

impl HasMetadata for OpcUaChannel {
    #[allow(clippy::disallowed_methods)] // json! macro
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "opcua",
            display_name: "OPC UA",
            description: "OPC UA client for industrial automation data exchange.",
            is_recommended: true,
            example_config: serde_json::json!({
                "endpoint_url": "opc.tcp://192.168.1.100:4840",
                "application_name": "Aether OPC UA Client",
                "username": "user",
                "password": "password",
                "trust_server_certs": true,
                "publishing_interval_ms": 1000,
                "sampling_interval_ms": 500
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "endpoint_url",
                    "Endpoint URL",
                    "OPC UA server endpoint URL (opc.tcp://host:port)",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "application_name",
                    "Application Name",
                    "Client application name for identification",
                    ParameterType::String,
                    serde_json::json!("Aether OPC UA Client"),
                ),
                ParameterMetadata::optional(
                    "username",
                    "Username",
                    "Username for authentication (if using username/password)",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "password",
                    "Password",
                    "Password for authentication",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "Connection timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(30000),
                ),
                ParameterMetadata::optional(
                    "session_timeout_ms",
                    "Session Timeout (ms)",
                    "Session timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(60000),
                ),
                ParameterMetadata::optional(
                    "trust_server_certs",
                    "Trust Server Certs",
                    "Auto-trust server certificates (for development)",
                    ParameterType::Boolean,
                    serde_json::json!(true),
                ),
                ParameterMetadata::optional(
                    "publishing_interval_ms",
                    "Publishing Interval (ms)",
                    "Subscription publishing interval in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(1000),
                ),
                ParameterMetadata::optional(
                    "sampling_interval_ms",
                    "Sampling Interval (ms)",
                    "Monitored item sampling interval in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(500),
                ),
            ],
        }
    }
}

// ============================================================================
// ChannelRuntime implementation (direct, no wrapper needed)
// ============================================================================

#[async_trait]
impl ChannelRuntime for OpcUaChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "opcua"
    }

    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::connect(self).await
    }

    async fn disconnect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::disconnect(self).await
    }

    async fn poll_once(&mut self) -> PollResult {
        <Self as ProtocolClient>::poll_once(self).await
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let cmds: Vec<_> = commands
            .iter()
            .map(|(id, value)| ControlCommand::latching(*id, *value != 0.0))
            .collect();
        let result = <Self as ProtocolClient>::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, value)| AdjustmentCommand::new(*id, *value))
            .collect();
        let result = <Self as ProtocolClient>::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(<Self as EventDrivenProtocol>::subscribe(self))
    }

    async fn start_events(&mut self) -> Result<()> {
        <Self as EventDrivenProtocol>::start(self).await
    }

    async fn stop_events(&mut self) -> Result<()> {
        <Self as EventDrivenProtocol>::stop(self).await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        <Self as Protocol>::diagnostics(self).await
    }

    fn connection_state(&self) -> ConnectionState {
        <Self as Protocol>::connection_state(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opcua_config() {
        let config = OpcUaChannelConfig::new("opc.tcp://localhost:4840")
            .with_security(
                OpcUaSecurityPolicy::Basic256Sha256,
                OpcUaMessageSecurityMode::SignAndEncrypt,
            )
            .with_user_identity("user", "pass");

        assert_eq!(config.endpoint_url, "opc.tcp://localhost:4840");
        assert_eq!(config.security_policy, OpcUaSecurityPolicy::Basic256Sha256);
        assert_eq!(
            config.message_security_mode,
            OpcUaMessageSecurityMode::SignAndEncrypt
        );
    }

    #[test]
    fn opcua_dependency_exception_allows_only_anonymous_unsecured_sessions() {
        let safe = OpcUaChannelConfig::new("opc.tcp://localhost:4840");
        assert!(validate_dependency_exception(&safe).is_ok());

        let rsa_security = OpcUaChannelConfig::new("opc.tcp://localhost:4840").with_security(
            OpcUaSecurityPolicy::Basic256Sha256,
            OpcUaMessageSecurityMode::SignAndEncrypt,
        );
        assert!(validate_dependency_exception(&rsa_security).is_err());

        let credentials = OpcUaChannelConfig::new("opc.tcp://localhost:4840")
            .with_user_identity("user", "password");
        assert!(validate_dependency_exception(&credentials).is_err());
    }

    #[test]
    fn test_node_id_parsing() {
        let node_id = parse_node_id("i=1234", 2);
        assert_eq!(node_id.namespace, 2);

        let node_id = parse_node_id("s=Temperature", 2);
        assert_eq!(node_id.namespace, 2);

        let node_id = parse_node_id("Temperature", 2);
        assert_eq!(node_id.namespace, 2);
    }

    #[test]
    fn test_variant_conversion() {
        let value = convert_variant_to_value(&Variant::Double(25.5));
        assert_eq!(value.as_f64(), Some(25.5));

        let value = convert_variant_to_value(&Variant::Boolean(true));
        assert_eq!(value.as_bool(), Some(true));

        let value = convert_variant_to_value(&Variant::Int32(100));
        assert_eq!(value.as_i64(), Some(100));
    }

    #[test]
    fn test_quality_conversion() {
        let quality = convert_status_code_to_quality(StatusCode::Good);
        assert_eq!(quality, Quality::Good);

        let quality = convert_status_code_to_quality(StatusCode::BadNodeIdUnknown);
        assert_eq!(quality, Quality::Bad);
    }

    #[test]
    fn test_channel_capabilities() {
        let config = OpcUaChannelConfig::new("opc.tcp://localhost:4840");
        let channel = OpcUaChannel::new(config, 1, "test_opcua".to_string());

        assert_eq!(ProtocolCapabilities::name(&channel), "OPC UA");
        assert!(
            channel
                .supported_modes()
                .contains(&CommunicationMode::EventDriven)
        );
        assert!(
            channel
                .supported_modes()
                .contains(&CommunicationMode::Hybrid)
        );
    }

    #[test]
    fn test_node_id_key() {
        let key = make_node_id_key(2, "s=Temperature");
        assert_eq!(key, "ns=2;s=Temperature");
    }
}
