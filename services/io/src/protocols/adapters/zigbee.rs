//! Zigbee Protocol Adapter
//!
//! Event-driven data collection from Zigbee devices via TCP gateway.
//!
//! ## Design Overview
//!
//! Zigbee devices communicate through a TCP gateway implementing Aether's Raw
//! framing. The adapter:
//! - Connects to the TCP gateway
//! - Decodes the Raw gateway frames into ZCL values
//! - Maps attribute reports to data points via (ieee_addr, endpoint, cluster, attr) lookup
//! - Queues data events for the unified channel task
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "host": "192.168.1.100",
//!   "port": 8888,
//!   "connect_timeout_ms": 5000
//! }
//! ```

use async_trait::async_trait;
use bytes::BytesMut;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};

use crate::protocols::ChannelRuntime;
use crate::protocols::adapters::zigbee_codec::{
    AttributeReport, FrameCodec, RawFrameCodec, ZigbeeFrame,
};
use crate::protocols::adapters::zigbee_config::ZigbeeConfig;
use crate::protocols::core::data::{DataBatch, DataPoint};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::TransformConfig;
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
    data_event_channel,
};

/// TCP read buffer size
const TCP_READ_BUF_SIZE: usize = 4096;

/// Lookup key for fast point resolution from attribute reports.
type PointLookupKey = (u64, u8, u16, u16); // (ieee_addr, endpoint, cluster_id, attribute_id)

const fn is_acquisition_point(point_type: aether_core::PointType) -> bool {
    matches!(
        point_type,
        aether_core::PointType::Telemetry | aether_core::PointType::Signal
    )
}

/// Zigbee-owned persisted point mapping schema.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ZigbeePointMapping {
    ieee_address: u64,
    endpoint: u8,
    cluster_id: u16,
    attribute_id: Option<u16>,
}

impl ZigbeePointMapping {
    fn acquisition_key(self) -> Option<PointLookupKey> {
        self.attribute_id.map(|attribute_id| {
            (
                self.ieee_address,
                self.endpoint,
                self.cluster_id,
                attribute_id,
            )
        })
    }

    fn identity_key(self) -> (u64, u8, u16, Option<u16>) {
        (
            self.ieee_address,
            self.endpoint,
            self.cluster_id,
            self.attribute_id,
        )
    }

    fn validate(&self, point_type: aether_core::PointType) -> Result<()> {
        if self.ieee_address == 0 {
            return Err(GatewayError::Config(
                "Zigbee ieee_address must be non-zero".to_string(),
            ));
        }
        if !(1..=240).contains(&self.endpoint) {
            return Err(GatewayError::Config(
                "Zigbee endpoint must be in 1..=240".to_string(),
            ));
        }
        if !is_acquisition_point(point_type) {
            return Err(GatewayError::Config(
                "Zigbee supports telemetry and signal mappings only; acknowledged writes are not implemented"
                    .to_string(),
            ));
        }
        if self.attribute_id.is_none() {
            return Err(GatewayError::Config(
                "Zigbee telemetry and signal mappings require attribute_id".to_string(),
            ));
        }
        Ok(())
    }
}

/// Fully validated Zigbee point consumed by the runtime.
#[derive(Debug, Clone)]
pub(crate) struct ZigbeePointConfig {
    id: u32,
    point_type: aether_core::PointType,
    address: ZigbeePointMapping,
    transform: TransformConfig,
}

impl ZigbeePointConfig {
    pub(crate) fn from_mapping(
        id: u32,
        point_type: aether_core::PointType,
        transform: TransformConfig,
        mapping: &str,
    ) -> Result<Self> {
        let address = serde_json::from_str::<ZigbeePointMapping>(mapping).map_err(|error| {
            GatewayError::Config(format!("invalid Zigbee point mapping: {error}"))
        })?;
        address.validate(point_type)?;
        if point_type == aether_core::PointType::Telemetry
            && (!transform.scale.is_finite() || !transform.offset.is_finite())
        {
            return Err(GatewayError::Config(
                "Zigbee telemetry transforms require finite scale and offset".to_string(),
            ));
        }
        Ok(Self {
            id,
            point_type,
            address,
            transform,
        })
    }
}

