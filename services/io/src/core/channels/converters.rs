//! Point configuration converters
//!
//! Convert io RuntimeChannelConfig to PointConfig/CanPoint.
//!
//! This module handles the "translation" between io's configuration format
//! and the protocol layer's point configuration format.

use crate::core::channels::RuntimeChannelConfig;
use crate::core::config::{AdjustmentPoint, ControlPoint, Point, SignalPoint, TelemetryPoint};
#[cfg(any(
    feature = "modbus",
    feature = "iec104",
    feature = "opcua",
    feature = "ble",
    feature = "zigbee",
    all(feature = "can", target_os = "linux"),
    all(feature = "j1939", target_os = "linux")
))]
use crate::protocols::core::error::{GatewayError, Result as GatewayResult};
#[cfg(any(feature = "modbus", feature = "iec104", feature = "opcua"))]
use crate::protocols::core::point::PointConfig;
#[cfg(any(
    feature = "modbus",
    feature = "iec104",
    feature = "opcua",
    feature = "ble",
    feature = "zigbee"
))]
use crate::protocols::core::point::TransformConfig;
use aether_core::PointType;

#[cfg(feature = "iec104")]
use crate::protocols::adapters::iec104::Iec104Address;
#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus::ModbusAddress;
#[cfg(feature = "opcua")]
use crate::protocols::adapters::opcua::OpcUaAddress;

#[cfg(all(feature = "can", target_os = "linux"))]
use crate::protocols::adapters::can::CanPoint;

// ============================================================================
// Point conversion trait + helpers
// ============================================================================

/// Trait for extracting common data needed during point -> PointConfig conversion.
///
/// Each concrete point type (Telemetry, Signal, Control, Adjustment) has different
/// transform parameters (scale/offset/reverse), but they all share the same
/// conversion pattern: base point + point type + transform -> PointConfig.
trait PointConvertible {
    fn base(&self) -> &Point;
    fn point_type() -> PointType;
    #[cfg(any(
        feature = "modbus",
        feature = "iec104",
        feature = "opcua",
        feature = "ble",
        feature = "zigbee"
    ))]
    fn transform(&self) -> TransformConfig;
}

impl PointConvertible for TelemetryPoint {
    fn base(&self) -> &Point {
        &self.base
    }
    fn point_type() -> PointType {
        PointType::Telemetry
    }
    #[cfg(any(
        feature = "modbus",
        feature = "iec104",
        feature = "opcua",
        feature = "ble",
        feature = "zigbee"
    ))]
    fn transform(&self) -> TransformConfig {
        TransformConfig {
            scale: self.scale,
            offset: self.offset,
            reverse: self.reverse,
            ..Default::default()
        }
    }
}

impl PointConvertible for SignalPoint {
    fn base(&self) -> &Point {
        &self.base
    }
    fn point_type() -> PointType {
        PointType::Signal
    }
    #[cfg(any(
        feature = "modbus",
        feature = "iec104",
        feature = "opcua",
        feature = "ble",
        feature = "zigbee"
    ))]
    fn transform(&self) -> TransformConfig {
        TransformConfig {
            reverse: self.reverse,
            ..Default::default()
        }
    }
}

impl PointConvertible for ControlPoint {
    fn base(&self) -> &Point {
        &self.base
    }
    fn point_type() -> PointType {
        PointType::Control
    }
    #[cfg(any(
        feature = "modbus",
        feature = "iec104",
        feature = "opcua",
        feature = "ble",
        feature = "zigbee"
    ))]
    fn transform(&self) -> TransformConfig {
        TransformConfig {
            reverse: self.reverse,
            ..Default::default()
        }
    }
}

impl PointConvertible for AdjustmentPoint {
    fn base(&self) -> &Point {
        &self.base
    }
    fn point_type() -> PointType {
        PointType::Adjustment
    }
    #[cfg(any(
        feature = "modbus",
        feature = "iec104",
        feature = "opcua",
        feature = "ble",
        feature = "zigbee"
    ))]
    fn transform(&self) -> TransformConfig {
        TransformConfig {
            scale: self.scale,
            offset: self.offset,
            ..Default::default()
        }
    }
}

#[cfg(any(
    feature = "modbus",
    feature = "iec104",
    feature = "opcua",
    feature = "ble",
    feature = "zigbee",
    all(feature = "can", target_os = "linux"),
    all(feature = "j1939", target_os = "linux")
))]
pub(super) fn mapped_protocol_json<'a>(
    base: &'a Point,
    _protocol: &str,
) -> GatewayResult<Option<&'a str>> {
    let Some(mapping) = base.protocol_mappings.as_deref() else {
        return Ok(None);
    };
    if mapping
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .eq(b"null".iter().copied())
        || mapping
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .eq(b"{}".iter().copied())
    {
        return Ok(None);
    }
    Ok(Some(mapping))
}

