//! Zigbee Protocol Adapter
//!
//! Event-driven data collection from Zigbee devices via TCP gateway.
//!
//! ## Design Overview
//!
//! Zigbee devices communicate through a coordinator (CC2652, EFR32, etc.) that
//! is connected via TCP. The adapter:
//! - Connects to the TCP gateway
//! - Decodes ZCL frames using the appropriate codec (Raw, ZNP, EZSP)
//! - Maps attribute reports to data points via (ieee_addr, endpoint, cluster, attr) lookup
//! - Broadcasts data events to subscribers
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "host": "192.168.1.100",
//!   "port": 8888,
//!   "gateway_type": "raw",
//!   "permit_join_on_start": false
//! }
//! ```

use async_trait::async_trait;
use bytes::BytesMut;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::protocols::ChannelRuntime;
use crate::protocols::adapters::zigbee_codec::{
    AttributeReport, FrameCodec, RawFrameCodec, ZigbeeFrame,
};
use crate::protocols::adapters::zigbee_config::{GatewayType, ZigbeeConfig};
use crate::protocols::core::data::{DataBatch, DataPoint};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::PointConfig;
use crate::protocols::core::point::ZigbeeAddress;
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
};

/// TCP read buffer size
const TCP_READ_BUF_SIZE: usize = 4096;

/// Lookup key for fast point resolution from attribute reports.
type PointLookupKey = (u64, u8, u16, u16); // (ieee_addr, endpoint, cluster_id, attribute_id)

/// Zigbee Channel implementation.
///
/// Event-driven channel that connects to a Zigbee TCP gateway and decodes
/// ZCL attribute reports into data points.
pub struct ZigbeeChannel {
    /// Channel configuration
    config: ZigbeeConfig,
    /// Channel ID
    channel_id: u32,
    /// Channel name
    name: String,
    /// Point configurations
    points: Vec<PointConfig>,
    /// Event loop task handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// TCP write half (for sending commands)
    write_half: Option<tokio::io::WriteHalf<TcpStream>>,
    /// Connection state (atomic for lock-free access)
    state: Arc<AtomicU8>,
    /// Event broadcast sender
    event_tx: DataEventSender,
    /// Diagnostics counters
    diagnostics: Arc<AtomicDiagnostics>,
    /// Log handler
    log_handler: Option<Arc<dyn ChannelLogHandler>>,
    /// Log config
    _log_config: ChannelLogConfig,
}

impl ZigbeeChannel {
    /// Create a new Zigbee channel.
    pub fn new(
        config: ZigbeeConfig,
        channel_id: u32,
        name: String,
        points: Vec<PointConfig>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(1024);

        Self {
            config,
            channel_id,
            name,
            points,
            event_loop_handle: None,
            write_half: None,
            state: Arc::new(AtomicU8::new(ConnectionState::Disconnected as u8)),
            event_tx,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            log_handler: None,
            _log_config: ChannelLogConfig::default(),
        }
    }

    /// Build the point lookup map from configured points.
    fn build_point_lookup(points: &[PointConfig]) -> HashMap<PointLookupKey, PointConfig> {
        let mut map = HashMap::with_capacity(points.len());
        for point in points {
            if let crate::protocols::core::point::ProtocolAddress::Zigbee(ref addr) = point.address
            {
                let key = (
                    addr.ieee_address,
                    addr.endpoint,
                    addr.cluster_id,
                    addr.attribute_id,
                );
                map.insert(key, point.clone());
            }
        }
        map
    }

    /// Create the appropriate frame codec for the gateway type.
    fn create_codec(gateway_type: GatewayType) -> Box<dyn FrameCodec> {
        match gateway_type {
            GatewayType::Raw => Box::new(RawFrameCodec),
            GatewayType::Znp => {
                warn!("ZNP codec not implemented, falling back to Raw");
                Box::new(RawFrameCodec)
            },
            GatewayType::Ezsp => {
                warn!("EZSP codec not implemented, falling back to Raw");
                Box::new(RawFrameCodec)
            },
        }
    }

    /// Set connection state and broadcast event.
    fn set_state(state: &AtomicU8, event_tx: &DataEventSender, new_state: ConnectionState) {
        state.store(new_state as u8, Ordering::SeqCst);
        let _ = event_tx.send(DataEvent::ConnectionChanged(new_state));
    }

