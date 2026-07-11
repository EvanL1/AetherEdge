// Test code uses unwrap for simplicity
#![allow(clippy::disallowed_methods)]
#![allow(clippy::unnecessary_get_then_check)]

//! CAN frame data decoder (cross-platform)
//!
//! Provides functions to extract and decode fields from CAN frame data,
//! supporting various data types with Little-Endian byte ordering.
//!
//! This module is platform-independent and can be tested on any OS.

use super::can_types::{CanDataType, CanFrameCache, CanPoint};
use crate::protocols::core::data::Value;
use crate::protocols::core::error::{GatewayError, Result};

use std::collections::HashMap;

/// Point mapping manager
pub struct PointManager {
    /// All points indexed by point_id
    points: HashMap<u32, CanPoint>,
}

impl Default for PointManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PointManager {
    /// Create a new empty point manager
    pub fn new() -> Self {
        Self {
            points: HashMap::new(),
        }
    }

    /// Add a point to the manager
    pub fn add_point(&mut self, point: CanPoint) {
        self.points.insert(point.point_id, point);
    }

    /// Get all point IDs for SlotStore initialization.
    pub fn point_ids(&self) -> Vec<u32> {
        self.points.keys().copied().collect()
    }

    /// Apply mappings to decode CAN frames into data points
    ///
    /// Returns a HashMap of point_id -> Value for successfully decoded points.
    /// Quality is always Good for decoded values (no bad quality from CAN decode).
    pub fn apply_mappings(&self, frame_cache: &CanFrameCache) -> Result<HashMap<u32, Value>> {
        let mut result = HashMap::with_capacity(self.points.len());

        for (point_id, point) in &self.points {
            if let Some(frame_data) = frame_cache.get(point.can_id) {
                match decode_point(point, frame_data) {
                    Ok(value) => {
                        result.insert(*point_id, value);
                    },
                    Err(_e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!("Failed to decode point {}: {}", point_id, _e);
                    },
                }
            }
        }

        Ok(result)
    }
}

/// Extracted field data - stack-allocated buffer with length
/// CAN frames are max 8 bytes, so [u8; 8] is sufficient
struct ExtractedField {
    data: [u8; 8],
    len: u8,
}

impl ExtractedField {
    fn new(bytes: &[u8]) -> Self {
        let mut data = [0u8; 8];
        let len = bytes.len().min(8) as u8;
        data[..len as usize].copy_from_slice(&bytes[..len as usize]);
        Self { data, len }
    }

