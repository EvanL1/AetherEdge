//! Matter Protocol Adapter
//!
//! Event-driven data collection from Matter (Connected Home over IP) devices
//! using UDP transport.
//!
//! ## Design Overview
//!
//! Matter is a smart home connectivity standard built on IPv6/UDP.
//! This adapter provides:
//! - UDP socket communication with Matter devices
//! - Attribute read via Matter Read Request
//! - Control via Matter Invoke Request (e.g., OnOff cluster)
//! - Adjustment via Matter Write Request
//!
//! ## Architecture Notes
//!
//! The Matter protocol layer is a framework implementation. Protocol frame
//! encoding/decoding uses a minimal viable approach (no external crate dependency).
//! Full Matter CASE/PASE session establishment and TLV encoding are marked as
//! TODO for when the ecosystem matures.
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "device_id": 12345,
//!   "ip_address": "192.168.1.100",
//!   "port": 5540,
//!   "subscribe_min_interval": 1,
//!   "subscribe_max_interval": 60
//! }
//! ```

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::protocols::ChannelRuntime;
use crate::protocols::adapters::matter_config::MatterConfig;
use crate::protocols::core::data::DataBatch;
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::PointConfig;
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
};

/// Matter protocol message types (simplified).
///
/// In a full implementation, these would map to Matter Interaction Model opcodes.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatterMessageType {
    /// Status Report (used for connectivity check)
    StatusReport = 0x01,
    /// Write Request (attribute write)
    WriteRequest = 0x06,
    /// Invoke Request (command invocation)
    InvokeRequest = 0x08,
}

/// Minimal Matter protocol frame header.
///
/// This is a simplified frame structure for the framework implementation.
/// A full Matter implementation would include CASE/PASE session headers,
/// message counters, and TLV-encoded payloads.
#[derive(Debug, Clone)]
struct MatterFrame {
    /// Message type
    message_type: MatterMessageType,
    /// Exchange ID for request/response correlation
    exchange_id: u16,
    /// Payload bytes
    payload: Vec<u8>,
}

impl MatterFrame {
    /// Encode frame into bytes for UDP transmission.
    ///
    /// Format: [msg_type: 1B][exchange_id: 2B LE][payload_len: 2B LE][payload]
    fn encode(&self) -> Vec<u8> {
        let payload_len = self.payload.len() as u16;
        let mut buf = Vec::with_capacity(5 + self.payload.len());
        buf.push(self.message_type as u8);
        buf.extend_from_slice(&self.exchange_id.to_le_bytes());
        buf.extend_from_slice(&payload_len.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decode frame from received bytes.
    ///
    /// Returns `None` if the buffer is too short or malformed.
    fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 5 {
            return None;
        }

        let exchange_id = u16::from_le_bytes([buf[1], buf[2]]);
        let payload_len = u16::from_le_bytes([buf[3], buf[4]]) as usize;

        if buf.len() < 5 + payload_len {
            return None;
        }

        let payload = buf[5..5 + payload_len].to_vec();

        Some(Self {
            // Store raw type; callers check as needed
            message_type: MatterMessageType::StatusReport,
            exchange_id,
            payload,
        })
    }
}

/// Build a Matter-like invoke request payload (e.g., OnOff Toggle).
///
/// Format: [endpoint: 2B LE][cluster_id: 4B LE][command_id: 4B LE][value: 8B LE f64]
fn build_invoke_request_payload(
    endpoint: u16,
    cluster_id: u32,
    command_id: u32,
    value: f64,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(18);
    payload.extend_from_slice(&endpoint.to_le_bytes());
    payload.extend_from_slice(&cluster_id.to_le_bytes());
    payload.extend_from_slice(&command_id.to_le_bytes());
    payload.extend_from_slice(&value.to_le_bytes());
    payload
}

/// Build a Matter-like write request payload.
///
/// Format: [endpoint: 2B LE][cluster_id: 4B LE][attribute_id: 4B LE][value: 8B LE f64]
fn build_write_request_payload(
    endpoint: u16,
    cluster_id: u32,
    attribute_id: u32,
    value: f64,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(18);
    payload.extend_from_slice(&endpoint.to_le_bytes());
    payload.extend_from_slice(&cluster_id.to_le_bytes());
    payload.extend_from_slice(&attribute_id.to_le_bytes());
    payload.extend_from_slice(&value.to_le_bytes());
    payload
}

/// Matter Channel implementation.
///
/// Event-driven channel that communicates with Matter devices over UDP.
/// Uses the Matter Interaction Model for data reads, writes, and commands.
pub struct MatterChannel {
    /// Channel configuration
    config: MatterConfig,
    /// Channel ID
    channel_id: u32,
    /// Channel name
    name: String,
    /// Point configurations (point_id -> PointConfig)
    points: HashMap<u32, PointConfig>,
    /// UDP socket for Matter communication
    socket: Option<Arc<UdpSocket>>,
    /// Remote device address
    remote_addr: Option<SocketAddr>,
    /// Background event loop task handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection state (atomic for lock-free access)
    state: AtomicU8,
    /// Event broadcast sender
    event_tx: DataEventSender,
    /// Diagnostics counters
    diagnostics: Arc<AtomicDiagnostics>,
    /// Exchange ID counter for request correlation
    next_exchange_id: u16,
    /// Log handler
    log_handler: Option<Arc<dyn ChannelLogHandler>>,
    /// Log config
    log_config: ChannelLogConfig,
}

impl MatterChannel {
    /// Create a new Matter channel.
    pub fn new(config: MatterConfig, channel_id: u32, name: String) -> Self {
        let (event_tx, _) = broadcast::channel(1024);

        Self {
            config,
            channel_id,
            name,
            points: HashMap::new(),
            socket: None,
            remote_addr: None,
            event_loop_handle: None,
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            event_tx,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            next_exchange_id: 0,
            log_handler: None,
            log_config: ChannelLogConfig::default(),
        }
    }