    /// Process an attribute report into a DataPoint.
    fn process_attribute_report(
        report: &AttributeReport,
        lookup: &HashMap<PointLookupKey, PointConfig>,
    ) -> Option<DataPoint> {
        let key = (
            report.ieee_addr,
            report.endpoint,
            report.cluster_id,
            report.attribute_id,
        );

        let point = lookup.get(&key)?;
        let raw_value = report.value.to_f64();
        let transformed = point.transform.apply(raw_value);

        Some(DataPoint::new(point.id, point.point_type, transformed))
    }

    /// Run the TCP event loop — reads frames from the gateway and dispatches events.
    async fn run_event_loop(
        mut read_half: tokio::io::ReadHalf<TcpStream>,
        codec: Box<dyn FrameCodec>,
        channel_id: u32,
        state: Arc<AtomicU8>,
        event_tx: DataEventSender,
        diagnostics: Arc<AtomicDiagnostics>,
        point_lookup: HashMap<PointLookupKey, PointConfig>,
    ) {
        info!(channel_id, "Zigbee event loop started");

        let mut buf = BytesMut::with_capacity(TCP_READ_BUF_SIZE);

        loop {
            // Read data from TCP stream
            match read_half.read_buf(&mut buf).await {
                Ok(0) => {
                    // Connection closed by peer
                    info!(channel_id, "Zigbee TCP connection closed by peer");
                    Self::set_state(&state, &event_tx, ConnectionState::Disconnected);
                    break;
                },
                Ok(n) => {
                    debug!(channel_id, bytes = n, "Read from Zigbee TCP gateway");

                    // Decode all available frames
                    loop {
                        match codec.decode(&mut buf) {
                            Ok(Some(frame)) => {
                                Self::handle_frame(
                                    &frame,
                                    channel_id,
                                    &event_tx,
                                    &diagnostics,
                                    &point_lookup,
                                );
                            },
                            Ok(None) => break, // Need more data
                            Err(e) => {
                                debug!(
                                    channel_id,
                                    error = %e,
                                    "Failed to decode Zigbee frame"
                                );
                                diagnostics.record_error(e.to_string());
                                // Continue reading — decode errors don't break the loop
                                break;
                            },
                        }
                    }
                },
                Err(e) => {
                    error!(channel_id, error = %e, "Zigbee TCP read error");
                    Self::set_state(&state, &event_tx, ConnectionState::Reconnecting);
                    let _ = event_tx.send(DataEvent::Error(e.to_string()));
                    diagnostics.record_error(e.to_string());
                    break;
                },
            }
        }

        info!(channel_id, "Zigbee event loop exiting");
    }

    /// Handle a decoded Zigbee frame.
    fn handle_frame(
        frame: &ZigbeeFrame,
        channel_id: u32,
        event_tx: &DataEventSender,
        diagnostics: &AtomicDiagnostics,
        point_lookup: &HashMap<PointLookupKey, PointConfig>,
    ) {
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                if let Some(data_point) = Self::process_attribute_report(report, point_lookup) {
                    let mut batch = DataBatch::with_capacity(1);
                    batch.add(data_point);
                    diagnostics.inc_read();
                    let _ = event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));
                    debug!(
                        channel_id,
                        ieee = format!("0x{:016X}", report.ieee_addr),
                        endpoint = report.endpoint,
                        cluster = format!("0x{:04X}", report.cluster_id),
                        attr = format!("0x{:04X}", report.attribute_id),
                        "Processed Zigbee attribute report"
                    );
                } else {
                    debug!(
                        channel_id,
                        ieee = format!("0x{:016X}", report.ieee_addr),
                        endpoint = report.endpoint,
                        cluster = format!("0x{:04X}", report.cluster_id),
                        attr = format!("0x{:04X}", report.attribute_id),
                        "Unmapped Zigbee attribute report (no matching point)"
                    );
                }
            },
            ZigbeeFrame::DeviceAnnounce(announce) => {
                info!(
                    channel_id,
                    ieee = format!("0x{:016X}", announce.ieee_addr),
                    short_addr = format!("0x{:04X}", announce.short_addr),
                    "Zigbee device announced"
                );
            },
            ZigbeeFrame::CommandResponse { seq, status } => {
                debug!(channel_id, seq, status, "Zigbee command response");
            },
            ZigbeeFrame::Unknown(data) => {
                debug!(channel_id, len = data.len(), "Unknown Zigbee frame type");
            },
        }
    }
}

