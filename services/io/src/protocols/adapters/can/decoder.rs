//! CAN frame data decoder
//!
//! Provides functions to extract and decode fields from CAN frame data,
//! supporting various data types with Little-Endian byte ordering

use crate::protocols::adapters::can::config::{CanDataType, CanPoint};
use crate::protocols::core::data::Value;
use crate::protocols::core::error::{GatewayError, Result};

use std::collections::HashMap;

/// Point mapping manager
pub struct PointManager {
    /// All points indexed by point_id
    points: HashMap<u32, CanPoint>,
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
    pub fn apply_mappings(
        &self,
        frame_cache: &super::config::CanFrameCache,
    ) -> Result<HashMap<u32, Value>> {
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

/// Extract a field from CAN data (stack-allocated, no heap allocation)
fn extract_field(
    data: &[u8],
    byte_offset: u8,
    bit_position: u8,
    bit_length: u8,
) -> Result<ExtractedField> {
    let byte_offset = byte_offset as usize;

    // Validate parameters
    if byte_offset >= data.len() {
        return Err(GatewayError::Protocol(format!(
            "Byte offset {} out of range (data length: {})",
            byte_offset,
            data.len()
        )));
    }

    // Handle different bit lengths
    match bit_length {
        2 => {
            // 2-bit field (for alarm status)
            if bit_position > 6 {
                return Err(GatewayError::Protocol(format!(
                    "Invalid bit position {} for 2-bit field",
                    bit_position
                )));
            }
            let byte = data[byte_offset];
            let value = (byte >> bit_position) & 0x03;
            Ok(ExtractedField::single(value))
        },
        8 => {
            // Single byte
            if bit_position != 0 {
                return Err(GatewayError::Protocol(format!(
                    "8-bit field must start at bit position 0, got {}",
                    bit_position
                )));
            }
            Ok(ExtractedField::single(data[byte_offset]))
        },
        16 => {
            // 2 bytes (Little-Endian)
            if bit_position != 0 {
                return Err(GatewayError::Protocol(format!(
                    "16-bit field must start at bit position 0, got {}",
                    bit_position
                )));
            }
            if byte_offset + 2 > data.len() {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for 16-bit field at offset {}",
                    byte_offset
                )));
            }
            Ok(ExtractedField::new(&data[byte_offset..byte_offset + 2]))
        },
        32 => {
            // 4 bytes (Little-Endian)
            if bit_position != 0 {
                return Err(GatewayError::Protocol(format!(
                    "32-bit field must start at bit position 0, got {}",
                    bit_position
                )));
            }
            if byte_offset + 4 > data.len() {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for 32-bit field at offset {}",
                    byte_offset
                )));
            }
            Ok(ExtractedField::new(&data[byte_offset..byte_offset + 4]))
        },
        64 => {
            // 8 bytes (for ASCII strings)
            if bit_position != 0 {
                return Err(GatewayError::Protocol(format!(
                    "64-bit field must start at bit position 0, got {}",
                    bit_position
                )));
            }
            if byte_offset + 8 > data.len() {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for 64-bit field at offset {}",
                    byte_offset
                )));
            }
            Ok(ExtractedField::new(&data[byte_offset..byte_offset + 8]))
        },
        _ => Err(GatewayError::Protocol(format!(
            "Unsupported bit length: {}",
            bit_length
        ))),
    }
}

/// Decode a CAN point
fn decode_point(point: &CanPoint, frame_data: &[u8]) -> Result<Value> {
    // Extract raw bytes (stack-allocated)
    let field = extract_field(
        frame_data,
        point.byte_offset,
        point.bit_position,
        point.bit_length,
    )?;
    let raw_bytes = field.as_slice();

    // Decode based on data type (enum matching - faster than string comparison)
    let raw_value = match point.data_type {
        CanDataType::UInt8 => {
            if raw_bytes.is_empty() {
                return Err(GatewayError::Protocol("Empty data for uint8".to_string()));
            }
            Value::Integer(raw_bytes[0] as i64)
        },
        CanDataType::UInt16 => {
            if raw_bytes.len() < 2 {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for uint16, got {}",
                    raw_bytes.len()
                )));
            }
            let raw = u16::from_le_bytes([raw_bytes[0], raw_bytes[1]]);
            Value::Integer(raw as i64)
        },
        CanDataType::Int16 => {
            if raw_bytes.len() < 2 {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for int16, got {}",
                    raw_bytes.len()
                )));
            }
            let raw = i16::from_le_bytes([raw_bytes[0], raw_bytes[1]]);
            Value::Integer(raw as i64)
        },
        CanDataType::UInt32 => {
            if raw_bytes.len() < 4 {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for uint32, got {}",
                    raw_bytes.len()
                )));
            }
            let raw = u32::from_le_bytes([raw_bytes[0], raw_bytes[1], raw_bytes[2], raw_bytes[3]]);
            Value::Integer(raw as i64)
        },
        CanDataType::Int32 => {
            if raw_bytes.len() < 4 {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for int32, got {}",
                    raw_bytes.len()
                )));
            }
            let raw = i32::from_le_bytes([raw_bytes[0], raw_bytes[1], raw_bytes[2], raw_bytes[3]]);
            Value::Integer(raw as i64)
        },
        CanDataType::Float32 => {
            if raw_bytes.len() < 4 {
                return Err(GatewayError::Protocol(format!(
                    "Not enough bytes for float32, got {}",
                    raw_bytes.len()
                )));
            }
            let raw = f32::from_le_bytes([raw_bytes[0], raw_bytes[1], raw_bytes[2], raw_bytes[3]]);
            Value::Float(raw as f64)
        },
        CanDataType::Ascii => {
            // Decode ASCII string, stopping at first null byte
            // Use into_owned() + in-place truncation to avoid double allocation
            let mut s = String::from_utf8_lossy(raw_bytes).into_owned();
            while s.ends_with('\0') {
                s.pop();
            }
            Value::String(s)
        },
    };

    // Apply scale and offset for numeric values
    let final_value = match raw_value {
        Value::Integer(i) if point.scale != 1.0 || point.offset != 0.0 => {
            let scaled = (i as f64) * point.scale + point.offset;
            Value::Float(scaled)
        },
        other => other,
    };

    Ok(final_value)
}

// NOTE: Tests for CAN decoder have been moved to the cross-platform module
// at `protocols/adapters/can_decoder.rs` to enable testing on non-Linux platforms.