    fn single(byte: u8) -> Self {
        Self {
            data: [byte, 0, 0, 0, 0, 0, 0, 0],
            len: 1,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// Extract a multi-byte aligned field (16/32/64-bit).
///
/// Validates bit_position == 0 and sufficient data length, then returns the slice.
#[inline]
fn extract_multibyte(
    data: &[u8],
    byte_offset: usize,
    bit_position: u8,
    bit_length: u8,
) -> Result<ExtractedField> {
    if bit_position != 0 {
        return Err(GatewayError::Protocol(format!(
            "{bit_length}-bit field must start at bit position 0, got {bit_position}"
        )));
    }
    let byte_count = (bit_length / 8) as usize;
    if byte_offset + byte_count > data.len() {
        return Err(GatewayError::Protocol(format!(
            "Not enough bytes for {bit_length}-bit field at offset {byte_offset}"
        )));
    }
    Ok(ExtractedField::new(
        &data[byte_offset..byte_offset + byte_count],
    ))
}

/// Extract a field from CAN data (stack-allocated, no heap allocation)
fn extract_field(
    data: &[u8],
    byte_offset: u8,
    bit_position: u8,
    bit_length: u8,
) -> Result<ExtractedField> {
    let byte_offset = byte_offset as usize;

    if byte_offset >= data.len() {
        return Err(GatewayError::Protocol(format!(
            "Byte offset {byte_offset} out of range (data length: {})",
            data.len()
        )));
    }

    match bit_length {
        2 => {
            if bit_position > 6 {
                return Err(GatewayError::Protocol(format!(
                    "Invalid bit position {bit_position} for 2-bit field"
                )));
            }
            Ok(ExtractedField::single(
                (data[byte_offset] >> bit_position) & 0x03,
            ))
        },
        8 => {
            if bit_position != 0 {
                return Err(GatewayError::Protocol(format!(
                    "8-bit field must start at bit position 0, got {bit_position}"
                )));
            }
            Ok(ExtractedField::single(data[byte_offset]))
        },
        16 | 32 | 64 => extract_multibyte(data, byte_offset, bit_position, bit_length),
        _ => Err(GatewayError::Protocol(format!(
            "Unsupported bit length: {bit_length}"
        ))),
    }
}

/// Read N little-endian bytes from a slice, returning a fixed-size array.
#[inline]
fn read_le<const N: usize>(bytes: &[u8], type_name: &str) -> Result<[u8; N]> {
    bytes
        .get(..N)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| {
            GatewayError::Protocol(format!(
                "Not enough bytes for {type_name}, got {}",
                bytes.len()
            ))
        })
}

/// Decode a CAN point from frame data
pub fn decode_point(point: &CanPoint, frame_data: &[u8]) -> Result<Value> {
    let field = extract_field(
        frame_data,
        point.byte_offset,
        point.bit_position,
        point.bit_length,
    )?;
    let raw = field.as_slice();

    let raw_value = match point.data_type {
        CanDataType::UInt8 => {
            if raw.is_empty() {
                return Err(GatewayError::Protocol("Empty data for uint8".into()));
            }
            Value::Integer(raw[0] as i64)
        },
        CanDataType::UInt16 => Value::Integer(u16::from_le_bytes(read_le(raw, "uint16")?) as i64),
        CanDataType::Int16 => Value::Integer(i16::from_le_bytes(read_le(raw, "int16")?) as i64),
        CanDataType::UInt32 => Value::Integer(u32::from_le_bytes(read_le(raw, "uint32")?) as i64),
        CanDataType::Int32 => Value::Integer(i32::from_le_bytes(read_le(raw, "int32")?) as i64),
        CanDataType::Float32 => Value::Float(f32::from_le_bytes(read_le(raw, "float32")?) as f64),
        CanDataType::Ascii => {
            let mut s = String::from_utf8_lossy(raw).into_owned();
            while s.ends_with('\0') {
                s.pop();
            }
            Value::String(s)
        },
    };

    // Apply scale and offset for numeric values
    Ok(match raw_value {
        Value::Integer(i) if point.scale != 1.0 || point.offset != 0.0 => {
            Value::Float((i as f64) * point.scale + point.offset)
        },
        other => other,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aether_model::PointType;

    // =========================================================================
    // Helper functions
    // =========================================================================

    fn create_point(
        point_id: u32,
        can_id: u32,
        byte_offset: u8,
        bit_position: u8,
        bit_length: u8,
        data_type: CanDataType,
    ) -> CanPoint {
        CanPoint {
            point_id,
            point_type: PointType::Telemetry,
            can_id,
            byte_offset,
            bit_position,
            bit_length,
            data_type,
            scale: 1.0,
            offset: 0.0,
        }
    }

    fn create_point_with_transform(
        point_id: u32,
        can_id: u32,
        byte_offset: u8,
        bit_length: u8,
        data_type: CanDataType,
        scale: f64,
        offset: f64,
    ) -> CanPoint {
        CanPoint {
            point_id,
            point_type: PointType::Telemetry,
            can_id,
            byte_offset,
            bit_position: 0,
            bit_length,
            data_type,
            scale,
            offset,
        }
    }

    // =========================================================================
    // extract_field tests
    // =========================================================================

    #[test]
    fn test_extract_field_2bit_at_position_0() {
        // Data: 0b11_10_01_00 = 0xE4
        let data = [0xE4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        // Extract 2 bits at position 0 -> should get 0b00 = 0
        let field = extract_field(&data, 0, 0, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x00]);

        // Extract 2 bits at position 2 -> should get 0b01 = 1
        let field = extract_field(&data, 0, 2, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x01]);

        // Extract 2 bits at position 4 -> should get 0b10 = 2
        let field = extract_field(&data, 0, 4, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x02]);

        // Extract 2 bits at position 6 -> should get 0b11 = 3
        let field = extract_field(&data, 0, 6, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x03]);
    }

    #[test]
    fn test_extract_field_8bit() {
        let data = [0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x00, 0x00];

        let field = extract_field(&data, 0, 0, 8).unwrap();
        assert_eq!(field.as_slice(), &[0x12]);

        let field = extract_field(&data, 1, 0, 8).unwrap();
        assert_eq!(field.as_slice(), &[0x34]);

        let field = extract_field(&data, 3, 0, 8).unwrap();
        assert_eq!(field.as_slice(), &[0x78]);
    }

    #[test]
    fn test_extract_field_16bit_little_endian() {
        // Little-Endian: 0x3412
        let data = [0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x00, 0x00];

        let field = extract_field(&data, 0, 0, 16).unwrap();
        assert_eq!(field.as_slice(), &[0x12, 0x34]);

        // Verify it decodes as Little-Endian
        let value = u16::from_le_bytes([field.as_slice()[0], field.as_slice()[1]]);
        assert_eq!(value, 0x3412);
    }

    #[test]
    fn test_extract_field_32bit_little_endian() {
        // Little-Endian: 0x78563412
        let data = [0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x00, 0x00];

        let field = extract_field(&data, 0, 0, 32).unwrap();
        assert_eq!(field.as_slice(), &[0x12, 0x34, 0x56, 0x78]);

        let value = u32::from_le_bytes([
            field.as_slice()[0],
            field.as_slice()[1],
            field.as_slice()[2],
            field.as_slice()[3],
        ]);
        assert_eq!(value, 0x78563412);
    }

    #[test]
    fn test_extract_field_64bit_ascii() {
        let data = [b'H', b'E', b'L', b'L', b'O', 0x00, 0x00, 0x00];

        let field = extract_field(&data, 0, 0, 64).unwrap();
        assert_eq!(
            field.as_slice(),
            &[b'H', b'E', b'L', b'L', b'O', 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn test_extract_field_boundary_error() {
        let data = [0x12, 0x34]; // Only 2 bytes

        // 16-bit at offset 0 -> OK
        assert!(extract_field(&data, 0, 0, 16).is_ok());

        // 16-bit at offset 1 -> Error (needs 2 bytes, only 1 available)
        assert!(extract_field(&data, 1, 0, 16).is_err());

        // 32-bit at offset 0 -> Error (needs 4 bytes, only 2 available)
        assert!(extract_field(&data, 0, 0, 32).is_err());

        // Byte offset out of range
        assert!(extract_field(&data, 5, 0, 8).is_err());
    }

    #[test]
    fn test_extract_field_invalid_bit_position() {
        let data = [0xFF; 8];

        // 2-bit at position 7 is invalid (would overflow)
        assert!(extract_field(&data, 0, 7, 2).is_err());

        // 8-bit must start at position 0
        assert!(extract_field(&data, 0, 1, 8).is_err());

        // 16-bit must start at position 0
        assert!(extract_field(&data, 0, 1, 16).is_err());
    }

    #[test]
    fn test_extract_field_unsupported_bit_length() {
        let data = [0xFF; 8];

        // Unsupported bit lengths
        assert!(extract_field(&data, 0, 0, 4).is_err());
        assert!(extract_field(&data, 0, 0, 24).is_err());
        assert!(extract_field(&data, 0, 0, 128).is_err());
    }

    // =========================================================================
    // decode_point tests - Data Types
    // =========================================================================

    #[test]
    fn test_decode_uint8() {
        let data = [0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 8, CanDataType::UInt8);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(0x42));
    }

    #[test]
    fn test_decode_uint16_little_endian() {
        // Little-Endian: low byte first -> 0x3412
        let data = [0x12, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 16, CanDataType::UInt16);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(0x3412));
    }

    #[test]
    fn test_decode_int16_positive() {
        // +1000 in Little-Endian
        let data = [0xE8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 16, CanDataType::Int16);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(1000));
    }

    #[test]
    fn test_decode_int16_negative() {
        // -1000 in Little-Endian (0xFC18)
        let data = [0x18, 0xFC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 16, CanDataType::Int16);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(-1000));
    }