#[async_trait]
impl ChannelRuntime for ZigbeeChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "zigbee"
    }

    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        if self.event_loop_handle.is_some() {
            return Ok(());
        }

        Self::set_state(&self.state, &self.event_tx, ConnectionState::Connecting);

        let addr = format!("{}:{}", self.config.host, self.config.port);

        // Connect to TCP gateway with timeout
        let stream = tokio::time::timeout(self.config.connect_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| {
                GatewayError::ConnectionTimeout(self.config.connect_timeout.as_millis() as u64)
            })?
            .map_err(|e| GatewayError::Connection(format!("TCP connect to {addr} failed: {e}")))?;

        // Disable Nagle's algorithm for lower latency
        stream
            .set_nodelay(true)
            .map_err(|e| GatewayError::Connection(format!("Failed to set TCP_NODELAY: {e}")))?;

        info!(
            channel_id = self.channel_id,
            addr = %addr,
            "Connected to Zigbee TCP gateway"
        );

        // Split stream for concurrent read/write
        let (read_half, write_half) = tokio::io::split(stream);
        self.write_half = Some(write_half);

        // Create codec
        let codec = Self::create_codec(self.config.gateway_type);

        // Build point lookup table
        let point_lookup = Self::build_point_lookup(&self.points);
        info!(
            channel_id = self.channel_id,
            point_count = point_lookup.len(),
            "Built Zigbee point lookup table"
        );

        // Spawn event loop
        let channel_id = self.channel_id;
        let state = self.state.clone();
        let event_tx = self.event_tx.clone();
        let diagnostics = self.diagnostics.clone();

        let handle = tokio::spawn(async move {
            Self::run_event_loop(
                read_half,
                codec,
                channel_id,
                state,
                event_tx,
                diagnostics,
                point_lookup,
            )
            .await;
        });

        self.event_loop_handle = Some(handle);

        Self::set_state(&self.state, &self.event_tx, ConnectionState::Connected);

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Abort event loop
        if let Some(handle) = self.event_loop_handle.take() {
            handle.abort();
        }

        // Drop write half (closes the TCP connection)
        self.write_half = None;

        Self::set_state(&self.state, &self.event_tx, ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "Zigbee channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        // Event-driven protocol — return empty batch.
        // Data is delivered via subscribe().
        PollResult::success(DataBatch::new())
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let write_half = self.write_half.as_mut().ok_or(GatewayError::NotConnected)?;

        let codec = Self::create_codec(self.config.gateway_type);
        let point_lookup = Self::build_point_lookup(&self.points);

        // Build reverse lookup: point_id -> ZigbeeAddress
        let addr_lookup: HashMap<u32, &ZigbeeAddress> = self
            .points
            .iter()
            .filter_map(|p| {
                if let crate::protocols::core::point::ProtocolAddress::Zigbee(ref addr) = p.address
                {
                    Some((p.id, addr))
                } else {
                    None
                }
            })
            .collect();

        let _ = point_lookup; // suppress unused warning — we only need addr_lookup here

        let mut written = 0;
        for (point_id, value) in commands {
            let addr = match addr_lookup.get(point_id) {
                Some(a) => *a,
                None => {
                    warn!(
                        channel_id = self.channel_id,
                        point_id, "No Zigbee address for control point"
                    );
                    continue;
                },
            };

            // Encode as a simple command with the value as a single byte payload.
            // For boolean controls (On/Off cluster 0x0006): value > 0.5 = On (cmd 0x01), else Off (cmd 0x00).
            let command_id = if *value > 0.5 { 0x01 } else { 0x00 };
            let frame = codec.encode_command(
                addr.ieee_address,
                addr.endpoint,
                addr.cluster_id,
                command_id,
                &[],
            );

            write_half
                .write_all(&frame)
                .await
                .map_err(|e| GatewayError::Protocol(format!("TCP write failed: {e}")))?;

            self.diagnostics.inc_write();
            written += 1;

            debug!(
                channel_id = self.channel_id,
                point_id, command_id, "Sent Zigbee control command"
            );
        }

        Ok(written)
    }

    async fn write_adjustment(&mut self, _adjustments: &[(u32, f64)]) -> Result<usize> {
        Err(GatewayError::Unsupported(
            "Zigbee channel does not yet support adjustment writes".to_string(),
        ))
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(self.event_tx.subscribe())
    }

    async fn start_events(&mut self) -> Result<()> {
        if self.event_loop_handle.is_none() {
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
            protocol: "zigbee".to_string(),
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
        self._log_config = config;
    }

    fn log_handler(&self) -> Option<Arc<dyn ChannelLogHandler>> {
        self.log_handler.clone()
    }
}

