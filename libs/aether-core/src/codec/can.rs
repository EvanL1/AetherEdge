//! CAN Bus Frame Decoder
//!
//! This module provides no_std compatible decoding of CAN bus frames
//! for industrial applications.
//!
//! ## Supported Data Types
//!
//! - `UInt8`, `UInt16`, `UInt32`: Unsigned integers (little-endian)
//! - `Int16`, `Int32`: Signed integers (little-endian)
//! - `Float32`: IEEE 754 single precision float
//! - `Ascii`: Fixed-length ASCII string (up to 8 bytes)
//!
//! ## Bit Field Extraction
//!
//! Supports extraction of bit fields at arbitrary positions:
//! - 2-bit fields (for alarm status)
//! - 8-bit fields (single byte)
//! - 16-bit, 32-bit, 64-bit fields (must start at bit 0)

use core::fmt;

/// CAN data types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum CanDataType {
    /// Unsigned 8-bit integer.
    UInt8 = 0,
    /// Unsigned 16-bit integer (little-endian).
    UInt16 = 1,
    /// Signed 16-bit integer (little-endian).
    Int16 = 2,
    /// Unsigned 32-bit integer (little-endian).
    UInt32 = 3,
    /// Signed 32-bit integer (little-endian).
    Int32 = 4,
    /// IEEE 754 32-bit float (little-endian).
    Float32 = 5,
    /// ASCII string (up to 8 bytes).
    Ascii = 6,
}

impl CanDataType {
    /// Get the typical bit length for this data type.
    #[inline]
    pub const fn bit_length(&self) -> u8 {
        match self {
            Self::UInt8 => 8,
            Self::UInt16 | Self::Int16 => 16,
            Self::UInt32 | Self::Int32 | Self::Float32 => 32,
            Self::Ascii => 64,
        }
    }
}

/// CAN decode error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanDecodeError {
    /// Byte offset is out of range.
    OffsetOutOfRange,
    /// Invalid bit position for field.
    InvalidBitPosition,
    /// Not enough bytes for data type.
    InsufficientData,
    /// Unsupported bit length.
    UnsupportedBitLength,
}

impl fmt::Display for CanDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffsetOutOfRange => write!(f, "Offset out of range"),
            Self::InvalidBitPosition => write!(f, "Invalid bit position"),
            Self::InsufficientData => write!(f, "Insufficient data"),
            Self::UnsupportedBitLength => write!(f, "Unsupported bit length"),
        }
    }
}

/// Extracted field data (stack-allocated, no heap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractedField {
    /// Raw data bytes.
    data: [u8; 8],
    /// Actual length of data.
    len: u8,
}

impl ExtractedField {
    /// Create a new extracted field.
    pub fn new(bytes: &[u8]) -> Self {
        let mut data = [0u8; 8];
        let len = bytes.len().min(8);
        data[..len].copy_from_slice(&bytes[..len]);
        Self {
            data,
            len: len as u8,
        }
    }

    /// Create a single-byte field.
    #[inline]
    pub const fn single(byte: u8) -> Self {
        Self {
            data: [byte, 0, 0, 0, 0, 0, 0, 0],
            len: 1,
        }
    }