/// Validate a non-empty Zigbee mapping at the governed topology boundary.
pub(crate) fn validate_point_mapping(
    point_type: aether_core::PointType,
    mapping: &serde_json::Value,
) -> Result<()> {
    let mapping = ZigbeePointMapping::deserialize(mapping)
        .map_err(|error| GatewayError::Config(format!("invalid Zigbee point mapping: {error}")))?;
    mapping.validate(point_type)
}

fn validate_point_set(points: &[ZigbeePointConfig]) -> Result<()> {
    let mut identities = HashSet::with_capacity(points.len());
    let mut addresses = HashSet::with_capacity(points.len());
    for point in points {
        if !identities.insert((point.point_type, point.id)) {
            return Err(GatewayError::Config(format!(
                "duplicate Zigbee {:?} point ID {}",
                point.point_type, point.id
            )));
        }
        if !addresses.insert((point.address.identity_key(), point.point_type)) {
            return Err(GatewayError::Config(format!(
                "ambiguous Zigbee {:?} mapping for IEEE 0x{:016X}, endpoint {}, cluster 0x{:04X}, attribute {:?}",
                point.point_type,
                point.address.ieee_address,
                point.address.endpoint,
                point.address.cluster_id,
                point.address.attribute_id
            )));
        }
    }
    Ok(())
}

/// Zigbee Channel implementation.
///
/// Event-driven channel that connects to a Zigbee TCP gateway and decodes
/// ZCL attribute reports into data points.
pub(crate) struct ZigbeeChannel {
    /// Channel configuration
    config: ZigbeeConfig,
    /// Channel ID
    channel_id: u32,
    /// Point configurations
    points: Vec<ZigbeePointConfig>,
    /// Event loop task handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection state (atomic for lock-free access)
    state: Arc<AtomicU8>,
    /// Event sender for the unified channel task.
    event_tx: DataEventSender,
    /// Sole event receiver, taken once by the unified channel task.
    event_rx: Option<DataEventReceiver>,
    /// Diagnostics counters
    diagnostics: Arc<AtomicDiagnostics>,
}

impl ZigbeeChannel {
    /// Create a new Zigbee channel.
    pub(crate) fn new(
        config: ZigbeeConfig,
        channel_id: u32,
        points: Vec<ZigbeePointConfig>,
    ) -> Result<Self> {
        validate_point_set(&points)?;
        let (event_tx, event_rx) = data_event_channel();

        Ok(Self {
            config,
            channel_id,
            points,
            event_loop_handle: None,
            state: Arc::new(AtomicU8::new(ConnectionState::Disconnected as u8)),
            event_tx,
            event_rx: Some(event_rx),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
        })
    }

    /// Build the point lookup map from configured points.
    fn build_point_lookup(
        points: &[ZigbeePointConfig],
    ) -> HashMap<PointLookupKey, Vec<ZigbeePointConfig>> {
        let mut map = HashMap::with_capacity(points.len());
        for point in points
            .iter()
            .filter(|point| is_acquisition_point(point.point_type))
        {
            if let Some(key) = point.address.acquisition_key() {
                map.entry(key).or_insert_with(Vec::new).push(point.clone());
            }
        }
        map
    }

    fn create_codec() -> Box<dyn FrameCodec> {
        Box::new(RawFrameCodec)
    }

    /// Set connection state and queue an event.
    fn set_state(state: &AtomicU8, event_tx: &DataEventSender, new_state: ConnectionState) {
        state.store(new_state as u8, Ordering::SeqCst);
        let _ = event_tx.try_send(DataEvent::ConnectionChanged(new_state));
    }