    /// Add point configurations.
    pub fn with_points(mut self, points: Vec<PointConfig>) -> Self {
        for point in points {
            self.points.insert(point.id, point);
        }
        self
    }

    /// Set connection state and broadcast event.
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
        let _ = self.event_tx.send(DataEvent::ConnectionChanged(state));
    }

    /// Get next exchange ID (wrapping).
    fn next_exchange_id(&mut self) -> u16 {
        let id = self.next_exchange_id;
        self.next_exchange_id = self.next_exchange_id.wrapping_add(1);
        id
    }

    /// Resolve remote device address.
    ///
    /// If `ip_address` is configured, use it directly.
    /// Otherwise, attempt mDNS discovery (currently a placeholder).
    fn resolve_address(&self) -> Result<SocketAddr> {
        if let Some(ref ip) = self.config.ip_address {
            let addr: SocketAddr = format!("{}:{}", ip, self.config.port)
                .parse()
                .map_err(|e| {
                    GatewayError::Config(format!("Invalid Matter device address: {}", e))
                })?;
            Ok(addr)
        } else {
            // TODO: Implement mDNS discovery for Matter devices
            // Matter uses DNS-SD (mDNS) for device discovery on the local network.
            // Service type: _matter._tcp or _matterc._udp (commissioning)
            Err(GatewayError::Config(
                "Matter mDNS discovery not yet implemented. Please specify ip_address.".into(),
            ))
        }
    }

    /// Send a UDP frame and wait for response with timeout.
    async fn send_and_recv(
        socket: &UdpSocket,
        remote_addr: SocketAddr,
        frame: &MatterFrame,
        timeout: std::time::Duration,
    ) -> Result<MatterFrame> {
        let data = frame.encode();
        socket
            .send_to(&data, remote_addr)
            .await
            .map_err(|e| GatewayError::Protocol(format!("Matter UDP send failed: {}", e)))?;

        let mut buf = [0u8; 2048];
        let recv_result = tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await;

        match recv_result {
            Ok(Ok((len, _from))) => MatterFrame::decode(&buf[..len])
                .ok_or_else(|| GatewayError::Protocol("Invalid Matter frame received".into())),
            Ok(Err(e)) => Err(GatewayError::Protocol(format!(
                "Matter UDP recv failed: {}",
                e
            ))),
            Err(_) => Err(GatewayError::Protocol("Matter response timeout".into())),
        }
    }