    /// Get the data slice.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get length.
    #[inline]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Check if empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Extract a field from CAN data.
///
/// # Arguments
///
/// * `data` - CAN frame data (up to 8 bytes)
/// * `byte_offset` - Starting byte offset (0-7)
/// * `bit_position` - Bit position within byte (0-7)
/// * `bit_length` - Number of bits to extract (2, 8, 16, 32, or 64)
///
/// # Returns
///
/// Extracted field data or an error.
pub fn extract_field(
    data: &[u8],
    byte_offset: u8,
    bit_position: u8,
    bit_length: u8,
) -> Result<ExtractedField, CanDecodeError> {
    let byte_offset = byte_offset as usize;

    // Validate parameters
    if byte_offset >= data.len() {
        return Err(CanDecodeError::OffsetOutOfRange);
    }

    match bit_length {
        2 => {
            // 2-bit field (for alarm status)
            if bit_position > 6 {
                return Err(CanDecodeError::InvalidBitPosition);
            }
            let byte = data[byte_offset];
            let value = (byte >> bit_position) & 0x03;
            Ok(ExtractedField::single(value))
        },
        8 => {
            // Single byte
            if bit_position != 0 {
                return Err(CanDecodeError::InvalidBitPosition);
            }
            Ok(ExtractedField::single(data[byte_offset]))
        },
        16 => {
            // 2 bytes (little-endian)
            if bit_position != 0 {
                return Err(CanDecodeError::InvalidBitPosition);
            }
            if byte_offset + 2 > data.len() {
                return Err(CanDecodeError::InsufficientData);
            }
            Ok(ExtractedField::new(&data[byte_offset..byte_offset + 2]))
        },
        32 => {
            // 4 bytes (little-endian)
            if bit_position != 0 {
                return Err(CanDecodeError::InvalidBitPosition);
            }
            if byte_offset + 4 > data.len() {
                return Err(CanDecodeError::InsufficientData);
            }
            Ok(ExtractedField::new(&data[byte_offset..byte_offset + 4]))
        },
        64 => {
            // 8 bytes (for ASCII strings)
            if bit_position != 0 {
                return Err(CanDecodeError::InvalidBitPosition);
            }
            if byte_offset + 8 > data.len() {
                return Err(CanDecodeError::InsufficientData);
            }
            Ok(ExtractedField::new(&data[byte_offset..byte_offset + 8]))
        },
        _ => Err(CanDecodeError::UnsupportedBitLength),
    }
}

/// Decoded CAN value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanValue {
    /// Integer value (for UInt8, UInt16, UInt32, Int16, Int32).
    Integer(i64),
    /// Float value (for Float32).
    Float(f64),
    /// ASCII string (for Ascii, up to 8 bytes).
    String([u8; 8], u8),
}

impl CanValue {
    /// Apply scale and offset transformation.
    ///
    /// For integer values, converts to float if scale != 1.0 or offset != 0.0.
    #[inline]
    pub fn apply_transform(self, scale: f64, offset: f64) -> Self {
        match self {
            Self::Integer(i) if scale != 1.0 || offset != 0.0 => {
                Self::Float((i as f64) * scale + offset)
            },
            other => other,
        }
    }

    /// Try to get as f64.
    #[inline]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(i) => Some(*i as f64),
            Self::Float(f) => Some(*f),
            Self::String(_, _) => None,
        }
    }
}