    /// Process an attribute report into a DataPoint.
    fn process_attribute_report(
        report: &AttributeReport,
        lookup: &HashMap<PointLookupKey, Vec<ZigbeePointConfig>>,
    ) -> Vec<DataPoint> {
        let key = (
            report.ieee_addr,
            report.endpoint,
            report.cluster_id,
            report.attribute_id,
        );

        let Some(points) = lookup.get(&key) else {
            return Vec::new();
        };
        let Some(raw_value) = report.value.to_f64() else {
            warn!(
                ieee_addr = report.ieee_addr,
                endpoint = report.endpoint,
                cluster_id = report.cluster_id,
                attribute_id = report.attribute_id,
                "ignoring non-numeric Zigbee attribute report"
            );
            return Vec::new();
        };
        if !raw_value.is_finite() {
            warn!(
                ieee_addr = report.ieee_addr,
                endpoint = report.endpoint,
                cluster_id = report.cluster_id,
                attribute_id = report.attribute_id,
                "ignoring non-finite Zigbee attribute report"
            );
            return Vec::new();
        }
        points
            .iter()
            .filter_map(|point| {
                let transformed = if point.point_type == aether_core::PointType::Signal {
                    let active = raw_value != 0.0;
                    f64::from(u8::from(if point.transform.reverse {
                        !active
                    } else {
                        active
                    }))
                } else {
                    point.transform.apply(raw_value)
                };
                if transformed.is_finite() {
                    Some(DataPoint::new(point.id, point.point_type, transformed))
                } else {
                    warn!(
                        point_id = point.id,
                        "ignoring Zigbee sample whose transform produced a non-finite value"
                    );
                    None
                }
            })
            .collect()
    }

    /// Run the TCP event loop — reads frames from the gateway and dispatches events.
    async fn run_event_loop(
        mut stream: TcpStream,
        codec: Box<dyn FrameCodec>,
        channel_id: u32,
        state: Arc<AtomicU8>,
        event_tx: DataEventSender,
        diagnostics: Arc<AtomicDiagnostics>,
        point_lookup: HashMap<PointLookupKey, Vec<ZigbeePointConfig>>,
    ) {
        info!(channel_id, "Zigbee event loop started");

        let mut buf = BytesMut::with_capacity(TCP_READ_BUF_SIZE);

        loop {
            // Read data from TCP stream
            match stream.read_buf(&mut buf).await {
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
                    let _ = event_tx.try_send(DataEvent::Error(e.to_string()));
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
        point_lookup: &HashMap<PointLookupKey, Vec<ZigbeePointConfig>>,
    ) {
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                let data_points = Self::process_attribute_report(report, point_lookup);
                if !data_points.is_empty() {
                    let mut batch = DataBatch::with_capacity(data_points.len());
                    for data_point in data_points {
                        batch.add(data_point);
                    }
                    diagnostics.add_read(batch.len() as u64);
                    let _ = event_tx.try_send(DataEvent::DataUpdate(batch));
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
    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        if self
            .event_loop_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Ok(());
        }
        self.event_loop_handle.take();
        Self::set_state(&self.state, &self.event_tx, ConnectionState::Connecting);

        let connect_result = tokio::time::timeout(
            self.config.connect_timeout,
            TcpStream::connect((self.config.host.as_str(), self.config.port)),
        )
        .await;
        let stream = match connect_result {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                Self::set_state(&self.state, &self.event_tx, ConnectionState::Error);
                return Err(GatewayError::Connection(format!(
                    "TCP connect to {}:{} failed: {error}",
                    self.config.host, self.config.port
                )));
            },
            Err(_) => {
                Self::set_state(&self.state, &self.event_tx, ConnectionState::Error);
                return Err(GatewayError::ConnectionTimeout(
                    self.config.connect_timeout.as_millis() as u64,
                ));
            },
        };

        // Disable Nagle's algorithm for lower latency
        if let Err(error) = stream.set_nodelay(true) {
            Self::set_state(&self.state, &self.event_tx, ConnectionState::Error);
            return Err(GatewayError::Connection(format!(
                "Failed to set TCP_NODELAY: {error}"
            )));
        }

        info!(
            channel_id = self.channel_id,
            host = %self.config.host,
            port = self.config.port,
            "Connected to Zigbee TCP gateway"
        );

        // Create codec
        let codec = Self::create_codec();

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
                stream,
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

        Self::set_state(&self.state, &self.event_tx, ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "Zigbee channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        // Event-driven protocol — return empty batch.
        // Data is delivered through the runtime's event receiver.
        PollResult::success(DataBatch::new())
    }

    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        self.event_rx.take()
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
}

impl HasMetadata for ZigbeeChannel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "zigbee",
            display_name: "Zigbee (Aether Raw TCP Gateway)",
            description: "Read-only Zigbee ZCL telemetry through the Aether Raw TCP gateway framing",
            is_recommended: true,
            example_config: json!({
                "host": "192.168.1.100",
                "port": 8888,
                "connect_timeout_ms": 5000
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
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "TCP connection timeout",
                    ParameterType::Integer,
                    json!(5000),
                ),
            ],
        }
    }
}

