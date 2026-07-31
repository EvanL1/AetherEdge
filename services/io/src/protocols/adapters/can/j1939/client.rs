//! J1939 Protocol Client Implementation
//!
//! Implements the protocol layer's Protocol traits for J1939/CAN communication.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use aether_core::PointType;
use arc_swap::ArcSwapOption;
use serde::Deserialize;
use socketcan::{EmbeddedFrame, Id, tokio::CanSocket};
use tokio::task::JoinHandle;
use voltage_j1939::{database_stats, decode_frame, extract_source_address, get_spn_def};

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::TransformConfig;
use crate::protocols::core::traits::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, DataEvent,
    DataEventReceiver, DataEventSender, Diagnostics, PollResult, WriteResult, data_event_channel,
};
use crate::protocols::runtime::ChannelRuntime;

// ============================================================================
// Configuration
// ============================================================================

/// J1939 client configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct J1939Config {
    /// CAN interface name (e.g., "can0").
    #[serde(alias = "can_interface")]
    pub device: String,

    /// Source address of the target device (ECU address).
    pub source_address: u8,
}

impl Default for J1939Config {
    fn default() -> Self {
        Self {
            device: "can0".to_string(),
            source_address: 0x00,
        }
    }
}

impl J1939Config {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.device.trim().is_empty() {
            return Err(GatewayError::Config(
                "J1939 device must be a non-empty CAN interface name".to_string(),
            ));
        }
        Ok(())
    }
}

/// One governed IO point bound to an SAE J1939 SPN.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct J1939PointMapping {
    spn: u32,
    active_raw_value: Option<u64>,
}

impl J1939PointMapping {
    fn validate(&self, point_id: u32, point_type: PointType) -> Result<Option<u64>> {
        if self.spn == 0 {
            return Err(GatewayError::Config(format!(
                "J1939 {} point {point_id} requires a non-zero SPN",
                point_type.as_str()
            )));
        }
        let definition = get_spn_def(self.spn).ok_or_else(|| {
            GatewayError::Config(format!(
                "J1939 {} point {point_id} references unsupported SPN {}",
                point_type.as_str(),
                self.spn
            ))
        })?;
        match point_type {
            PointType::Telemetry => {
                if self.active_raw_value.is_some() {
                    return Err(GatewayError::Config(format!(
                        "J1939 telemetry point {point_id} must not define active_raw_value"
                    )));
                }
                Ok(None)
            },
            PointType::Signal => {
                let active_raw_value = self.active_raw_value.ok_or_else(|| {
                    GatewayError::Config(format!(
                        "J1939 signal point {point_id} requires active_raw_value"
                    ))
                })?;
                let reserved_threshold = 1_u64
                    .checked_shl(u32::from(definition.bit_length))
                    .and_then(|max_exclusive| max_exclusive.checked_sub(2))
                    .ok_or_else(|| {
                        GatewayError::Config(format!(
                            "J1939 SPN {} has an unsupported bit width",
                            self.spn
                        ))
                    })?;
                if active_raw_value >= reserved_threshold {
                    return Err(GatewayError::Config(format!(
                        "J1939 signal point {point_id} active_raw_value {active_raw_value} is reserved or outside SPN {}",
                        self.spn
                    )));
                }
                Ok(Some(active_raw_value))
            },
            PointType::Control | PointType::Adjustment => Err(GatewayError::Config(format!(
                "J1939 does not support {} point mappings",
                point_type.as_str()
            ))),
        }
    }
}

/// One validated point mapping projected into the runtime.
#[derive(Debug, Clone)]
pub struct J1939PointConfig {
    pub point_id: u32,
    pub point_type: PointType,
    pub spn: u32,
    pub active_raw_value: Option<u64>,
    pub transform: TransformConfig,
}

impl J1939PointConfig {
    pub(crate) fn from_mapping(
        point_id: u32,
        point_type: PointType,
        mapping: &serde_json::Value,
        transform: TransformConfig,
    ) -> Result<Self> {
        let mapping = J1939PointMapping::deserialize(mapping).map_err(|error| {
            GatewayError::Config(format!(
                "invalid J1939 mapping for {} point {point_id}: {error}",
                point_type.as_str()
            ))
        })?;
        let active_raw_value = mapping.validate(point_id, point_type)?;
        if !transform.scale.is_finite() || !transform.offset.is_finite() {
            return Err(GatewayError::Config(format!(
                "J1939 {} point {point_id} has a non-finite transform",
                point_type.as_str()
            )));
        }
        Ok(Self {
            point_id,
            point_type,
            spn: mapping.spn,
            active_raw_value,
            transform,
        })
    }