#[cfg(any(
    feature = "modbus",
    feature = "iec104",
    feature = "opcua",
    feature = "ble",
    feature = "zigbee"
))]
fn convert_mapped_points<P: PointConvertible, T>(
    points: &[P],
    protocol: &str,
    build: &impl Fn(u32, PointType, TransformConfig, &str) -> GatewayResult<T>,
) -> GatewayResult<Vec<T>> {
    points
        .iter()
        .filter_map(|point| {
            let base = point.base();
            match mapped_protocol_json(base, protocol) {
                Ok(Some(mapping)) => Some(
                    build(base.point_id, P::point_type(), point.transform(), mapping).map_err(
                        |error| {
                            GatewayError::Config(format!(
                                "invalid {protocol} mapping for point {}: {error}",
                                base.point_id
                            ))
                        },
                    ),
                ),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

#[cfg(feature = "ble")]
fn convert_mapped_control_points<T>(
    points: &[ControlPoint],
    protocol: &str,
    build: &impl Fn(u32, TransformConfig, u16, u16, &str) -> GatewayResult<T>,
) -> GatewayResult<Vec<T>> {
    points
        .iter()
        .filter_map(|point| {
            let base = &point.base;
            match mapped_protocol_json(base, protocol) {
                Ok(Some(mapping)) => Some(
                    build(
                        base.point_id,
                        point.transform(),
                        point.on_value,
                        point.off_value,
                        mapping,
                    )
                    .map_err(|error| {
                        GatewayError::Config(format!(
                            "invalid {protocol} mapping for point {}: {error}",
                            base.point_id
                        ))
                    }),
                ),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

#[cfg(any(
    feature = "modbus",
    feature = "iec104",
    feature = "opcua",
    feature = "ble",
    feature = "zigbee"
))]
fn convert_all_mapped_points<T>(
    runtime_config: &RuntimeChannelConfig,
    protocol: &str,
    build: &impl Fn(u32, PointType, TransformConfig, &str) -> GatewayResult<T>,
) -> GatewayResult<Vec<T>> {
    let capacity = runtime_config.telemetry_points.len()
        + runtime_config.signal_points.len()
        + runtime_config.control_points.len()
        + runtime_config.adjustment_points.len();
    let mut configs = Vec::with_capacity(capacity);
    configs.extend(convert_mapped_points(
        &runtime_config.telemetry_points,
        protocol,
        build,
    )?);
    configs.extend(convert_mapped_points(
        &runtime_config.signal_points,
        protocol,
        build,
    )?);
    configs.extend(convert_mapped_points(
        &runtime_config.control_points,
        protocol,
        build,
    )?);
    configs.extend(convert_mapped_points(
        &runtime_config.adjustment_points,
        protocol,
        build,
    )?);
    Ok(configs)
}

#[cfg(feature = "iec104")]
pub fn convert_to_iec104_point_configs(
    runtime_config: &RuntimeChannelConfig,
) -> GatewayResult<Vec<PointConfig<Iec104Address>>> {
    convert_all_mapped_points(
        runtime_config,
        "IEC 104",
        &|id, point_type, transform, mapping| {
            crate::protocols::adapters::iec104::parse_point_mapping(mapping)
                .map(|address| PointConfig::new(id, point_type, address).with_transform(transform))
        },
    )
}

#[cfg(feature = "opcua")]
pub fn convert_to_opcua_point_configs(
    runtime_config: &RuntimeChannelConfig,
) -> GatewayResult<Vec<PointConfig<OpcUaAddress>>> {
    convert_all_mapped_points(
        runtime_config,
        "OPC UA",
        &|id, point_type, transform, mapping| {
            crate::protocols::adapters::opcua::parse_point_mapping(mapping)
                .map(|address| PointConfig::new(id, point_type, address).with_transform(transform))
        },
    )
}

#[cfg(feature = "ble")]
pub(crate) fn convert_to_ble_point_configs(
    runtime_config: &RuntimeChannelConfig,
) -> GatewayResult<Vec<crate::protocols::adapters::ble::BlePointConfig>> {
    use crate::protocols::adapters::ble::BlePointConfig;

    let mut configs = convert_mapped_points(
        &runtime_config.telemetry_points,
        "BLE",
        &BlePointConfig::from_mapping,
    )?;
    configs.extend(convert_mapped_points(
        &runtime_config.signal_points,
        "BLE",
        &BlePointConfig::from_mapping,
    )?);
    configs.extend(convert_mapped_control_points(
        &runtime_config.control_points,
        "BLE",
        &BlePointConfig::from_control_mapping,
    )?);
    configs.extend(convert_mapped_points(
        &runtime_config.adjustment_points,
        "BLE",
        &BlePointConfig::from_mapping,
    )?);
    Ok(configs)
}

#[cfg(feature = "zigbee")]
pub(crate) fn convert_to_zigbee_point_configs(
    runtime_config: &RuntimeChannelConfig,
) -> GatewayResult<Vec<crate::protocols::adapters::zigbee::ZigbeePointConfig>> {
    use crate::protocols::adapters::zigbee::ZigbeePointConfig;

    convert_all_mapped_points(runtime_config, "Zigbee", &ZigbeePointConfig::from_mapping)
}

// ============================================================================
// Modbus Point Conversion
// ============================================================================

/// Convert RuntimeChannelConfig to PointConfig list for Modbus.
///
/// Extracts Modbus mapping information from each point's embedded protocol_mappings JSON field.
/// This replaces the old approach of using separate modbus_mappings collection.
///
/// Each PointConfig carries an explicit `point_type` field for routing.
#[cfg(feature = "modbus")]
pub fn convert_to_modbus_point_configs(
    runtime_config: &RuntimeChannelConfig,
) -> GatewayResult<Vec<PointConfig<ModbusAddress>>> {
    convert_all_mapped_points(
        runtime_config,
        "Modbus",
        &|id, point_type, transform, mapping| {
            parse_modbus_mapping(id, point_type, mapping)
                .map(|address| PointConfig::new(id, point_type, address).with_transform(transform))
        },
    )
}

#[cfg(feature = "modbus")]
fn parse_modbus_mapping(
    point_id: u32,
    point_type: PointType,
    mapping: &str,
) -> GatewayResult<ModbusAddress> {
    let value: serde_json::Value = serde_json::from_str(mapping).map_err(|error| {
        GatewayError::Config(format!(
            "invalid Modbus mapping JSON for point {point_id}: {error}"
        ))
    })?;
    crate::protocols::adapters::modbus::parse_point_mapping(point_type, point_id, &value)
}

// ============================================================================
// CAN Point Conversion
// ============================================================================

/// Collect CanPoints from a slice of typed points.
#[cfg(all(feature = "can", target_os = "linux"))]
fn collect_can_points<P: PointConvertible>(points: &[P]) -> GatewayResult<Vec<CanPoint>> {
    points
        .iter()
        .filter_map(|pt| {
            let base = pt.base();
            match mapped_protocol_json(base, "CAN") {
                Ok(Some(mapping)) => {
                    Some(parse_can_mapping(base.point_id, P::point_type(), mapping))
                },
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

#[cfg(all(feature = "can", target_os = "linux"))]
fn parse_can_mapping(point_id: u32, point_type: PointType, raw: &str) -> GatewayResult<CanPoint> {
    let mapping = serde_json::from_str(raw).map_err(|error| {
        GatewayError::Config(format!("invalid CAN mapping for point {point_id}: {error}"))
    })?;
    crate::protocols::adapters::can::config::parse_point_mapping(&mapping, point_type, point_id)
}

/// Convert RuntimeChannelConfig to CanPoint list for CAN protocol.
///
/// Parses CAN configuration from each point's protocol_mappings JSON field.
/// Scale and offset are applied during decoding in the protocol layer.
#[cfg(all(feature = "can", target_os = "linux"))]
pub fn convert_to_can_point_configs(
    runtime_config: &RuntimeChannelConfig,
) -> GatewayResult<Vec<CanPoint>> {
    let mut configs = collect_can_points(&runtime_config.telemetry_points)?;
    configs.extend(collect_can_points(&runtime_config.signal_points)?);
    configs.extend(collect_can_points(&runtime_config.control_points)?);
    configs.extend(collect_can_points(&runtime_config.adjustment_points)?);
    Ok(configs)
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    #[cfg(any(
        feature = "modbus",
        feature = "ble",
        feature = "zigbee",
        all(feature = "can", target_os = "linux")
    ))]
    use super::*;
    #[cfg(feature = "ble")]
    use crate::core::config::ControlPoint;
    #[cfg(any(
        feature = "modbus",
        feature = "ble",
        feature = "zigbee",
        all(feature = "can", target_os = "linux")
    ))]
    use crate::core::config::{ChannelConfig, ChannelCore, Point, SignalPoint, TelemetryPoint};
    #[cfg(feature = "modbus")]
    use crate::protocols::core::point::{ByteOrder, DataFormat};
    #[cfg(any(
        feature = "modbus",
        feature = "ble",
        feature = "zigbee",
        all(feature = "can", target_os = "linux")
    ))]
    use std::collections::HashMap;

    #[test]
    #[cfg(feature = "modbus")]
    fn test_convert_to_modbus_point_configs() {
        // Create a runtime config with embedded protocol_mappings
        let base_config = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "test_modbus".to_string(),
                description: None,
                protocol: "modbus_tcp".to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: Default::default(),
        };
        let mut runtime_config = RuntimeChannelConfig::from_base(base_config);

        // Add telemetry point with embedded Modbus mapping
        runtime_config.telemetry_points.push(TelemetryPoint {
            base: Point {
                point_id: 100,
                signal_name: "voltage".to_string(),
                description: None,
                unit: Some("V".to_string()),
                protocol_mappings: Some(r#"{"slave_id":"1","function_code":"3","register_address":"0","data_type":"F32","byte_order":"big_endian"}"#.to_string()),
            },
            scale: 1.0,
            offset: 0.0,
            data_type: "float32".to_string(),
            reverse: false,
        });

        // Add signal point with embedded Modbus mapping (with bit_position)
        runtime_config.signal_points.push(SignalPoint {
            base: Point {
                point_id: 101,
                signal_name: "status".to_string(),
                description: None,
                unit: None,
                protocol_mappings: Some(r#"{"slave_id":1,"function_code":1,"register_address":10,"data_type":"bool","byte_order":"ABCD","bit_position":5}"#.to_string()),
            },
            reverse: false,
        });

        use aether_core::PointType;

        let configs = convert_to_modbus_point_configs(&runtime_config).unwrap();

        assert_eq!(configs.len(), 2);

        // Check first point (telemetry, float32) - uses original point_id and explicit point_type
        let pt1 = configs
            .iter()
            .find(|c| c.id == 100 && c.point_type == PointType::Telemetry)
            .unwrap();
        let addr = &pt1.address;
        assert_eq!(addr.slave_id, 1);
        assert_eq!(addr.function_code, 3);
        assert_eq!(addr.register, 0);
        assert_eq!(addr.format, DataFormat::Float32);
        assert_eq!(addr.byte_order, ByteOrder::Abcd);

        // Check second point (signal, bool with bit_position) - uses original point_id and explicit point_type
        let pt2 = configs
            .iter()
            .find(|c| c.id == 101 && c.point_type == PointType::Signal)
            .unwrap();
        let addr = &pt2.address;
        assert_eq!(addr.slave_id, 1);
        assert_eq!(addr.function_code, 1);
        assert_eq!(addr.register, 10);
        assert_eq!(addr.format, DataFormat::Bool);
        assert_eq!(addr.bit_position, Some(5));
    }

    #[test]
    #[cfg(feature = "modbus")]
    fn modbus_conversion_skips_only_canonical_unmapped_rows_and_fails_closed() {
        let base_config = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "strict_modbus".to_string(),
                description: None,
                protocol: "modbus_tcp".to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: Default::default(),
        };
        let mut runtime_config = RuntimeChannelConfig::from_base(base_config);
        for (point_id, mapping) in [
            (1, None),
            (2, Some("null")),
            (3, Some("{}")),
            (4, Some(r#"{"slave_id":1"#)),
        ] {
            runtime_config.telemetry_points.push(TelemetryPoint {
                base: Point {
                    point_id,
                    signal_name: format!("point-{point_id}"),
                    description: None,
                    unit: None,
                    protocol_mappings: mapping.map(str::to_string),
                },
                scale: 1.0,
                offset: 0.0,
                data_type: "float64".to_string(),
                reverse: false,
            });
        }

        assert!(convert_to_modbus_point_configs(&runtime_config).is_err());
        runtime_config.telemetry_points.pop();
        assert!(
            convert_to_modbus_point_configs(&runtime_config)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(all(feature = "can", target_os = "linux"))]
    #[test]
    fn can_conversion_uses_the_adapter_owned_strict_codec() {
        let base_config = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "strict_can".to_string(),
                description: None,
                protocol: "can".to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: Default::default(),
        };
        let mut runtime_config = RuntimeChannelConfig::from_base(base_config);
        runtime_config.telemetry_points.push(TelemetryPoint {
            base: Point {
                point_id: 20,
                signal_name: "voltage".to_string(),
                description: None,
                unit: Some("V".to_string()),
                protocol_mappings: Some(
                    r#"{"can_id":"0x351","start_bit":0,"bit_length":16}"#.to_string(),
                ),
            },
            scale: 1.0,
            offset: 0.0,
            data_type: "uint16".to_string(),
            reverse: false,
        });

        let points = convert_to_can_point_configs(&runtime_config).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].point_id, 20);
        assert_eq!(points[0].can_id, 0x351);

        runtime_config.telemetry_points[0].base.protocol_mappings =
            Some(r#"{"can_id":"0x351","start_bit":0,"bit_length":16,"unknown":true}"#.to_string());
        assert!(convert_to_can_point_configs(&runtime_config).is_err());
    }

    #[cfg(feature = "ble")]
    fn control_runtime(protocol: &str, mapping: &str) -> RuntimeChannelConfig {
        let base_config = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: format!("test_{protocol}"),
                description: None,
                protocol: protocol.to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: Default::default(),
        };
        let mut runtime_config = RuntimeChannelConfig::from_base(base_config);
        runtime_config.control_points.push(ControlPoint {
            base: Point {
                point_id: 7,
                signal_name: "switch".to_string(),
                description: None,
                unit: None,
                protocol_mappings: Some(mapping.to_string()),
            },
            reverse: true,
            control_type: "latching".to_string(),
            on_value: 17,
            off_value: 4,
            pulse_duration_ms: None,
        });
        runtime_config
    }

    #[cfg(feature = "ble")]
    #[test]
    fn ble_conversion_carries_control_values() {
        let mut runtime_config = control_runtime(
            "ble",
            r#"{"service_uuid":"180f","characteristic_uuid":"2a19"}"#,
        );
        assert_eq!(
            convert_to_ble_point_configs(&runtime_config).unwrap().len(),
            1
        );
        runtime_config.control_points[0].off_value = 17;
        assert!(convert_to_ble_point_configs(&runtime_config).is_err());
    }

    #[cfg(feature = "zigbee")]
    #[test]
    fn zigbee_conversion_skips_unmapped_points_and_rejects_commands() {
        let base_config = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "test_zigbee".to_string(),
                description: None,
                protocol: "zigbee".to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: Default::default(),
        };
        let mut runtime_config = RuntimeChannelConfig::from_base(base_config);
        runtime_config.telemetry_points.push(TelemetryPoint {
            base: Point {
                point_id: 1,
                signal_name: "unmapped".to_string(),
                description: None,
                unit: None,
                protocol_mappings: None,
            },
            scale: 1.0,
            offset: 0.0,
            data_type: "float64".to_string(),
            reverse: false,
        });
        runtime_config.signal_points.push(SignalPoint {
            base: Point {
                point_id: 2,
                signal_name: "empty".to_string(),
                description: None,
                unit: None,
                protocol_mappings: Some("{}".to_string()),
            },
            reverse: false,
        });
        assert!(
            convert_to_zigbee_point_configs(&runtime_config)
                .unwrap()
                .is_empty()
        );

        runtime_config
            .control_points
            .push(crate::core::config::ControlPoint {
                base: Point {
                    point_id: 3,
                    signal_name: "command".to_string(),
                    description: None,
                    unit: None,
                    protocol_mappings: Some(
                        r#"{"ieee_address":1,"endpoint":1,"cluster_id":6,"attribute_id":0}"#
                            .to_string(),
                    ),
                },
                reverse: false,
                control_type: "latching".to_string(),
                on_value: 1,
                off_value: 0,
                pulse_duration_ms: None,
            });
        assert!(convert_to_zigbee_point_configs(&runtime_config).is_err());
    }

    /// Test the specific internal_id encoding for all four point types.
    #[test]
    fn test_internal_id_encoding_for_all_point_types() {
        use aether_core::PointType;

        let point_id = 1u32;

        // Telemetry: offset = 0
        let telemetry_internal = PointType::Telemetry.to_internal_id(point_id);
        assert_eq!(telemetry_internal, point_id); // No offset

        // Signal: offset = OFFSET (0x40000000)
        let signal_internal = PointType::Signal.to_internal_id(point_id);
        assert_eq!(signal_internal, PointType::OFFSET + point_id);

        // Control: offset = OFFSET * 2 (0x80000000)
        let control_internal = PointType::Control.to_internal_id(point_id);
        assert_eq!(control_internal, PointType::OFFSET * 2 + point_id);

        // Adjustment: offset = OFFSET * 3 (0xC0000000)
        let adjustment_internal = PointType::Adjustment.to_internal_id(point_id);
        assert_eq!(adjustment_internal, PointType::OFFSET * 3 + point_id);

        // Verify round-trip
        let (pt, id) = PointType::from_internal_id(control_internal);
        assert_eq!(pt, PointType::Control);
        assert_eq!(id, point_id);
    }
}