    /// Run the background UDP event loop.
    ///
    /// Listens for incoming Matter messages (subscription reports, etc.)
    /// and broadcasts them as DataEvents.
    async fn run_event_loop(
        socket: Arc<UdpSocket>,
        channel_id: u32,
        state: Arc<AtomicU8>,
        event_tx: DataEventSender,
        diagnostics: Arc<AtomicDiagnostics>,
    ) {
        info!(channel_id, "Matter event loop started");

        let mut buf = [0u8; 2048];

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, from)) => {
                    debug!(
                        channel_id,
                        len,
                        from = %from,
                        "Received Matter UDP message"
                    );

                    if let Some(frame) = MatterFrame::decode(&buf[..len]) {
                        // TODO: Parse Matter ReportData response and extract attribute values.
                        // For now, we log the frame and record the read.
                        // In a full implementation, we would:
                        // 1. Decode TLV payload
                        // 2. Match attribute paths to point IDs
                        // 3. Create DataPoint entries
                        // 4. Broadcast via event_tx

                        let batch = DataBatch::new();
                        // Placeholder: actual data extraction from frame.payload would go here
                        let _ = &frame;

                        if !batch.is_empty() {
                            diagnostics.add_read(batch.len() as u64);
                            let _ = event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));
                        }
                    } else {
                        debug!(channel_id, len, "Failed to decode Matter frame");
                        diagnostics.record_error("Invalid Matter frame received");
                    }
                },
                Err(e) => {
                    error!(channel_id, error = %e, "Matter UDP recv error");
                    state.store(ConnectionState::Reconnecting as u8, Ordering::SeqCst);
                    let _ =
                        event_tx.send(DataEvent::ConnectionChanged(ConnectionState::Reconnecting));
                    let _ = event_tx.send(DataEvent::Error(e.to_string()));
                    diagnostics.record_error(e.to_string());

                    // Brief delay before retrying recv
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                },
            }
        }
    }

    /// Find point config and extract Matter address components.
    ///
    /// Returns (endpoint, cluster_id, attribute_id) for the given point ID.
    fn resolve_point_address(&self, point_id: u32) -> Result<(u16, u32, u32)> {
        let point = self
            .points
            .get(&point_id)
            .ok_or_else(|| GatewayError::Config(format!("Unknown point ID: {}", point_id)))?;

        match &point.address {
            crate::protocols::core::point::ProtocolAddress::Matter(addr) => {
                Ok((addr.endpoint, addr.cluster_id, addr.attribute_id))
            },
            _ => Err(GatewayError::Config(format!(
                "Point {} does not have a Matter address",
                point_id
            ))),
        }
    }
}

