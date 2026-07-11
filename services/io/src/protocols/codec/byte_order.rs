//! Byte order conversion utilities.
//!
//! Provides functions for reading and writing multi-byte values
//! with configurable byte order.

use crate::protocols::core::data::Value;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::{ByteOrder, DataFormat};

/// Decode a value from 16-bit registers.
///
/// # Arguments
///
/// * `registers` - Slice of 16-bit register values
/// * `format` - Data format to decode
/// * `byte_order` - Byte order for multi-byte values
/// * `bit_position` - Bit position for boolean extraction (0-15)
///
/// # Returns
///
/// Decoded `Value` or error
pub fn decode_registers(
    registers: &[u16],
    format: DataFormat,
    byte_order: ByteOrder,
    bit_position: Option<u8>,
) -> Result<Value> {
    match format {
        DataFormat::Bool => {
            if registers.is_empty() {
                return Err(GatewayError::invalid_data("No registers for bool"));
            }
            let bit = bit_position.unwrap_or(0);
            if bit > 15 {
                return Err(GatewayError::invalid_data("Bit position must be 0-15"));
            }
            Ok(Value::Bool((registers[0] >> bit) & 1 == 1))
        },

        DataFormat::UInt16 => {
            if registers.is_empty() {
                return Err(GatewayError::invalid_data("No registers for uint16"));
            }
            Ok(Value::Integer(registers[0] as i64))
        },

        DataFormat::Int16 => {
            if registers.is_empty() {
                return Err(GatewayError::invalid_data("No registers for int16"));
            }
            Ok(Value::Integer(registers[0] as i16 as i64))
        },

        DataFormat::UInt32 | DataFormat::Int32 => {
            if registers.len() < 2 {
                return Err(GatewayError::invalid_data(
                    "Need 2 registers for 32-bit value",
                ));
            }
            let bytes = reorder_bytes_32(registers[0], registers[1], byte_order);
            let v = if format == DataFormat::UInt32 {
                u32::from_be_bytes(bytes) as i64
            } else {
                i32::from_be_bytes(bytes) as i64
            };
            Ok(Value::Integer(v))
        },

        DataFormat::Float32 => {
            if registers.len() < 2 {
                return Err(GatewayError::invalid_data("Need 2 registers for float32"));
            }
            let bytes = reorder_bytes_32(registers[0], registers[1], byte_order);
            let value = f32::from_be_bytes(bytes);
            if !value.is_finite() {
                return Err(GatewayError::invalid_data("Invalid float32 value"));
            }
            Ok(Value::Float(value as f64))
        },

        DataFormat::UInt64 | DataFormat::Int64 => {
            if registers.len() < 4 {
                return Err(GatewayError::invalid_data(
                    "Need 4 registers for 64-bit value",
                ));
            }
            let bytes = reorder_bytes_64(&registers[0..4], byte_order);
            Ok(Value::Integer(i64::from_be_bytes(bytes)))
        },

        DataFormat::Float64 => {
            if registers.len() < 4 {
                return Err(GatewayError::invalid_data("Need 4 registers for float64"));
            }
            let bytes = reorder_bytes_64(&registers[0..4], byte_order);
            let value = f64::from_be_bytes(bytes);
            if !value.is_finite() {
                return Err(GatewayError::invalid_data("Invalid float64 value"));
            }
            Ok(Value::Float(value))
        },

        DataFormat::String => {
            // Pre-allocate: each register can produce up to 2 characters
            let mut s = String::with_capacity(registers.len() * 2);
            for &reg in registers {
                let hi = (reg >> 8) as u8;
                let lo = (reg & 0xFF) as u8;
                if hi != 0 {
                    s.push(hi as char);
                }
                if lo != 0 {
                    s.push(lo as char);
                }
            }
            // Note: The loop already filters null bytes, so trim is defensive only
            // Truncate in place to avoid extra allocation from trim_end_matches().to_string()
            while s.ends_with('\0') {
                s.pop();
            }
            Ok(Value::String(s))
        },
    }
}

