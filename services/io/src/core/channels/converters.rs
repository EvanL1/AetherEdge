//! Point configuration converters
//!
//! Convert io RuntimeChannelConfig to PointConfig/CanPoint.
//!
//! This module handles the "translation" between io's configuration format
//! and the protocol layer's point configuration format.

#[cfg(any(feature = "modbus", all(feature = "can", target_os = "linux")))]
use tracing::warn;

use crate::core::config::{
    AdjustmentPoint, ControlPoint, Point, RuntimeChannelConfig, SignalPoint, TelemetryPoint,
};
use crate::protocols::core::point::{
    PointConfig, ProtocolAddress, TransformConfig, VirtualAddress,
};
use aether_model::PointType;

#[cfg(feature = "modbus")]
use crate::protocols::core::point::{ByteOrder, DataFormat, ModbusAddress};

#[cfg(all(feature = "can", target_os = "linux"))]
use crate::protocols::adapters::can::{CanDataType, CanPoint};

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
    fn transform(&self) -> TransformConfig;
}

impl PointConvertible for TelemetryPoint {
    fn base(&self) -> &Point {
        &self.base
    }
    fn point_type() -> PointType {
        PointType::Telemetry
    }
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
    fn transform(&self) -> TransformConfig {
        TransformConfig {
            scale: self.scale,
            offset: self.offset,
            ..Default::default()
        }
    }
}

/// Convert a slice of typed points to PointConfig using the given address builder.
fn convert_points<P: PointConvertible>(
    points: &[P],
    addr_fn: &impl Fn(&Point) -> Option<ProtocolAddress>,
) -> Vec<PointConfig> {
    points
        .iter()
        .filter_map(|pt| {
            let addr = addr_fn(pt.base())?;
            Some(
                PointConfig::new(pt.base().point_id, P::point_type(), addr)
                    .with_name(&pt.base().signal_name)
                    .with_transform(pt.transform()),
            )
        })
        .collect()
}

/// Collect PointConfigs from all four point types on a RuntimeChannelConfig.
fn convert_all_points(
    rc: &RuntimeChannelConfig,
    addr_fn: &impl Fn(&Point) -> Option<ProtocolAddress>,
) -> Vec<PointConfig> {
    let mut configs = convert_points(&rc.telemetry_points, addr_fn);
    configs.extend(convert_points(&rc.signal_points, addr_fn));
    configs.extend(convert_points(&rc.control_points, addr_fn));
    configs.extend(convert_points(&rc.adjustment_points, addr_fn));
    configs
}

// ============================================================================
// Virtual Channel Point Conversion
// ============================================================================