impl HasMetadata for ZigbeeChannel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "zigbee",
            display_name: "Zigbee (TCP Gateway)",
            description: "Zigbee protocol via TCP-connected coordinator gateway",
            is_recommended: true,
            example_config: json!({
                "host": "192.168.1.100",
                "port": 8888,
                "gateway_type": "raw",
                "permit_join_on_start": false,
                "connect_timeout_ms": 5000,
                "reconnect_interval_ms": 5000
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "host",
                    "Gateway Host",
                    "TCP gateway host address",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "port",
                    "Gateway Port",
                    "TCP gateway port",
                    ParameterType::Integer,
                    json!(8888),
                ),
                ParameterMetadata::optional(
                    "gateway_type",
                    "Gateway Type",
                    "Frame encoding type: raw, znp, or ezsp",
                    ParameterType::String,
                    json!("raw"),
                ),
                ParameterMetadata::optional(
                    "permit_join_on_start",
                    "Permit Join",
                    "Open network for joining on startup",
                    ParameterType::Boolean,
                    json!(false),
                ),
            ],
        }
    }
}

impl std::fmt::Debug for ZigbeeChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZigbeeChannel")
            .field("channel_id", &self.channel_id)
            .field("name", &self.name)
            .field("host", &self.config.host)
            .field("port", &self.config.port)
            .field("gateway_type", &self.config.gateway_type)
            .field("state", &self.connection_state())
            .field("points", &self.points.len())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::protocols::adapters::zigbee_config::ZigbeeParamsConfig;
    use crate::protocols::core::point::{ProtocolAddress, TransformConfig};
    use aether_model::PointType;

    fn make_test_point(id: u32, ieee: u64, ep: u8, cluster: u16, attr: u16) -> PointConfig {
        PointConfig {
            id,
            point_type: PointType::Telemetry,
            name: Some(format!("test_point_{id}")),
            address: ProtocolAddress::Zigbee(ZigbeeAddress {
                ieee_address: ieee,
                endpoint: ep,
                cluster_id: cluster,
                attribute_id: attr,
            }),
            transform: TransformConfig::linear(1.0, 0.0),
            poll_group: None,
            enabled: true,
        }
    }

    #[test]
    fn test_build_point_lookup() {
        let points = vec![
            make_test_point(1, 0x00124B0018ED1234, 1, 0x0402, 0x0000),
            make_test_point(2, 0x00124B0018ED1234, 1, 0x0405, 0x0000),
            make_test_point(3, 0x00124B0018ED5678, 2, 0x0006, 0x0000),
        ];

        let lookup = ZigbeeChannel::build_point_lookup(&points);
        assert_eq!(lookup.len(), 3);

        let key = (0x00124B0018ED1234, 1, 0x0402, 0x0000);
        assert!(lookup.contains_key(&key));
        assert_eq!(lookup[&key].id, 1);
    }

    #[test]
    fn test_process_attribute_report() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let points = vec![make_test_point(1, 0x00124B0018ED1234, 1, 0x0402, 0x0000)];

        let lookup = ZigbeeChannel::build_point_lookup(&points);

        let report = AttributeReport {
            ieee_addr: 0x00124B0018ED1234,
            endpoint: 1,
            cluster_id: 0x0402,
            attribute_id: 0x0000,
            value: ZclValue::UInt16(2500),
        };

        let dp = ZigbeeChannel::process_attribute_report(&report, &lookup);
        assert!(dp.is_some());
        let dp = dp.unwrap();
        assert_eq!(dp.id, 1);
        assert_eq!(dp.point_type, PointType::Telemetry);
        assert_eq!(dp.value.as_f64(), Some(2500.0));
    }

    #[test]
    fn test_process_attribute_report_with_transform() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let mut point = make_test_point(1, 0x00124B0018ED1234, 1, 0x0402, 0x0000);
        point.transform = TransformConfig::linear(0.01, 0.0); // scale by 0.01

        let lookup = ZigbeeChannel::build_point_lookup(&[point]);

        let report = AttributeReport {
            ieee_addr: 0x00124B0018ED1234,
            endpoint: 1,
            cluster_id: 0x0402,
            attribute_id: 0x0000,
            value: ZclValue::UInt16(2500),
        };

        let dp = ZigbeeChannel::process_attribute_report(&report, &lookup).unwrap();
        assert!((dp.value.as_f64().unwrap() - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_process_attribute_report_unmapped() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let lookup = HashMap::new(); // empty

        let report = AttributeReport {
            ieee_addr: 0x00124B0018ED1234,
            endpoint: 1,
            cluster_id: 0x0402,
            attribute_id: 0x0000,
            value: ZclValue::UInt16(2500),
        };

        assert!(ZigbeeChannel::process_attribute_report(&report, &lookup).is_none());
    }

    #[test]
    fn test_metadata() {
        let meta = ZigbeeChannel::metadata();
        assert_eq!(meta.name, "zigbee");
        assert!(meta.is_recommended);
        assert!(!meta.parameters.is_empty());
    }

    #[test]
    fn test_channel_creation() {
        let config = ZigbeeParamsConfig::default().to_config();
        let channel = ZigbeeChannel::new(config, 1, "test_zigbee".to_string(), vec![]);
        assert_eq!(channel.id(), 1);
        assert_eq!(channel.name(), "test_zigbee");
        assert_eq!(channel.protocol(), "zigbee");
        assert!(channel.is_event_driven());
        assert_eq!(channel.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn test_subscribe_returns_some() {
        let config = ZigbeeParamsConfig::default().to_config();
        let channel = ZigbeeChannel::new(config, 1, "test".to_string(), vec![]);
        assert!(channel.subscribe().is_some());
    }

    #[test]
    fn test_build_point_lookup_empty() {
        let points: Vec<PointConfig> = vec![];
        let lookup = ZigbeeChannel::build_point_lookup(&points);
        assert!(lookup.is_empty());
    }

    #[test]
    fn test_build_point_lookup_duplicate_key() {
        // Two points with the same (ieee, ep, cluster, attr) — later one overwrites.
        let points = vec![
            make_test_point(1, 0x00124B0018ED1234, 1, 0x0402, 0x0000),
            make_test_point(2, 0x00124B0018ED1234, 1, 0x0402, 0x0000),
        ];
        let lookup = ZigbeeChannel::build_point_lookup(&points);
        assert_eq!(lookup.len(), 1);
        let key = (0x00124B0018ED1234, 1, 0x0402, 0x0000);
        assert_eq!(lookup[&key].id, 2); // second insert wins
    }

    #[test]
    fn test_process_attribute_report_all_zcl_types() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let zcl_values_and_expected: Vec<(ZclValue, f64)> = vec![
            (ZclValue::Bool(true), 1.0),
            (ZclValue::Bool(false), 0.0),
            (ZclValue::UInt8(200), 200.0),
            (ZclValue::Int8(-42), -42.0),
            (ZclValue::UInt16(50000), 50000.0),
            (ZclValue::Int16(-1000), -1000.0),
            (ZclValue::UInt32(100_000), 100_000.0),
            (ZclValue::Int32(-99999), -99999.0),
            (ZclValue::Float(3.5), 3.5_f32 as f64),
            (ZclValue::Double(2.5), 2.5),
        ];

        for (i, (zcl_val, expected)) in zcl_values_and_expected.into_iter().enumerate() {
            let point_id = (100 + i) as u32;
            let attr_id = i as u16;
            let points = vec![make_test_point(point_id, 0xAA, 1, 0x0001, attr_id)];
            let lookup = ZigbeeChannel::build_point_lookup(&points);

            let report = AttributeReport {
                ieee_addr: 0xAA,
                endpoint: 1,
                cluster_id: 0x0001,
                attribute_id: attr_id,
                value: zcl_val,
            };

            let dp = ZigbeeChannel::process_attribute_report(&report, &lookup);
            assert!(
                dp.is_some(),
                "ZclValue variant #{i} should produce a DataPoint"
            );
            let dp = dp.unwrap();
            assert_eq!(dp.id, point_id);
            assert!(
                (dp.value.as_f64().unwrap() - expected).abs() < 0.01,
                "ZclValue variant #{i}: expected {expected}, got {:?}",
                dp.value.as_f64()
            );
        }
    }

    #[test]
    fn test_channel_initial_diagnostics() {
        let config = ZigbeeParamsConfig::default().to_config();
        let channel = ZigbeeChannel::new(config, 1, "test".to_string(), vec![]);
        let diag = channel.diagnostics.snapshot();
        assert_eq!(diag.read_count, 0);
        assert_eq!(diag.write_count, 0);
        assert_eq!(diag.error_count, 0);
    }

    #[test]
    fn test_channel_is_event_driven() {
        let config = ZigbeeParamsConfig::default().to_config();
        let channel = ZigbeeChannel::new(config, 1, "test".to_string(), vec![]);
        assert!(channel.is_event_driven());
    }
}