    /// Decode the adapter-owned point mapping from the runtime snapshot.
    pub fn from_json(
        point_id: u32,
        point_type: PointType,
        mapping: Option<&str>,
        transform: TransformConfig,
    ) -> Result<Self> {
        let mapping = mapping.ok_or_else(|| {
            GatewayError::Config(format!(
                "J1939 {} point {point_id} requires a protocol mapping",
                point_type.as_str()
            ))
        })?;
        let mapping: serde_json::Value = serde_json::from_str(mapping).map_err(|error| {
            GatewayError::Config(format!(
                "invalid J1939 mapping for {} point {point_id}: {error}",
                point_type.as_str()
            ))
        })?;
        Self::from_mapping(point_id, point_type, &mapping, transform)
    }
}

/// Validate a non-empty J1939 mapping at the governed topology boundary.
pub(crate) fn validate_point_mapping(
    point_type: PointType,
    mapping: &serde_json::Value,
) -> Result<()> {
    let mapping = J1939PointMapping::deserialize(mapping)
        .map_err(|error| GatewayError::Config(format!("invalid J1939 point mapping: {error}")))?;
    mapping.validate(0, point_type).map(|_| ())
}

// ============================================================================
// J1939Client
// ============================================================================

/// J1939 protocol client.
///
/// Implements event-driven communication over CAN bus using the SAE J1939 protocol.
/// Uses `voltage_j1939` crate for protocol parsing and SPN database.
pub struct J1939Client {
    config: J1939Config,

    // Connection state (lock-free)
    connection_state: Arc<AtomicU8>,

    // Statistics (lock-free)
    read_count: Arc<AtomicU64>,
    error_count: Arc<AtomicU64>,
    last_error: Arc<ArcSwapOption<String>>,

    // Tasks
    socket: Option<CanSocket>,
    receive_handle: Option<JoinHandle<()>>,

    // Event queue for the unified channel task.
    event_tx: DataEventSender,
    event_rx: Option<DataEventReceiver>,

    // Explicit SPN-to-point projection from the immutable runtime snapshot.
    point_lookup: Arc<HashMap<u32, Vec<J1939PointConfig>>>,
}

impl J1939Client {
    /// Create a new J1939 client with the given configuration.
    pub fn new(config: J1939Config, points: Vec<J1939PointConfig>) -> Result<Self> {
        config.validate()?;
        let mut point_lookup = HashMap::<u32, Vec<J1939PointConfig>>::new();
        for point in points {
            if !matches!(point.point_type, PointType::Telemetry | PointType::Signal) {
                return Err(GatewayError::Config(format!(
                    "J1939 SPN {} targets unsupported {} point {}",
                    point.spn,
                    point.point_type.as_str(),
                    point.point_id
                )));
            }
            point_lookup.entry(point.spn).or_default().push(point);
        }

        let (event_tx, event_rx) = data_event_channel();

        Ok(Self {
            config,
            connection_state: Arc::new(AtomicU8::new(ConnectionState::Disconnected.into())),
            read_count: Arc::new(AtomicU64::new(0)),
            error_count: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(ArcSwapOption::empty()),
            socket: None,
            receive_handle: None,
            event_tx,
            event_rx: Some(event_rx),
            point_lookup: Arc::new(point_lookup),
        })
    }

    fn project_samples(
        point_lookup: &HashMap<u32, Vec<J1939PointConfig>>,
        samples: impl IntoIterator<Item = (u32, f64, u64)>,
    ) -> DataBatch {
        let mut batch = DataBatch::new();
        for (spn, sample, raw_sample) in samples {
            if !sample.is_finite() {
                continue;
            }
            let Some(points) = point_lookup.get(&spn) else {
                continue;
            };
            for point in points {
                let value = match point.point_type {
                    PointType::Telemetry => {
                        let transformed = point.transform.apply(sample);
                        if !transformed.is_finite() {
                            continue;
                        }
                        Value::Float(transformed)
                    },
                    PointType::Signal => {
                        let Some(active_raw_value) = point.active_raw_value else {
                            continue;
                        };
                        Value::Bool(point.transform.apply_bool(raw_sample == active_raw_value))
                    },
                    PointType::Control | PointType::Adjustment => continue,
                };
                batch.add(DataPoint::new(point.point_id, point.point_type, value));
            }
        }
        batch
    }