/// Convert RuntimeChannelConfig to PointConfig list.
///
/// This function sets up TransformConfig for each point type:
/// - Telemetry: scale/offset transformation
/// - Signal: reverse boolean transformation
/// - Control: reverse boolean transformation
/// - Adjustment: scale/offset transformation
///
/// Each PointConfig carries an explicit `point_type` field that routes
/// the data to the correct typed SHM slot.
pub fn convert_to_point_configs(runtime_config: &RuntimeChannelConfig) -> Vec<PointConfig> {
    convert_all_points(runtime_config, &|base: &Point| {
        Some(ProtocolAddress::Virtual(VirtualAddress::new(
            base.point_id.to_string(),
        )))
    })
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
pub fn convert_to_modbus_point_configs(runtime_config: &RuntimeChannelConfig) -> Vec<PointConfig> {
    // Helper to parse modbus config from protocol_mappings JSON
    // Returns: (slave_id, function_code, register, data_type, byte_order, bit_position)
    fn parse_modbus_mapping(
        json_str: &str,
        point_id: u32,
    ) -> Option<(u8, u8, u16, String, String, Option<u8>)> {
        let v: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "Point {} has invalid protocol_mappings JSON: {}",
                    point_id, e
                );
                return None;
            },
        };

        if !v.is_object() {
            warn!(
                "Point {} has invalid protocol_mappings (expected JSON object): {}",
                point_id, v
            );
            return None;
        }

        fn parse_u64_field(v: &serde_json::Value, key: &str) -> Option<u64> {
            let raw = v.get(key)?;
            raw.as_u64()
                .or_else(|| raw.as_i64().and_then(|n| u64::try_from(n).ok()))
                .or_else(|| raw.as_str().and_then(|s| s.parse::<u64>().ok()))
        }

        let slave_id: u8 = match parse_u64_field(&v, "slave_id").and_then(|n| u8::try_from(n).ok())
        {
            Some(n) => n,
            None => {
                warn!(
                    "Point {} protocol_mappings missing/invalid 'slave_id': {}",
                    point_id, v
                );
                return None;
            },
        };

        let function_code: u8 =
            match parse_u64_field(&v, "function_code").and_then(|n| u8::try_from(n).ok()) {
                Some(n) => n,
                None => {
                    warn!(
                        "Point {} protocol_mappings missing/invalid 'function_code': {}",
                        point_id, v
                    );
                    return None;
                },
            };

        let register: u16 =
            match parse_u64_field(&v, "register_address").and_then(|n| u16::try_from(n).ok()) {
                Some(n) => n,
                None => {
                    warn!(
                        "Point {} protocol_mappings missing/invalid 'register_address': {}",
                        point_id, v
                    );
                    return None;
                },
            };

        Some((
            slave_id,
            function_code,
            register,
            v.get("data_type")
                .and_then(|x| x.as_str())
                .unwrap_or("uint16")
                .to_string(),
            v.get("byte_order")
                .and_then(|x| x.as_str())
                .unwrap_or("ABCD")
                .to_string(),
            // bit_position: None means not set, Some(0) means bit 0
            parse_u64_field(&v, "bit_position").and_then(|n| u8::try_from(n).ok()),
        ))
    }

    convert_all_points(runtime_config, &|base: &Point| {
        let json = base.protocol_mappings.as_ref()?;
        let (slave_id, fc, reg, dt, bo, bp) = parse_modbus_mapping(json, base.point_id)?;
        Some(ProtocolAddress::Modbus(ModbusAddress {
            slave_id,
            function_code: fc,
            register: reg,
            format: parse_data_format(&dt),
            byte_order: parse_byte_order(&bo),
            bit_position: bp,
        }))
    })
}

/// Parse data format string to DataFormat enum.
#[cfg(feature = "modbus")]
pub fn parse_data_format(s: &str) -> DataFormat {
    match s.to_lowercase().as_str() {
        "bool" | "boolean" => DataFormat::Bool,
        "uint16" | "u16" => DataFormat::UInt16,
        "int16" | "i16" => DataFormat::Int16,
        "uint32" | "u32" => DataFormat::UInt32,
        "int32" | "i32" => DataFormat::Int32,
        "float32" | "f32" | "float" => DataFormat::Float32,
        "float64" | "f64" | "double" => DataFormat::Float64,
        "uint64" | "u64" => DataFormat::UInt64,
        "int64" | "i64" => DataFormat::Int64,
        _ => DataFormat::UInt16, // Default
    }
}

/// Parse byte order string to ByteOrder enum.
#[cfg(feature = "modbus")]
pub fn parse_byte_order(s: &str) -> ByteOrder {
    match s.to_uppercase().as_str() {
        "ABCD" | "BIG_ENDIAN" | "BE" => ByteOrder::Abcd,
        "DCBA" | "LITTLE_ENDIAN" | "LE" => ByteOrder::Dcba,
        "BADC" | "WORD_SWAP" => ByteOrder::Badc,
        "CDAB" | "BYTE_SWAP" => ByteOrder::Cdab,
        _ => ByteOrder::Abcd, // Default to big-endian
    }
}

// ============================================================================
// CAN Point Conversion
// ============================================================================

/// CAN protocol mapping from protocol_mappings JSON field
#[cfg(all(feature = "can", target_os = "linux"))]
#[derive(Debug, Clone, serde::Deserialize)]
struct CanProtocolMapping {
    can_id: u32,
    start_bit: u32,
    bit_length: u32,
    #[serde(default)]
    data_type: CanDataType,
    #[serde(default = "default_scale")]
    scale: f64,
    #[serde(default)]
    offset: f64,
}