#[async_trait]
impl ChannelRuntime for MatterChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "matter"
    }

    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        if self.socket.is_some() {
            return Ok(());
        }

        self.set_state(ConnectionState::Connecting);

        // Resolve remote device address
        let remote_addr = self.resolve_address()?;
        self.remote_addr = Some(remote_addr);

        // Bind UDP socket to any available local port
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| {
            GatewayError::Protocol(format!("Failed to bind Matter UDP socket: {}", e))
        })?;

        info!(
            channel_id = self.channel_id,
            remote = %remote_addr,
            local = %socket.local_addr().unwrap_or_else(|_| "unknown".parse().unwrap()),
            "Matter UDP socket bound"
        );

        // Send a StatusReport frame to verify connectivity
        let exchange_id = self.next_exchange_id();
        let probe_frame = MatterFrame {
            message_type: MatterMessageType::StatusReport,
            exchange_id,
            payload: vec![],
        };

        // Try to send probe; if it fails, we still consider socket ready
        // (the device might not respond to our simplified probe)
        let probe_data = probe_frame.encode();
        match socket.send_to(&probe_data, remote_addr).await {
            Ok(_) => {
                debug!(
                    channel_id = self.channel_id,
                    "Matter connectivity probe sent"
                );
            },
            Err(e) => {
                warn!(
                    channel_id = self.channel_id,
                    error = %e,
                    "Matter connectivity probe failed (continuing anyway)"
                );
            },
        }

        let socket = Arc::new(socket);
        self.socket = Some(socket.clone());

        self.set_state(ConnectionState::Connected);

        info!(
            channel_id = self.channel_id,
            device_id = self.config.device_id,
            remote = %remote_addr,
            "Matter channel connected"
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Abort event loop
        if let Some(handle) = self.event_loop_handle.take() {
            handle.abort();
        }

        // Drop socket
        self.socket = None;
        self.remote_addr = None;

        self.set_state(ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "Matter channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        // Event-driven protocol - return empty batch.
        // Data is delivered via subscribe() from the background event loop.
        PollResult::success(DataBatch::new())
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let socket = self
            .socket
            .clone()
            .ok_or_else(|| GatewayError::Protocol("Matter channel not connected".into()))?;
        let remote_addr = self
            .remote_addr
            .ok_or_else(|| GatewayError::Protocol("Matter remote address not set".into()))?;
        let timeout = self.config.connect_timeout;

        let mut success_count = 0;

        for &(point_id, value) in commands {
            let (endpoint, cluster_id, _attribute_id) = self.resolve_point_address(point_id)?;

            // For control commands, use InvokeRequest
            // Map the value to a command ID. Convention:
            // - cluster 0x0006 (OnOff): command 0=Off, 1=On, 2=Toggle
            let command_id = if value != 0.0 { 1u32 } else { 0u32 };

            let payload = build_invoke_request_payload(endpoint, cluster_id, command_id, value);
            let exchange_id = self.next_exchange_id();

            let frame = MatterFrame {
                message_type: MatterMessageType::InvokeRequest,
                exchange_id,
                payload,
            };

            // TODO: Full Matter session + CASE authentication would wrap this frame.
            // For now, send raw and handle response.
            match Self::send_and_recv(&socket, remote_addr, &frame, timeout).await {
                Ok(_response) => {
                    debug!(
                        channel_id = self.channel_id,
                        point_id, endpoint, cluster_id, command_id, "Matter InvokeRequest sent"
                    );
                    self.diagnostics.inc_write();
                    success_count += 1;
                },
                Err(e) => {
                    warn!(
                        channel_id = self.channel_id,
                        point_id,
                        error = %e,
                        "Matter InvokeRequest failed"
                    );
                    self.diagnostics.record_error(e.to_string());
                },
            }
        }

        Ok(success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let socket = self
            .socket
            .clone()
            .ok_or_else(|| GatewayError::Protocol("Matter channel not connected".into()))?;
        let remote_addr = self
            .remote_addr
            .ok_or_else(|| GatewayError::Protocol("Matter remote address not set".into()))?;
        let timeout = self.config.connect_timeout;

        let mut success_count = 0;

        for &(point_id, value) in adjustments {
            let (endpoint, cluster_id, attribute_id) = self.resolve_point_address(point_id)?;

            let payload = build_write_request_payload(endpoint, cluster_id, attribute_id, value);
            let exchange_id = self.next_exchange_id();

            let frame = MatterFrame {
                message_type: MatterMessageType::WriteRequest,
                exchange_id,
                payload,
            };

            // TODO: Full Matter session + CASE authentication would wrap this frame.
            match Self::send_and_recv(&socket, remote_addr, &frame, timeout).await {
                Ok(_response) => {
                    debug!(
                        channel_id = self.channel_id,
                        point_id,
                        endpoint,
                        cluster_id,
                        attribute_id,
                        value,
                        "Matter WriteRequest sent"
                    );
                    self.diagnostics.inc_write();
                    success_count += 1;
                },
                Err(e) => {
                    warn!(
                        channel_id = self.channel_id,
                        point_id,
                        error = %e,
                        "Matter WriteRequest failed"
                    );
                    self.diagnostics.record_error(e.to_string());
                },
            }
        }

        Ok(success_count)
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(self.event_tx.subscribe())
    }

    async fn start_events(&mut self) -> Result<()> {
        // Connect if not already connected
        if self.socket.is_none() {
            self.connect().await?;
        }

        // Start background event loop if not already running
        if self.event_loop_handle.is_none() {
            let socket = self
                .socket
                .clone()
                .ok_or_else(|| GatewayError::Protocol("Matter socket not available".into()))?;
            let channel_id = self.channel_id;
            let state = Arc::new(AtomicU8::new(ConnectionState::Connected as u8));
            let event_tx = self.event_tx.clone();
            let diagnostics = self.diagnostics.clone();

            let handle = tokio::spawn(async move {
                Self::run_event_loop(socket, channel_id, state, event_tx, diagnostics).await;
            });

            self.event_loop_handle = Some(handle);

            info!(channel_id = self.channel_id, "Matter event loop started");
        }

        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        self.disconnect().await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "matter".to_string(),
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

    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        self.log_handler = Some(handler);
    }

    fn set_log_config(&mut self, config: ChannelLogConfig) {
        self.log_config = config;
    }

    fn log_handler(&self) -> Option<Arc<dyn ChannelLogHandler>> {
        self.log_handler.clone()
    }
}

impl HasMetadata for MatterChannel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "matter_udp",
            display_name: "Matter (UDP)",
            description: "Matter smart home protocol over UDP",
            is_recommended: true,
            example_config: json!({
                "device_id": 12345,
                "ip_address": "192.168.1.100",
                "port": 5540,
                "subscribe_min_interval": 1,
                "subscribe_max_interval": 60
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "device_id",
                    "Device ID",
                    "Matter Node ID of the target device",
                    ParameterType::Integer,
                ),
                ParameterMetadata::optional(
                    "ip_address",
                    "IP Address",
                    "Known IP address (skip mDNS discovery)",
                    ParameterType::String,
                    json!(null),
                ),
                ParameterMetadata::optional(
                    "port",
                    "Port",
                    "Device port (default 5540)",
                    ParameterType::Integer,
                    json!(5540),
                ),
                ParameterMetadata::optional(
                    "fabric_id",
                    "Fabric ID",
                    "Fabric ID for multi-fabric setups",
                    ParameterType::Integer,
                    json!(null),
                ),
                ParameterMetadata::optional(
                    "pin_code",
                    "PIN Code",
                    "Pairing PIN code for commissioning",
                    ParameterType::Integer,
                    json!(null),
                ),
                ParameterMetadata::optional(
                    "subscribe_min_interval",
                    "Min Subscribe Interval",
                    "Subscription minimum interval in seconds",
                    ParameterType::Integer,
                    json!(1),
                ),
                ParameterMetadata::optional(
                    "subscribe_max_interval",
                    "Max Subscribe Interval",
                    "Subscription maximum interval in seconds",
                    ParameterType::Integer,
                    json!(60),
                ),
            ],
        }
    }
}