impl std::fmt::Debug for ZigbeeChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZigbeeChannel")
            .field("channel_id", &self.channel_id)
            .field("host", &self.config.host)
            .field("port", &self.config.port)
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
    use aether_core::PointType;

    fn make_test_point(
        id: u32,
        ieee_address: u64,
        endpoint: u8,
        cluster_id: u16,
        attribute_id: u16,
    ) -> ZigbeePointConfig {
        ZigbeePointConfig {
            id,
            point_type: PointType::Telemetry,
            address: ZigbeePointMapping {
                ieee_address,
                endpoint,
                cluster_id,
                attribute_id: Some(attribute_id),
            },
            transform: TransformConfig::linear(1.0, 0.0),
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
        assert_eq!(lookup[&key][0].id, 1);
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

        let samples = ZigbeeChannel::process_attribute_report(&report, &lookup);
        assert_eq!(samples.len(), 1);
        let dp = &samples[0];
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

        let samples = ZigbeeChannel::process_attribute_report(&report, &lookup);
        let dp = &samples[0];
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

        assert!(ZigbeeChannel::process_attribute_report(&report, &lookup).is_empty());
    }

    #[test]
    fn test_process_attribute_report_rejects_non_numeric_values() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let point = make_test_point(1, 0x00124B0018ED1234, 1, 0x0402, 0x0000);
        let lookup = ZigbeeChannel::build_point_lookup(&[point]);

        for value in [
            ZclValue::String("not-a-number".to_string()),
            ZclValue::Bytes(vec![1, 2, 3]),
        ] {
            let report = AttributeReport {
                ieee_addr: 0x00124B0018ED1234,
                endpoint: 1,
                cluster_id: 0x0402,
                attribute_id: 0x0000,
                value,
            };

            assert!(
                ZigbeeChannel::process_attribute_report(&report, &lookup).is_empty(),
                "non-numeric Zigbee values must not become acquisition samples"
            );
        }
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
        let config = ZigbeeParamsConfig::default().into_config().unwrap();
        let channel = ZigbeeChannel::new(config, 1, vec![]).unwrap();
        assert!(channel.is_event_driven());
        assert_eq!(channel.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn test_event_receiver_is_taken_once() {
        let config = ZigbeeParamsConfig::default().into_config().unwrap();
        let mut channel = ZigbeeChannel::new(config, 1, vec![]).unwrap();
        assert!(channel.take_event_receiver().is_some());
        assert!(channel.take_event_receiver().is_none());
    }

    #[test]
    fn test_build_point_lookup_empty() {
        let points: Vec<ZigbeePointConfig> = vec![];
        let lookup = ZigbeeChannel::build_point_lookup(&points);
        assert!(lookup.is_empty());
    }

    #[test]
    fn test_build_point_lookup_duplicate_key() {
        let points = vec![
            make_test_point(1, 0x00124B0018ED1234, 1, 0x0402, 0x0000),
            make_test_point(2, 0x00124B0018ED1234, 1, 0x0402, 0x0000),
        ];
        let config = ZigbeeParamsConfig::default().into_config().unwrap();
        assert!(ZigbeeChannel::new(config, 1, points).is_err());
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

            let samples = ZigbeeChannel::process_attribute_report(&report, &lookup);
            assert!(
                !samples.is_empty(),
                "ZclValue variant #{i} should produce a DataPoint"
            );
            let dp = &samples[0];
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
        let config = ZigbeeParamsConfig::default().into_config().unwrap();
        let channel = ZigbeeChannel::new(config, 1, vec![]).unwrap();
        let diag = channel.diagnostics.snapshot();
        assert_eq!(diag.read_count, 0);
        assert_eq!(diag.write_count, 0);
        assert_eq!(diag.error_count, 0);
    }

    #[test]
    fn test_channel_is_event_driven() {
        let config = ZigbeeParamsConfig::default().into_config().unwrap();
        let channel = ZigbeeChannel::new(config, 1, vec![]).unwrap();
        assert!(channel.is_event_driven());
    }

    #[test]
    fn point_mapping_is_adapter_owned_and_strict() {
        let mapping = r#"{
            "ieee_address":5149012980814388,
            "endpoint":1,
            "cluster_id":1026,
            "attribute_id":0
        }"#;
        let point = ZigbeePointConfig::from_mapping(
            7,
            PointType::Telemetry,
            TransformConfig::linear(0.01, 0.0),
            mapping,
        )
        .unwrap();
        assert_eq!(
            point.address.acquisition_key(),
            Some((5149012980814388, 1, 1026, 0))
        );

        for invalid in [
            r#"{"ieee_address":0,"endpoint":1,"cluster_id":6,"attribute_id":0}"#,
            r#"{"ieee_address":1,"endpoint":0,"cluster_id":6,"attribute_id":0}"#,
            r#"{"ieee_address":1,"endpoint":241,"cluster_id":6,"attribute_id":0}"#,
            r#"{"ieee_address":1,"endpoint":1,"cluster_id":6}"#,
            r#"{"ieee_address":1,"endpoint":1,"cluster_id":6,"attribute_id":0,"extra":1}"#,
        ] {
            assert!(
                ZigbeePointConfig::from_mapping(
                    7,
                    PointType::Telemetry,
                    TransformConfig::default(),
                    invalid,
                )
                .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn command_mappings_fail_closed() {
        let adjustment = ZigbeePointConfig::from_mapping(
            1,
            PointType::Adjustment,
            TransformConfig::default(),
            r#"{"ieee_address":1,"endpoint":1,"cluster_id":6,"attribute_id":0}"#,
        );
        assert!(adjustment.is_err());

        let control = ZigbeePointConfig::from_mapping(
            1,
            PointType::Control,
            TransformConfig::default(),
            r#"{"ieee_address":1,"endpoint":1,"cluster_id":8}"#,
        );
        assert!(control.is_err());
    }

    #[test]
    fn reversed_signal_is_applied() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let mut point = make_test_point(1, 1, 1, 6, 0);
        point.point_type = PointType::Signal;
        point.transform.reverse = true;
        let lookup = ZigbeeChannel::build_point_lookup(&[point]);
        let report = AttributeReport {
            ieee_addr: 1,
            endpoint: 1,
            cluster_id: 6,
            attribute_id: 0,
            value: ZclValue::Bool(false),
        };
        let samples = ZigbeeChannel::process_attribute_report(&report, &lookup);
        let sample = &samples[0];
        assert_eq!(sample.value.as_f64(), Some(1.0));
    }

    #[test]
    fn acquisition_fans_out_and_rejects_non_finite_values() {
        use crate::protocols::adapters::zigbee_codec::{AttributeReport, ZclValue};

        let telemetry = make_test_point(1, 1, 1, 6, 0);
        let mut signal = make_test_point(2, 1, 1, 6, 0);
        signal.point_type = PointType::Signal;
        let lookup = ZigbeeChannel::build_point_lookup(&[telemetry, signal]);

        let report = |value| AttributeReport {
            ieee_addr: 1,
            endpoint: 1,
            cluster_id: 6,
            attribute_id: 0,
            value,
        };
        let samples =
            ZigbeeChannel::process_attribute_report(&report(ZclValue::Bool(true)), &lookup);
        assert_eq!(samples.len(), 2);
        assert!(
            ZigbeeChannel::process_attribute_report(&report(ZclValue::Double(f64::NAN)), &lookup)
                .is_empty()
        );

        let mut overflowing = make_test_point(3, 2, 1, 6, 0);
        overflowing.transform.scale = f64::MAX;
        let overflow_lookup = ZigbeeChannel::build_point_lookup(&[overflowing]);
        let overflow_report = AttributeReport {
            ieee_addr: 2,
            endpoint: 1,
            cluster_id: 6,
            attribute_id: 0,
            value: ZclValue::Double(2.0),
        };
        assert!(
            ZigbeeChannel::process_attribute_report(&overflow_report, &overflow_lookup).is_empty()
        );
    }
}