#[cfg(all(feature = "can", target_os = "linux"))]
fn default_scale() -> f64 {
    1.0
}

/// Collect CanPoints from a slice of typed points.
#[cfg(all(feature = "can", target_os = "linux"))]
fn collect_can_points<P: PointConvertible>(points: &[P]) -> Vec<CanPoint> {
    points
        .iter()
        .filter_map(|pt| {
            let json_str = pt.base().protocol_mappings.as_ref()?;
            let mapping: CanProtocolMapping = serde_json::from_str(json_str)
                .map_err(|e| {
                    tracing::warn!(
                        point_id = pt.base().point_id,
                        point_type = ?P::point_type(),
                        error = %e,
                        "Failed to parse CAN protocol_mappings JSON"
                    );
                    e
                })
                .ok()?;
            Some(CanPoint {
                point_id: pt.base().point_id,
                point_type: P::point_type(),
                can_id: mapping.can_id,
                byte_offset: (mapping.start_bit / 8) as u8,
                bit_position: (mapping.start_bit % 8) as u8,
                bit_length: mapping.bit_length as u8,
                data_type: mapping.data_type,
                scale: mapping.scale,
                offset: mapping.offset,
            })
        })
        .collect()
}

/// Convert RuntimeChannelConfig to CanPoint list for CAN protocol.
///
/// Parses CAN configuration from each point's protocol_mappings JSON field.
/// Scale and offset are applied during decoding in the protocol layer.
#[cfg(all(feature = "can", target_os = "linux"))]
pub fn convert_to_can_point_configs(runtime_config: &RuntimeChannelConfig) -> Vec<CanPoint> {
    let mut configs = collect_can_points(&runtime_config.telemetry_points);
    configs.extend(collect_can_points(&runtime_config.signal_points));
    configs.extend(collect_can_points(&runtime_config.control_points));
    configs.extend(collect_can_points(&runtime_config.adjustment_points));
    configs
}