    /// Start the receive task.
    fn start_receive_task(&mut self, socket: CanSocket) {
        let source_address = self.config.source_address;
        let connection_state = Arc::clone(&self.connection_state);
        let point_lookup = Arc::clone(&self.point_lookup);
        let read_count = Arc::clone(&self.read_count);
        let error_count = Arc::clone(&self.error_count);
        let last_error = Arc::clone(&self.last_error);
        let event_tx = self.event_tx.clone();

        let handle = tokio::spawn(async move {
            loop {
                match socket.read_frame().await {
                    Ok(frame) => {
                        // J1939 uses extended CAN IDs (29-bit)
                        let can_id = match frame.id() {
                            Id::Extended(ext_id) => ext_id.as_raw(),
                            Id::Standard(_) => continue, // Skip standard frames
                        };
                        let sa = extract_source_address(can_id);

                        // Filter by source address
                        if sa != source_address {
                            continue;
                        }

                        // Use voltage_j1939 to decode the frame
                        let decoded_spns = decode_frame(can_id, frame.data());
                        if decoded_spns.is_empty() {
                            continue;
                        }

                        let batch = Self::project_samples(
                            &point_lookup,
                            decoded_spns
                                .into_iter()
                                .map(|sample| (sample.spn, sample.value, sample.raw_value)),
                        );

                        if !batch.is_empty() {
                            read_count.fetch_add(1, Ordering::Relaxed);

                            // Queue the update without blocking the receive loop.
                            let _ = event_tx.try_send(DataEvent::DataUpdate(batch));
                        }
                    },
                    Err(e) => {
                        last_error.store(Some(Arc::new(format!("CAN read error: {}", e))));
                        error_count.fetch_add(1, Ordering::Relaxed);
                        connection_state.store(ConnectionState::Error.into(), Ordering::Release);
                        let _ = event_tx.try_send(DataEvent::Error(e.to_string()));
                        let _ =
                            event_tx.try_send(DataEvent::ConnectionChanged(ConnectionState::Error));
                        break;
                    },
                }
            }
        });

        self.receive_handle = Some(handle);
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl HasMetadata for J1939Client {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "j1939",
            display_name: "SAE J1939",
            description: "SAE J1939 telemetry over a Linux CAN interface",
            is_recommended: false,
            example_config: serde_json::json!({
                "device": "can0",
                "source_address": 0
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "device",
                    "CAN Interface",
                    "Linux CAN interface name",
                    ParameterType::String,
                )
                .with_min_length(1),
                ParameterMetadata::optional(
                    "source_address",
                    "Source Address",
                    "J1939 ECU source address to accept",
                    ParameterType::Integer,
                    serde_json::json!(0),
                )
                .with_integer_range(0, u8::MAX.into()),
            ],
        }
    }
}

impl J1939Client {
    fn name(&self) -> &'static str {
        "J1939"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::EventDriven]
    }

    fn supports_client(&self) -> bool {
        true
    }

    fn supports_server(&self) -> bool {
        false
    }

    fn version(&self) -> &'static str {
        "SAE J1939-21"
    }
}