    #[test]
    fn test_decode_uint32() {
        // 0x12345678 in Little-Endian
        let data = [0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 32, CanDataType::UInt32);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(0x12345678));
    }

    #[test]
    fn test_decode_int32_negative() {
        // -12345 in Little-Endian (0xFFFFCFC7)
        let data = [0xC7, 0xCF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 32, CanDataType::Int32);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(-12345));
    }

    #[test]
    fn test_decode_float32() {
        // 42.5 in IEEE 754 Little-Endian = 0x422A0000 -> [0x00, 0x00, 0x2A, 0x42]
        let data = [0x00, 0x00, 0x2A, 0x42, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 32, CanDataType::Float32);

        let value = decode_point(&point, &data).unwrap();
        match value {
            Value::Float(f) => assert!((f - 42.5).abs() < 0.001),
            _ => panic!("Expected Float value"),
        }
    }

    #[test]
    fn test_decode_ascii() {
        let data = [b'H', b'E', b'L', b'L', b'O', 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 64, CanDataType::Ascii);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::String("HELLO".to_string()));
    }

    #[test]
    fn test_decode_ascii_with_trailing_nulls() {
        let data = [b'A', b'B', 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point(1, 0x351, 0, 0, 64, CanDataType::Ascii);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::String("AB".to_string()));
    }

    // =========================================================================
    // decode_point tests - Scale and Offset
    // =========================================================================

    #[test]
    fn test_decode_with_scale() {
        // Raw value: 100, Scale: 0.1 -> Result: 10.0
        let data = [0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point_with_transform(1, 0x351, 0, 16, CanDataType::UInt16, 0.1, 0.0);

        let value = decode_point(&point, &data).unwrap();
        match value {
            Value::Float(f) => assert!((f - 10.0).abs() < 0.001),
            _ => panic!("Expected Float value after scaling"),
        }
    }

    #[test]
    fn test_decode_with_offset() {
        // Raw value: 100, Offset: -50 -> Result: 50.0
        let data = [0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point_with_transform(1, 0x351, 0, 16, CanDataType::UInt16, 1.0, -50.0);

        let value = decode_point(&point, &data).unwrap();
        match value {
            Value::Float(f) => assert!((f - 50.0).abs() < 0.001),
            _ => panic!("Expected Float value after offset"),
        }
    }

    #[test]
    fn test_decode_with_scale_and_offset() {
        // Raw value: 1000, Scale: 0.1, Offset: -50 -> Result: 50.0
        let data = [0xE8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; // 1000 LE
        let point = create_point_with_transform(1, 0x351, 0, 16, CanDataType::UInt16, 0.1, -50.0);

        let value = decode_point(&point, &data).unwrap();
        match value {
            Value::Float(f) => assert!((f - 50.0).abs() < 0.001),
            _ => panic!("Expected Float value"),
        }
    }

    #[test]
    fn test_decode_no_transform_returns_integer() {
        // When scale=1.0 and offset=0.0, should return Integer (no conversion)
        let data = [0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point_with_transform(1, 0x351, 0, 16, CanDataType::UInt16, 1.0, 0.0);

        let value = decode_point(&point, &data).unwrap();
        assert_eq!(value, Value::Integer(100));
    }

    // =========================================================================
    // decode_point tests - Byte Offset
    // =========================================================================

    #[test]
    fn test_decode_at_different_offsets() {
        let data = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

        // Read uint16 at offset 0 -> 0x2211
        let point0 = create_point(1, 0x351, 0, 0, 16, CanDataType::UInt16);
        let value0 = decode_point(&point0, &data).unwrap();
        assert_eq!(value0, Value::Integer(0x2211));

        // Read uint16 at offset 2 -> 0x4433
        let point2 = create_point(2, 0x351, 2, 0, 16, CanDataType::UInt16);
        let value2 = decode_point(&point2, &data).unwrap();
        assert_eq!(value2, Value::Integer(0x4433));

        // Read uint16 at offset 4 -> 0x6655
        let point4 = create_point(3, 0x351, 4, 0, 16, CanDataType::UInt16);
        let value4 = decode_point(&point4, &data).unwrap();
        assert_eq!(value4, Value::Integer(0x6655));
    }

    // =========================================================================
    // PointManager tests
    // =========================================================================

    #[test]
    fn test_point_manager_apply_mappings() {
        let mut manager = PointManager::new();

        // Add two points from different CAN IDs
        manager.add_point(create_point(1, 0x351, 0, 0, 16, CanDataType::UInt16));
        manager.add_point(create_point(2, 0x356, 0, 0, 16, CanDataType::UInt16));

        // Create frame cache with test data
        let mut cache = CanFrameCache::new();
        cache.update(0x351, &[0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // 100
        cache.update(0x356, &[0xC8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // 200

        // Apply mappings
        let result = manager.apply_mappings(&cache).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&1), Some(&Value::Integer(100)));
        assert_eq!(result.get(&2), Some(&Value::Integer(200)));
    }

    #[test]
    fn test_point_manager_missing_frame() {
        let mut manager = PointManager::new();
        manager.add_point(create_point(1, 0x351, 0, 0, 16, CanDataType::UInt16));
        manager.add_point(create_point(2, 0x999, 0, 0, 16, CanDataType::UInt16)); // No data for this

        let mut cache = CanFrameCache::new();
        cache.update(0x351, &[0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // No data for 0x999

        let result = manager.apply_mappings(&cache).unwrap();

        // Only point 1 should be decoded
        assert_eq!(result.len(), 1);
        assert_eq!(result.get(&1), Some(&Value::Integer(100)));
        assert!(result.get(&2).is_none());
    }

    // =========================================================================
    // LYNK Protocol specific tests (real-world scenarios)
    // =========================================================================

    #[test]
    fn test_lynk_battery_voltage() {
        // LYNK 0x351: Battery voltage at offset 0, uint16, scale 0.01
        // Real value: 5120 (0x1400) * 0.01 = 51.20V
        let data = [0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point_with_transform(1, 0x351, 0, 16, CanDataType::UInt16, 0.01, 0.0);

        let value = decode_point(&point, &data).unwrap();
        match value {
            Value::Float(f) => assert!((f - 51.2).abs() < 0.01),
            _ => panic!("Expected Float value"),
        }
    }

    #[test]
    fn test_lynk_battery_current_signed() {
        // LYNK: Battery current (signed), can be negative during discharge
        // Value: -1000 (0xFC18 LE) * 0.1 = -100.0A
        let data = [0x18, 0xFC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let point = create_point_with_transform(1, 0x351, 0, 16, CanDataType::Int16, 0.1, 0.0);

        let value = decode_point(&point, &data).unwrap();
        match value {
            Value::Float(f) => assert!((f - (-100.0)).abs() < 0.01),
            _ => panic!("Expected Float value"),
        }
    }

    #[test]
    fn test_lynk_alarm_status_2bit() {
        // LYNK alarm status: 2-bit field
        // 0 = OK, 1 = Warning, 2 = Alarm, 3 = Critical
        let data = [0b00_10_01_00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        // Alarm 0 at bit 0-1 -> 0 (OK)
        let point0 = create_point(1, 0x359, 0, 0, 2, CanDataType::UInt8);
        let value0 = decode_point(&point0, &data).unwrap();
        assert_eq!(value0, Value::Integer(0));

        // Alarm 1 at bit 2-3 -> 1 (Warning)
        let point1 = create_point(2, 0x359, 0, 2, 2, CanDataType::UInt8);
        let value1 = decode_point(&point1, &data).unwrap();
        assert_eq!(value1, Value::Integer(1));

        // Alarm 2 at bit 4-5 -> 2 (Alarm)
        let point2 = create_point(3, 0x359, 0, 4, 2, CanDataType::UInt8);
        let value2 = decode_point(&point2, &data).unwrap();
        assert_eq!(value2, Value::Integer(2));
    }
}