/// Convert runtime CAN mappings to point configuration metadata.
///
/// This conversion is used to register points with the data store for proper
/// data transformation and storage.
/// Parses CAN configuration from each point's protocol_mappings JSON field.
#[cfg(all(feature = "can", target_os = "linux"))]
pub fn convert_can_to_point_configs(runtime_config: &RuntimeChannelConfig) -> Vec<PointConfig> {
    convert_all_points(runtime_config, &|base: &Point| {
        let json_str = base.protocol_mappings.as_ref()?;
        let mapping: CanProtocolMapping = serde_json::from_str(json_str).ok()?;
        Some(ProtocolAddress::Generic(format!(
            "can_id:0x{:X},start_bit:{},len:{}",
            mapping.can_id, mapping.start_bit, mapping.bit_length
        )))
    })
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use crate::core::config::{
        AdjustmentPoint, ChannelConfig, ChannelCore, ControlPoint, Point, SignalPoint,
        TelemetryPoint,
    };
    use std::collections::HashMap;

    fn create_test_runtime_config() -> RuntimeChannelConfig {
        let base_config = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "test_channel".to_string(),
                description: None,
                protocol: "virtual".to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: Default::default(),
        };
        let mut config = RuntimeChannelConfig::from_base(base_config);

        config.telemetry_points.push(TelemetryPoint {
            base: Point {
                point_id: 10,
                signal_name: "temperature".to_string(),
                description: None,
                unit: Some("C".to_string()),
                protocol_mappings: None,
            },
            scale: 1.0,
            offset: 0.0,
            data_type: "float32".to_string(),
            reverse: false,
        });

        config.signal_points.push(SignalPoint {
            base: Point {
                point_id: 20,
                signal_name: "status".to_string(),
                description: None,
                unit: None,
                protocol_mappings: None,
            },
            reverse: false,
        });

        config.control_points.push(ControlPoint {
            base: Point {
                point_id: 30,
                signal_name: "switch".to_string(),
                description: None,
                unit: None,
                protocol_mappings: None,
            },
            reverse: false,
            control_type: "latching".to_string(),
            on_value: 1,
            off_value: 0,
            pulse_duration_ms: None,
        });

        config.adjustment_points.push(AdjustmentPoint {
            base: Point {
                point_id: 40,
                signal_name: "setpoint".to_string(),
                description: None,
                unit: Some("C".to_string()),
                protocol_mappings: None,
            },
            min_value: None,
            max_value: None,
            step: 1.0,
            data_type: "float32".to_string(),
            scale: 1.0,
            offset: 0.0,
        });

        config
    }

    #[test]
    fn test_convert_to_point_configs() {
        use aether_model::PointType;

        let runtime_config = create_test_runtime_config();
        let configs = convert_to_point_configs(&runtime_config);

        assert_eq!(configs.len(), 4);

        // Check telemetry point - uses original point_id and explicit point_type
        let telemetry = configs
            .iter()
            .find(|c| c.id == 10 && c.point_type == PointType::Telemetry)
            .unwrap();
        assert_eq!(telemetry.name, Some("temperature".to_string()));

        // Check signal point exists with original point_id and point_type
        assert!(
            configs
                .iter()
                .any(|c| c.id == 20 && c.point_type == PointType::Signal)
        );

        // Check control point exists with original point_id and point_type
        assert!(
            configs
                .iter()
                .any(|c| c.id == 30 && c.point_type == PointType::Control)
        );

        // Check adjustment point exists with original point_id and point_type
        assert!(
            configs
                .iter()
                .any(|c| c.id == 40 && c.point_type == PointType::Adjustment)
        );
    }

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
                protocol_mappings: Some(r#"{"slave_id":1,"function_code":3,"register_address":0,"data_type":"float32","byte_order":"ABCD"}"#.to_string()),
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

        use aether_model::PointType;

        let configs = convert_to_modbus_point_configs(&runtime_config);

        assert_eq!(configs.len(), 2);

        // Check first point (telemetry, float32) - uses original point_id and explicit point_type
        let pt1 = configs
            .iter()
            .find(|c| c.id == 100 && c.point_type == PointType::Telemetry)
            .unwrap();
        if let ProtocolAddress::Modbus(addr) = &pt1.address {
            assert_eq!(addr.slave_id, 1);
            assert_eq!(addr.function_code, 3);
            assert_eq!(addr.register, 0);
            assert_eq!(addr.format, DataFormat::Float32);
            assert_eq!(addr.byte_order, ByteOrder::Abcd);
        } else {
            panic!("Expected ModbusAddress");
        }

        // Check second point (signal, bool with bit_position) - uses original point_id and explicit point_type
        let pt2 = configs
            .iter()
            .find(|c| c.id == 101 && c.point_type == PointType::Signal)
            .unwrap();
        if let ProtocolAddress::Modbus(addr) = &pt2.address {
            assert_eq!(addr.slave_id, 1);
            assert_eq!(addr.function_code, 1);
            assert_eq!(addr.register, 10);
            assert_eq!(addr.format, DataFormat::Bool);
            assert_eq!(addr.bit_position, Some(5));
        } else {
            panic!("Expected ModbusAddress");
        }
    }

    #[test]
    #[cfg(feature = "modbus")]
    fn test_parse_data_format() {
        assert_eq!(parse_data_format("bool"), DataFormat::Bool);
        assert_eq!(parse_data_format("FLOAT32"), DataFormat::Float32);
        assert_eq!(parse_data_format("uint16"), DataFormat::UInt16);
        assert_eq!(parse_data_format("Int32"), DataFormat::Int32);
    }

    #[test]
    #[cfg(feature = "modbus")]
    fn test_parse_byte_order() {
        assert_eq!(parse_byte_order("ABCD"), ByteOrder::Abcd);
        assert_eq!(parse_byte_order("big_endian"), ByteOrder::Abcd);
        assert_eq!(parse_byte_order("CDAB"), ByteOrder::Cdab);
        assert_eq!(parse_byte_order("DCBA"), ByteOrder::Dcba);
    }

    /// Test the specific internal_id encoding for all four point types.
    #[test]
    fn test_internal_id_encoding_for_all_point_types() {
        use aether_model::PointType;

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