#[async_trait::async_trait]
impl ChannelRuntime for J1939Client {
    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        ProtocolClient::connect(self).await
    }

    async fn disconnect(&mut self) -> Result<()> {
        ProtocolClient::disconnect(self).await
    }

    async fn poll_once(&mut self) -> PollResult {
        ProtocolClient::poll_once(self).await
    }

    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        EventDrivenProtocol::take_event_receiver(self)
    }

    async fn start_events(&mut self) -> Result<()> {
        EventDrivenProtocol::start(self).await
    }

    async fn stop_events(&mut self) -> Result<()> {
        EventDrivenProtocol::stop(self).await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        Protocol::diagnostics(self).await
    }

    fn connection_state(&self) -> ConnectionState {
        Protocol::connection_state(self)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use voltage_j1939::parse_can_id;

    #[test]
    fn test_parse_can_id() {
        // EEC1 from SA=0x00: CAN ID = 0x0CF00400
        let id = parse_can_id(0x0CF00400);
        assert_eq!(id.priority, 3);
        assert_eq!(id.pgn, 61444);
        assert_eq!(id.source_address, 0x00);

        // ET1 from SA=0x00: CAN ID = 0x18FEEE00
        let id = parse_can_id(0x18FEEE00);
        assert_eq!(id.priority, 6);
        assert_eq!(id.pgn, 65262);
        assert_eq!(id.source_address, 0x00);
    }

    #[test]
    fn test_decode_frame() {
        // EEC1 frame
        let can_id = 0x0CF00400;
        let data = [0x00, 0x00, 0x00, 0x20, 0x4E, 0x00, 0x00, 0x00];
        let decoded = decode_frame(can_id, &data);
        assert!(!decoded.is_empty());

        // Find engine speed (SPN 190)
        let engine_speed = decoded.iter().find(|d| d.spn == 190);
        assert!(engine_speed.is_some());
        assert_eq!(engine_speed.unwrap().value, 2500.0);
    }

    #[test]
    fn test_config_default() {
        let config = J1939Config::default();
        assert_eq!(config.device, "can0");
        assert_eq!(config.source_address, 0x00);
    }

    #[test]
    fn config_rejects_retired_no_effect_fields() {
        assert!(
            serde_json::from_value::<J1939Config>(
                serde_json::json!({"device": "can0", "pgn_list": [61444]})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<J1939Config>(
                serde_json::json!({"device": "can0", "request_interval_ms": 1000})
            )
            .is_err()
        );
    }

    #[test]
    fn test_client_creation() {
        let config = J1939Config::default();
        let client = J1939Client::new(
            config,
            vec![J1939PointConfig {
                point_id: 7,
                point_type: PointType::Telemetry,
                spn: 190,
                active_raw_value: None,
                transform: TransformConfig::default(),
            }],
        )
        .unwrap();
        assert_eq!(client.name(), "J1939");
        assert_eq!(client.supported_modes(), &[CommunicationMode::EventDriven]);
    }

    #[test]
    fn projects_only_configured_spns_to_governed_point_ids() {
        let point_lookup = HashMap::from([
            (
                190,
                vec![J1939PointConfig {
                    point_id: 7,
                    point_type: PointType::Telemetry,
                    spn: 190,
                    active_raw_value: None,
                    transform: TransformConfig::default(),
                }],
            ),
            (
                110,
                vec![J1939PointConfig {
                    point_id: 8,
                    point_type: PointType::Signal,
                    spn: 110,
                    active_raw_value: Some(130),
                    transform: TransformConfig::default(),
                }],
            ),
        ]);
        let batch = J1939Client::project_samples(
            &point_lookup,
            [(190, 2_500.0, 20_000), (110, 90.0, 130), (999, 1.0, 1)],
        );

        assert_eq!(batch.len(), 2);
        let telemetry = batch.iter().find(|point| point.id == 7).unwrap();
        assert_eq!(telemetry.point_type, PointType::Telemetry);
        assert_eq!(telemetry.value, Value::Float(2_500.0));
        let signal = batch.iter().find(|point| point.id == 8).unwrap();
        assert_eq!(signal.point_type, PointType::Signal);
        assert_eq!(signal.value, Value::Bool(true));
    }

    #[test]
    fn projection_rejects_non_finite_raw_and_transformed_samples() {
        let point_lookup = HashMap::from([(
            190,
            vec![J1939PointConfig {
                point_id: 7,
                point_type: PointType::Telemetry,
                spn: 190,
                active_raw_value: None,
                transform: TransformConfig {
                    scale: f64::MAX,
                    ..TransformConfig::default()
                },
            }],
        )]);

        assert!(J1939Client::project_samples(&point_lookup, [(190, f64::NAN, 0)]).is_empty());
        assert!(J1939Client::project_samples(&point_lookup, [(190, 2.0, 16)]).is_empty());
    }

    #[test]
    fn rejects_writable_point_maps() {
        assert!(
            J1939Client::new(
                J1939Config::default(),
                vec![J1939PointConfig {
                    point_id: 7,
                    point_type: PointType::Control,
                    spn: 190,
                    active_raw_value: None,
                    transform: TransformConfig::default(),
                }]
            )
            .is_err()
        );
    }

    #[test]
    fn point_mapping_is_strict_and_required() {
        let point = J1939PointConfig::from_json(
            7,
            PointType::Telemetry,
            Some(r#"{"spn":190}"#),
            TransformConfig::default(),
        )
        .unwrap();
        assert_eq!(point.spn, 190);
        let signal = J1939PointConfig::from_json(
            8,
            PointType::Signal,
            Some(r#"{"spn":110,"active_raw_value":130}"#),
            TransformConfig::default(),
        )
        .unwrap();
        assert_eq!(signal.active_raw_value, Some(130));
        assert!(
            J1939PointConfig::from_json(
                7,
                PointType::Telemetry,
                Some(r#"{"spn":190,"pgn":61444}"#),
                TransformConfig::default(),
            )
            .is_err()
        );
        for invalid in [r#"{"spn":999999}"#, r#"{"spn":190,"active_raw_value":1}"#] {
            assert!(
                J1939PointConfig::from_json(
                    7,
                    PointType::Telemetry,
                    Some(invalid),
                    TransformConfig::default(),
                )
                .is_err()
            );
        }
        for invalid in [r#"{"spn":110}"#, r#"{"spn":110,"active_raw_value":254}"#] {
            assert!(
                J1939PointConfig::from_json(
                    8,
                    PointType::Signal,
                    Some(invalid),
                    TransformConfig::default(),
                )
                .is_err()
            );
        }
        assert!(
            J1939PointConfig::from_json(
                7,
                PointType::Telemetry,
                Some(r#"{"spn":0}"#),
                TransformConfig::default(),
            )
            .is_err()
        );
        assert!(
            J1939PointConfig::from_json(7, PointType::Telemetry, None, TransformConfig::default(),)
                .is_err()
        );
    }
}