impl std::fmt::Debug for MatterChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatterChannel")
            .field("channel_id", &self.channel_id)
            .field("name", &self.name)
            .field("device_id", &self.config.device_id)
            .field("remote_addr", &self.remote_addr)
            .field("state", &self.connection_state())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn test_config() -> MatterConfig {
        MatterConfig {
            device_id: 12345,
            fabric_id: None,
            pin_code: None,
            discriminator: None,
            ip_address: Some("192.168.1.100".to_string()),
            port: 5540,
            subscribe_min_interval: 1,
            subscribe_max_interval: 60,
            connect_timeout: std::time::Duration::from_secs(5),
            reconnect_interval: std::time::Duration::from_secs(5),
        }
    }

    #[test]
    fn test_matter_channel_creation() {
        let config = test_config();
        let channel = MatterChannel::new(config, 1, "test_matter".to_string());

        assert_eq!(channel.id(), 1);
        assert_eq!(channel.name(), "test_matter");
        assert_eq!(channel.protocol(), "matter");
        assert!(channel.is_event_driven());
        assert_eq!(channel.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn test_matter_frame_encode_decode() {
        let frame = MatterFrame {
            message_type: MatterMessageType::StatusReport,
            exchange_id: 42,
            payload: vec![1, 2, 3, 4],
        };

        let encoded = frame.encode();
        assert_eq!(encoded.len(), 5 + 4); // header + payload

        let decoded = MatterFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.exchange_id, 42);
        assert_eq!(decoded.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_matter_frame_decode_too_short() {
        let buf = [0u8; 3];
        assert!(MatterFrame::decode(&buf).is_none());
    }

    #[test]
    fn test_matter_frame_decode_truncated_payload() {
        // Header says payload_len=10 but only 2 bytes follow
        let buf = [0x01, 0x00, 0x00, 0x0A, 0x00, 0x01, 0x02];
        assert!(MatterFrame::decode(&buf).is_none());
    }

    #[test]
    fn test_build_invoke_request_payload() {
        let payload = build_invoke_request_payload(1, 0x0006, 1, 1.0);
        assert_eq!(payload.len(), 18);

        let endpoint = u16::from_le_bytes([payload[0], payload[1]]);
        let cluster = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        let command = u32::from_le_bytes([payload[6], payload[7], payload[8], payload[9]]);

        assert_eq!(endpoint, 1);
        assert_eq!(cluster, 0x0006);
        assert_eq!(command, 1);
    }

    #[test]
    fn test_build_write_request_payload() {
        let payload = build_write_request_payload(2, 0x0201, 0x0012, 22.5);
        assert_eq!(payload.len(), 18);

        let endpoint = u16::from_le_bytes([payload[0], payload[1]]);
        let cluster = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        let attr = u32::from_le_bytes([payload[6], payload[7], payload[8], payload[9]]);
        let value = f64::from_le_bytes([
            payload[10],
            payload[11],
            payload[12],
            payload[13],
            payload[14],
            payload[15],
            payload[16],
            payload[17],
        ]);

        assert_eq!(endpoint, 2);
        assert_eq!(cluster, 0x0201);
        assert_eq!(attr, 0x0012);
        assert!((value - 22.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_resolve_address_with_ip() {
        let config = test_config();
        let channel = MatterChannel::new(config, 1, "test".to_string());
        let addr = channel.resolve_address().unwrap();
        assert_eq!(addr.port(), 5540);
        assert_eq!(addr.ip().to_string(), "192.168.1.100");
    }

    #[test]
    fn test_resolve_address_without_ip() {
        let config = MatterConfig {
            ip_address: None,
            ..test_config()
        };
        let channel = MatterChannel::new(config, 1, "test".to_string());
        let result = channel.resolve_address();
        assert!(result.is_err());
    }

    #[test]
    fn test_matter_metadata() {
        let meta = MatterChannel::metadata();
        assert_eq!(meta.name, "matter_udp");
        assert!(meta.is_recommended);
        assert!(!meta.parameters.is_empty());
    }

    #[test]
    fn test_matter_subscribe() {
        let config = test_config();
        let channel = MatterChannel::new(config, 1, "test".to_string());
        let rx = channel.subscribe();
        assert!(rx.is_some());
    }

    #[test]
    fn test_matter_channel_with_points() {
        use crate::protocols::core::point::{MatterAddress, PointConfig, ProtocolAddress};

        let config = test_config();
        let points = vec![
            PointConfig::telemetry(
                100,
                ProtocolAddress::Matter(MatterAddress::new(1, 0x0402, 0x0000)),
            ),
            PointConfig::control(
                200,
                ProtocolAddress::Matter(MatterAddress::new(1, 0x0006, 0x0000)),
            ),
            PointConfig::adjustment(
                300,
                ProtocolAddress::Matter(MatterAddress::new(2, 0x0201, 0x0012)),
            ),
        ];

        let channel =
            MatterChannel::new(config, 1, "test_with_points".to_string()).with_points(points);

        assert_eq!(channel.points.len(), 3);
        assert!(channel.points.contains_key(&100));
        assert!(channel.points.contains_key(&200));
        assert!(channel.points.contains_key(&300));

        // Verify point data is preserved
        let p100 = &channel.points[&100];
        assert_eq!(p100.id, 100);
        match &p100.address {
            ProtocolAddress::Matter(addr) => {
                assert_eq!(addr.endpoint, 1);
                assert_eq!(addr.cluster_id, 0x0402);
                assert_eq!(addr.attribute_id, 0x0000);
            },
            _ => panic!("Expected Matter address"),
        }
    }

    #[test]
    fn test_matter_channel_protocol() {
        let config = test_config();
        let channel = MatterChannel::new(config, 1, "proto_test".to_string());
        assert_eq!(channel.protocol(), "matter");
    }

    #[test]
    fn test_matter_channel_is_event_driven() {
        let config = test_config();
        let channel = MatterChannel::new(config, 1, "event_test".to_string());
        assert!(channel.is_event_driven());
    }

    #[tokio::test]
    async fn test_matter_channel_initial_diagnostics() {
        let config = test_config();
        let channel = MatterChannel::new(config, 1, "diag_test".to_string());
        let diag = channel.diagnostics().await.unwrap();

        assert_eq!(diag.protocol, "matter");
        assert_eq!(diag.connection_state, ConnectionState::Disconnected);
        assert_eq!(diag.read_count, 0);
        assert_eq!(diag.write_count, 0);
        assert_eq!(diag.error_count, 0);
        assert!(diag.last_error.is_none());
    }

    #[test]
    fn test_matter_frame_encode_empty_payload() {
        let frame = MatterFrame {
            message_type: MatterMessageType::StatusReport,
            exchange_id: 0,
            payload: vec![],
        };

        let encoded = frame.encode();
        // Header only: 1 (msg_type) + 2 (exchange_id) + 2 (payload_len) = 5
        assert_eq!(encoded.len(), 5);
        assert_eq!(encoded[0], MatterMessageType::StatusReport as u8);
        assert_eq!(u16::from_le_bytes([encoded[1], encoded[2]]), 0); // exchange_id
        assert_eq!(u16::from_le_bytes([encoded[3], encoded[4]]), 0); // payload_len

        // Round-trip decode
        let decoded = MatterFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.exchange_id, 0);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_matter_frame_all_message_types() {
        let types = [
            MatterMessageType::StatusReport,
            MatterMessageType::WriteRequest,
            MatterMessageType::InvokeRequest,
        ];

        for msg_type in types {
            let frame = MatterFrame {
                message_type: msg_type,
                exchange_id: 1000,
                payload: vec![0xAA, 0xBB],
            };

            let encoded = frame.encode();
            // Verify the first byte is the message type discriminant
            assert_eq!(encoded[0], msg_type as u8);
            // Verify total length: 5 header + 2 payload
            assert_eq!(encoded.len(), 7);

            // Decode round-trip preserves exchange_id and payload
            let decoded = MatterFrame::decode(&encoded).unwrap();
            assert_eq!(decoded.exchange_id, 1000);
            assert_eq!(decoded.payload, vec![0xAA, 0xBB]);
        }
    }

    #[test]
    fn test_build_write_request_negative_value() {
        let payload = build_write_request_payload(1, 0x0201, 0x0012, -10.5);
        assert_eq!(payload.len(), 18);

        let value = f64::from_le_bytes([
            payload[10],
            payload[11],
            payload[12],
            payload[13],
            payload[14],
            payload[15],
            payload[16],
            payload[17],
        ]);
        assert!((value - (-10.5)).abs() < f64::EPSILON);

        // Also verify endpoint/cluster/attribute are correctly encoded
        let endpoint = u16::from_le_bytes([payload[0], payload[1]]);
        let cluster = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        let attr = u32::from_le_bytes([payload[6], payload[7], payload[8], payload[9]]);
        assert_eq!(endpoint, 1);
        assert_eq!(cluster, 0x0201);
        assert_eq!(attr, 0x0012);
    }

    #[test]
    fn test_resolve_address_invalid_ip() {
        let config = MatterConfig {
            ip_address: Some("not-a-valid-ip".to_string()),
            ..test_config()
        };
        let channel = MatterChannel::new(config, 1, "test".to_string());
        let result = channel.resolve_address();
        assert!(result.is_err());

        // Verify error message mentions "Invalid Matter device address"
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Invalid Matter device address"),
            "Error message was: {}",
            err_msg
        );
    }
}