/// Encode a value to 16-bit registers.
///
/// # Arguments
///
/// * `value` - Value to encode
/// * `format` - Target data format
/// * `byte_order` - Byte order for multi-byte values
///
/// # Returns
///
/// Vec of 16-bit register values
pub fn encode_registers(
    value: &Value,
    format: DataFormat,
    byte_order: ByteOrder,
) -> Result<Vec<u16>> {
    match format {
        DataFormat::Bool => {
            let b = value
                .as_bool()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to bool"))?;
            Ok(vec![if b { 1 } else { 0 }])
        },

        DataFormat::UInt16 => {
            let v = value
                .as_i64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to integer"))?;
            if v < 0 || v > u16::MAX as i64 {
                return Err(GatewayError::invalid_data(format!(
                    "Value {v} out of u16 range [0, 65535]"
                )));
            }
            Ok(vec![v as u16])
        },

        DataFormat::Int16 => {
            let v = value
                .as_i64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to integer"))?;
            if v < i16::MIN as i64 || v > i16::MAX as i64 {
                return Err(GatewayError::invalid_data(format!(
                    "Value {v} out of i16 range [-32768, 32767]"
                )));
            }
            Ok(vec![v as i16 as u16])
        },

        DataFormat::UInt32 => {
            let v = value
                .as_i64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to integer"))?;
            if v < 0 || v > u32::MAX as i64 {
                return Err(GatewayError::invalid_data(format!(
                    "Value {v} out of u32 range [0, 4294967295]"
                )));
            }
            let bytes = (v as u32).to_be_bytes();
            let (r0, r1) = reorder_to_registers_32(bytes, byte_order);
            Ok(vec![r0, r1])
        },

        DataFormat::Int32 => {
            let v = value
                .as_i64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to integer"))?;
            if v < i32::MIN as i64 || v > i32::MAX as i64 {
                return Err(GatewayError::invalid_data(format!(
                    "Value {v} out of i32 range [-2147483648, 2147483647]"
                )));
            }
            let bytes = (v as i32 as u32).to_be_bytes();
            let (r0, r1) = reorder_to_registers_32(bytes, byte_order);
            Ok(vec![r0, r1])
        },

        DataFormat::Float32 => {
            let v = value
                .as_f64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to float"))?;
            if !v.is_finite() {
                return Err(GatewayError::invalid_data(
                    "Cannot encode NaN/Infinity as float32",
                ));
            }
            let bytes = (v as f32).to_be_bytes();
            let (r0, r1) = reorder_to_registers_32(bytes, byte_order);
            Ok(vec![r0, r1])
        },

        DataFormat::UInt64 | DataFormat::Int64 => {
            let v = value
                .as_i64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to integer"))?;
            let bytes = v.to_be_bytes();
            let regs = reorder_to_registers_64(bytes, byte_order);
            Ok(regs.to_vec())
        },

        DataFormat::Float64 => {
            let v = value
                .as_f64()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to float"))?;
            if !v.is_finite() {
                return Err(GatewayError::invalid_data(
                    "Cannot encode NaN/Infinity as float64",
                ));
            }
            let bytes = v.to_be_bytes();
            let regs = reorder_to_registers_64(bytes, byte_order);
            Ok(regs.to_vec())
        },

        DataFormat::String => {
            let s = value
                .as_string()
                .ok_or_else(|| GatewayError::invalid_data("Cannot convert to string"))?;
            let bytes = s.as_bytes();
            // Pre-allocate: 2 bytes per register, round up
            let mut regs = Vec::with_capacity(bytes.len().div_ceil(2));
            for chunk in bytes.chunks(2) {
                let hi = chunk[0] as u16;
                let lo = chunk.get(1).copied().unwrap_or(0) as u16;
                regs.push((hi << 8) | lo);
            }
            Ok(regs)
        },
    }
}

/// Reorder 2 registers (4 bytes) according to byte order.
fn reorder_bytes_32(r0: u16, r1: u16, order: ByteOrder) -> [u8; 4] {
    let a = (r0 >> 8) as u8;
    let b = (r0 & 0xFF) as u8;
    let c = (r1 >> 8) as u8;
    let d = (r1 & 0xFF) as u8;

    match order {
        ByteOrder::Abcd => [a, b, c, d],
        ByteOrder::Dcba => [d, c, b, a],
        ByteOrder::Badc => [b, a, d, c],
        ByteOrder::Cdab => [c, d, a, b],
    }
}