/// Decode a CAN field value.
///
/// # Arguments
///
/// * `field` - Extracted field data
/// * `data_type` - Expected data type
///
/// # Returns
///
/// Decoded value or an error.
pub fn decode_value(
    field: &ExtractedField,
    data_type: CanDataType,
) -> Result<CanValue, CanDecodeError> {
    let bytes = field.as_slice();

    match data_type {
        CanDataType::UInt8 => {
            if bytes.is_empty() {
                return Err(CanDecodeError::InsufficientData);
            }
            Ok(CanValue::Integer(bytes[0] as i64))
        },
        CanDataType::UInt16 => {
            if bytes.len() < 2 {
                return Err(CanDecodeError::InsufficientData);
            }
            let raw = u16::from_le_bytes([bytes[0], bytes[1]]);
            Ok(CanValue::Integer(raw as i64))
        },
        CanDataType::Int16 => {
            if bytes.len() < 2 {
                return Err(CanDecodeError::InsufficientData);
            }
            let raw = i16::from_le_bytes([bytes[0], bytes[1]]);
            Ok(CanValue::Integer(raw as i64))
        },
        CanDataType::UInt32 => {
            if bytes.len() < 4 {
                return Err(CanDecodeError::InsufficientData);
            }
            let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            Ok(CanValue::Integer(raw as i64))
        },
        CanDataType::Int32 => {
            if bytes.len() < 4 {
                return Err(CanDecodeError::InsufficientData);
            }
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            Ok(CanValue::Integer(raw as i64))
        },
        CanDataType::Float32 => {
            if bytes.len() < 4 {
                return Err(CanDecodeError::InsufficientData);
            }
            let raw = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            Ok(CanValue::Float(raw as f64))
        },
        CanDataType::Ascii => {
            let mut buf = [0u8; 8];
            let mut len = 0u8;
            for &b in bytes {
                if b == 0 {
                    break;
                }
                if len < 8 {
                    buf[len as usize] = b;
                    len += 1;
                }
            }
            Ok(CanValue::String(buf, len))
        },
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::approx_constant)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_2bit_field() {
        let data = [0b1011_0101];

        // Extract bits 0-1 (value = 01)
        let field = extract_field(&data, 0, 0, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x01]);

        // Extract bits 2-3 (value = 01)
        let field = extract_field(&data, 0, 2, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x01]);

        // Extract bits 4-5 (value = 11)
        let field = extract_field(&data, 0, 4, 2).unwrap();
        assert_eq!(field.as_slice(), &[0x03]);
    }

    #[test]
    fn test_extract_8bit_field() {
        let data = [0x12, 0x34, 0x56];

        let field = extract_field(&data, 0, 0, 8).unwrap();
        assert_eq!(field.as_slice(), &[0x12]);

        let field = extract_field(&data, 1, 0, 8).unwrap();
        assert_eq!(field.as_slice(), &[0x34]);
    }

    #[test]
    fn test_extract_16bit_field() {
        let data = [0x12, 0x34, 0x56, 0x78];

        let field = extract_field(&data, 0, 0, 16).unwrap();
        assert_eq!(field.as_slice(), &[0x12, 0x34]);

        // Little-endian: 0x3412
        let value = decode_value(&field, CanDataType::UInt16).unwrap();
        assert_eq!(value, CanValue::Integer(0x3412));
    }

    #[test]
    fn test_decode_int16() {
        // -100 in little-endian
        let data = [0x9C, 0xFF];
        let field = ExtractedField::new(&data);
        let value = decode_value(&field, CanDataType::Int16).unwrap();
        assert_eq!(value, CanValue::Integer(-100));
    }

    #[test]
    fn test_decode_float32() {
        // 3.14 as IEEE 754 float, little-endian
        let f: f32 = 3.14;
        let bytes = f.to_le_bytes();
        let field = ExtractedField::new(&bytes);
        let value = decode_value(&field, CanDataType::Float32).unwrap();

        if let CanValue::Float(decoded) = value {
            assert!((decoded - 3.14).abs() < 0.001);
        } else {
            panic!("Expected Float");
        }
    }

    #[test]
    fn test_apply_transform() {
        let value = CanValue::Integer(100);

        // No transform
        let result = value.apply_transform(1.0, 0.0);
        assert_eq!(result, CanValue::Integer(100));

        // With scale
        let result = value.apply_transform(0.1, 0.0);
        assert_eq!(result, CanValue::Float(10.0));

        // With offset
        let result = value.apply_transform(1.0, 5.0);
        assert_eq!(result, CanValue::Float(105.0));

        // With both
        let result = value.apply_transform(0.1, 2.0);
        assert_eq!(result, CanValue::Float(12.0));
    }

    #[test]
    fn test_decode_ascii() {
        let data = b"HELLO\0\0\0";
        let field = ExtractedField::new(data);
        let value = decode_value(&field, CanDataType::Ascii).unwrap();

        if let CanValue::String(buf, len) = value {
            assert_eq!(len, 5);
            assert_eq!(&buf[..5], b"HELLO");
        } else {
            panic!("Expected String");
        }
    }

    #[test]
    fn test_error_cases() {
        let data = [0x12, 0x34];

        // Offset out of range
        assert_eq!(
            extract_field(&data, 5, 0, 8),
            Err(CanDecodeError::OffsetOutOfRange)
        );

        // Invalid bit position for 8-bit field
        assert_eq!(
            extract_field(&data, 0, 1, 8),
            Err(CanDecodeError::InvalidBitPosition)
        );

        // Insufficient data for 32-bit field
        assert_eq!(
            extract_field(&data, 0, 0, 32),
            Err(CanDecodeError::InsufficientData)
        );

        // Unsupported bit length
        assert_eq!(
            extract_field(&data, 0, 0, 24),
            Err(CanDecodeError::UnsupportedBitLength)
        );
    }
}