/// Reorder 4 registers (8 bytes) according to byte order.
fn reorder_bytes_64(regs: &[u16], order: ByteOrder) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    for (i, &reg) in regs.iter().take(4).enumerate() {
        bytes[i * 2] = (reg >> 8) as u8;
        bytes[i * 2 + 1] = (reg & 0xFF) as u8;
    }

    match order {
        ByteOrder::Abcd => bytes,
        ByteOrder::Dcba => {
            bytes.reverse();
            bytes
        },
        ByteOrder::Badc => {
            // Swap within each word
            [
                bytes[1], bytes[0], bytes[3], bytes[2], bytes[5], bytes[4], bytes[7], bytes[6],
            ]
        },
        ByteOrder::Cdab => {
            // Swap word pairs
            [
                bytes[2], bytes[3], bytes[0], bytes[1], bytes[6], bytes[7], bytes[4], bytes[5],
            ]
        },
    }
}

/// Convert 4 bytes to 2 registers with byte order.
fn reorder_to_registers_32(bytes: [u8; 4], order: ByteOrder) -> (u16, u16) {
    let [a, b, c, d] = bytes;

    let (b0, b1, b2, b3) = match order {
        ByteOrder::Abcd => (a, b, c, d),
        ByteOrder::Dcba => (d, c, b, a),
        ByteOrder::Badc => (b, a, d, c),
        ByteOrder::Cdab => (c, d, a, b),
    };

    let r0 = ((b0 as u16) << 8) | (b1 as u16);
    let r1 = ((b2 as u16) << 8) | (b3 as u16);
    (r0, r1)
}

/// Convert 8 bytes to 4 registers with byte order.
fn reorder_to_registers_64(bytes: [u8; 8], order: ByteOrder) -> [u16; 4] {
    let ordered = match order {
        ByteOrder::Abcd => bytes,
        ByteOrder::Dcba => {
            let mut b = bytes;
            b.reverse();
            b
        },
        ByteOrder::Badc => [
            bytes[1], bytes[0], bytes[3], bytes[2], bytes[5], bytes[4], bytes[7], bytes[6],
        ],
        ByteOrder::Cdab => [
            bytes[2], bytes[3], bytes[0], bytes[1], bytes[6], bytes[7], bytes[4], bytes[5],
        ],
    };

    [
        ((ordered[0] as u16) << 8) | (ordered[1] as u16),
        ((ordered[2] as u16) << 8) | (ordered[3] as u16),
        ((ordered[4] as u16) << 8) | (ordered[5] as u16),
        ((ordered[6] as u16) << 8) | (ordered[7] as u16),
    ]
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn test_decode_uint16() {
        let regs = [0x1234];
        let v = decode_registers(&regs, DataFormat::UInt16, ByteOrder::Abcd, None).unwrap();
        assert_eq!(v.as_i64(), Some(0x1234));
    }

    #[test]
    fn test_decode_int16() {
        let regs = [0xFFFF]; // -1 in signed
        let v = decode_registers(&regs, DataFormat::Int16, ByteOrder::Abcd, None).unwrap();
        assert_eq!(v.as_i64(), Some(-1));
    }

    #[test]
    fn test_decode_float32_abcd() {
        // 42.0 in IEEE 754: 0x42280000
        let regs = [0x4228, 0x0000];
        let v = decode_registers(&regs, DataFormat::Float32, ByteOrder::Abcd, None).unwrap();
        assert!((v.as_f64().unwrap() - 42.0).abs() < 0.001);
    }

    #[test]
    fn test_decode_float32_cdab() {
        // 42.0 with CDAB order
        let regs = [0x0000, 0x4228];
        let v = decode_registers(&regs, DataFormat::Float32, ByteOrder::Cdab, None).unwrap();
        assert!((v.as_f64().unwrap() - 42.0).abs() < 0.001);
    }

    #[test]
    fn test_decode_bool() {
        let regs = [0x0004]; // bit 2 set
        let v = decode_registers(&regs, DataFormat::Bool, ByteOrder::Abcd, Some(2)).unwrap();
        assert_eq!(v.as_bool(), Some(true));

        let v = decode_registers(&regs, DataFormat::Bool, ByteOrder::Abcd, Some(0)).unwrap();
        assert_eq!(v.as_bool(), Some(false));
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let original = Value::Float(123.456);
        let regs = encode_registers(&original, DataFormat::Float32, ByteOrder::Abcd).unwrap();
        let decoded = decode_registers(&regs, DataFormat::Float32, ByteOrder::Abcd, None).unwrap();

        let orig_f = original.as_f64().unwrap();
        let decoded_f = decoded.as_f64().unwrap();
        assert!((orig_f - decoded_f).abs() < 0.001);
    }
}
