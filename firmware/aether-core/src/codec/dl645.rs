//! DL/T 645-2007 Protocol Codec
//!
//! This module implements the Chinese smart meter communication protocol
//! DL/T 645-2007, which is widely used in power distribution automation.
//!
//! ## Frame Format
//!
//! ```text
//! ┌─────┬────────┬─────┬──────┬─────┬──────────┬────┬─────┐
//! │ 68H │ A0..A5 │ 68H │ Ctrl │ Len │ Data     │ CS │ 16H │
//! │ 1B  │ 6 bytes│ 1B  │ 1B   │ 1B  │ N bytes  │ 1B │ 1B  │
//! └─────┴────────┴─────┴──────┴─────┴──────────┴────┴─────┘
//!
//! - 68H: Start marker
//! - A0..A5: Meter address (BCD, little-endian)
//! - Ctrl: Control code
//! - Len: Data field length
//! - Data: Data identifier + actual data (encoded with +0x33)
//! - CS: Checksum (sum of all bytes from 68H to before CS, mod 256)
//! - 16H: End marker
//! ```
//!
//! ## Data Encoding
//!
//! All data bytes are encoded by adding 0x33 before transmission,
//! and decoded by subtracting 0x33 when received.

use core::fmt;

/// Frame start marker.
pub const FRAME_START: u8 = 0x68;

/// Frame end marker.
pub const FRAME_END: u8 = 0x16;

/// Control code: Read data request.
pub const CTRL_READ_DATA: u8 = 0x11;

/// Control code: Read data response (success).
pub const CTRL_READ_DATA_RESP: u8 = 0x91;

/// Control code: Read data response (error).
pub const CTRL_READ_DATA_ERR: u8 = 0xD1;

/// Data encoding offset.
pub const DATA_OFFSET: u8 = 0x33;

/// Maximum frame size.
pub const MAX_FRAME_SIZE: usize = 256;

/// Broadcast address (all 0x99).
pub const BROADCAST_ADDR: [u8; 6] = [0x99, 0x99, 0x99, 0x99, 0x99, 0x99];

/// DL/T 645 error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dl645Error {
    /// No data available.
    NoData,
    /// Date/time error.
    DateError,
    /// No permission.
    NoPermission,
    /// Frame format error.
    FrameError,
    /// Checksum mismatch.
    ChecksumError,
    /// Invalid frame length.
    InvalidLength,
    /// Invalid frame markers.
    InvalidMarkers,
    /// Address parse error.
    AddressError,
    /// Other/unknown error.
    OtherError,
}

impl fmt::Display for Dl645Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoData => write!(f, "No data"),
            Self::DateError => write!(f, "Date error"),
            Self::NoPermission => write!(f, "No permission"),
            Self::FrameError => write!(f, "Frame error"),
            Self::ChecksumError => write!(f, "Checksum error"),
            Self::InvalidLength => write!(f, "Invalid length"),
            Self::InvalidMarkers => write!(f, "Invalid markers"),
            Self::AddressError => write!(f, "Address error"),
            Self::OtherError => write!(f, "Other error"),
        }
    }
}

/// Meter address (6 bytes BCD).
///
/// Format: XX XX XX XX XX XX where each byte is a BCD digit pair.
/// Wire format uses little-endian byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterAddress {
    /// Address bytes in display order (big-endian).
    pub bytes: [u8; 6],
}

impl MeterAddress {
    /// Create a broadcast address.
    #[inline]
    pub const fn broadcast() -> Self {
        Self {
            bytes: BROADCAST_ADDR,
        }
    }

    /// Create from display string (e.g., "123456789012").
    ///
    /// The string must be exactly 12 hex digits.
    pub fn parse(s: &str) -> Result<Self, Dl645Error> {
        let bytes = s.as_bytes();
        if bytes.len() != 12 {
            return Err(Dl645Error::AddressError);
        }

        let mut addr = [0u8; 6];
        for i in 0..6 {
            let high = hex_char_to_nibble(bytes[i * 2]).ok_or(Dl645Error::AddressError)?;
            let low = hex_char_to_nibble(bytes[i * 2 + 1]).ok_or(Dl645Error::AddressError)?;
            addr[i] = (high << 4) | low;
        }

        Ok(Self { bytes: addr })
    }

    /// Create from wire bytes (little-endian).
    pub fn from_wire_bytes(wire: &[u8]) -> Result<Self, Dl645Error> {
        if wire.len() < 6 {
            return Err(Dl645Error::AddressError);
        }

        let mut bytes = [0u8; 6];
        // Reverse byte order (wire is little-endian)
        for i in 0..6 {
            bytes[i] = wire[5 - i];
        }

        Ok(Self { bytes })
    }

    /// Convert to wire bytes (little-endian).
    #[inline]
    pub fn to_wire_bytes(&self) -> [u8; 6] {
        let mut wire = [0u8; 6];
        for (i, byte) in wire.iter_mut().enumerate() {
            *byte = self.bytes[5 - i];
        }
        wire
    }
}

impl fmt::Display for MeterAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.bytes {
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// Data identifier (4 bytes).
///
/// Identifies the data item to read/write.
/// Common identifiers:
/// - 0x00010000: Total active energy (forward)
/// - 0x00020000: Total active energy (reverse)
/// - 0x02010100: A-phase voltage
/// - 0x02020100: A-phase current
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataIdentifier {
    /// Identifier bytes in display order.
    pub bytes: [u8; 4],
}

impl DataIdentifier {
    /// Create from hex string (e.g., "00010000").
    pub fn from_hex_str(s: &str) -> Result<Self, Dl645Error> {
        let bytes = s.as_bytes();
        if bytes.len() != 8 {
            return Err(Dl645Error::FrameError);
        }

        let mut di = [0u8; 4];
        for i in 0..4 {
            let high = hex_char_to_nibble(bytes[i * 2]).ok_or(Dl645Error::FrameError)?;
            let low = hex_char_to_nibble(bytes[i * 2 + 1]).ok_or(Dl645Error::FrameError)?;
            di[i] = (high << 4) | low;
        }

        Ok(Self { bytes: di })
    }

    /// Create from raw bytes.
    #[inline]
    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        Self { bytes }
    }

    /// Create from u32 (network byte order).
    #[inline]
    pub const fn from_u32(value: u32) -> Self {
        Self {
            bytes: value.to_be_bytes(),
        }
    }

    /// Convert to wire bytes (little-endian, for transmission).
    #[inline]
    pub fn to_wire_bytes(&self) -> [u8; 4] {
        let mut wire = [0u8; 4];
        for (i, byte) in wire.iter_mut().enumerate() {
            *byte = self.bytes[3 - i];
        }
        wire
    }

    /// Convert to hex string.
    pub fn to_hex_string(&self) -> [u8; 8] {
        let mut hex = [0u8; 8];
        for (i, b) in self.bytes.iter().enumerate() {
            hex[i * 2] = nibble_to_hex_char(b >> 4);
            hex[i * 2 + 1] = nibble_to_hex_char(b & 0x0F);
        }
        hex
    }
}

impl fmt::Display for DataIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.bytes {
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// Decoded DL/T 645 frame.
#[derive(Debug)]
pub struct Dl645Frame {
    /// Meter address.
    pub meter_addr: MeterAddress,
    /// Data identifier.
    pub data_id: DataIdentifier,
    /// Decoded data bytes.
    pub data: [u8; 64],
    /// Actual length of data.
    pub data_len: usize,
}

// ============================================================================
// Helper functions
// ============================================================================

/// Convert hex character to nibble value.
#[inline]
fn hex_char_to_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Convert nibble to hex character.
#[inline]
const fn nibble_to_hex_char(n: u8) -> u8 {
    match n & 0x0F {
        0..=9 => b'0' + n,
        _ => b'A' + (n - 10),
    }
}

/// Encode data bytes (add 0x33).
#[inline]
pub fn encode_data(data: &[u8]) -> [u8; 64] {
    let mut encoded = [0u8; 64];
    for (i, &b) in data.iter().enumerate() {
        if i >= 64 {
            break;
        }
        encoded[i] = b.wrapping_add(DATA_OFFSET);
    }
    encoded
}

/// Decode data bytes (subtract 0x33).
#[inline]
pub fn decode_data(data: &[u8]) -> [u8; 64] {
    let mut decoded = [0u8; 64];
    for (i, &b) in data.iter().enumerate() {
        if i >= 64 {
            break;
        }
        decoded[i] = b.wrapping_sub(DATA_OFFSET);
    }
    decoded
}

/// Calculate checksum (sum of all bytes mod 256).
pub fn calculate_checksum(frame: &[u8]) -> u8 {
    let mut sum: u16 = 0;
    for &b in frame {
        sum = sum.wrapping_add(b as u16);
    }
    (sum & 0xFF) as u8
}

/// Encode a read data request frame.
///
/// Returns (frame_buffer, frame_length).
pub fn encode_read_request(
    meter_addr: &MeterAddress,
    data_id: &DataIdentifier,
) -> ([u8; MAX_FRAME_SIZE], usize) {
    let mut frame = [0u8; MAX_FRAME_SIZE];
    let mut pos = 0;

    // Start marker
    frame[pos] = FRAME_START;
    pos += 1;

    // Meter address (6 bytes, reversed)
    let addr_bytes = meter_addr.to_wire_bytes();
    frame[pos..pos + 6].copy_from_slice(&addr_bytes);
    pos += 6;

    // Second start marker
    frame[pos] = FRAME_START;
    pos += 1;

    // Control code (read data)
    frame[pos] = CTRL_READ_DATA;
    pos += 1;

    // Data length (4 bytes for DI)
    frame[pos] = 4;
    pos += 1;

    // Data identifier (encoded with +0x33)
    let di_wire = data_id.to_wire_bytes();
    for &b in &di_wire {
        frame[pos] = b.wrapping_add(DATA_OFFSET);
        pos += 1;
    }

    // Calculate checksum (from first 0x68 to before CS)
    let cs = calculate_checksum(&frame[..pos]);
    frame[pos] = cs;
    pos += 1;

    // End marker
    frame[pos] = FRAME_END;
    pos += 1;

    (frame, pos)
}

/// Decode a response frame.
///
/// Returns the parsed frame or an error.
pub fn decode_response(frame: &[u8]) -> Result<Dl645Frame, Dl645Error> {
    // Minimum frame length: 68 + 6 + 68 + C + L + 4(DI) + CS + 16 = 14 bytes
    if frame.len() < 14 {
        return Err(Dl645Error::InvalidLength);
    }

    // Verify frame markers
    if frame[0] != FRAME_START || frame[7] != FRAME_START {
        return Err(Dl645Error::InvalidMarkers);
    }

    let frame_end_idx = frame.len() - 1;
    if frame[frame_end_idx] != FRAME_END {
        return Err(Dl645Error::InvalidMarkers);
    }

    // Verify checksum
    let cs_idx = frame_end_idx - 1;
    let expected_cs = calculate_checksum(&frame[..cs_idx]);
    if frame[cs_idx] != expected_cs {
        return Err(Dl645Error::ChecksumError);
    }

    // Parse meter address (bytes 1-6, reversed)
    let meter_addr = MeterAddress::from_wire_bytes(&frame[1..7])?;

    // Parse control code and data length
    let ctrl = frame[8];
    let data_len = frame[9] as usize;

    // Verify data length
    let expected_len = 10 + data_len + 2; // header + data + CS + end
    if frame.len() != expected_len {
        return Err(Dl645Error::InvalidLength);
    }

    // Check for error response
    if ctrl == CTRL_READ_DATA_ERR {
        if data_len > 0 {
            let error_code = frame[10].wrapping_sub(DATA_OFFSET);
            return Err(match error_code {
                0x01 => Dl645Error::NoData,
                0x02 => Dl645Error::DateError,
                0x04 => Dl645Error::NoPermission,
                0x08 => Dl645Error::FrameError,
                _ => Dl645Error::OtherError,
            });
        }
        return Err(Dl645Error::OtherError);
    }

    // Verify normal response control code
    if ctrl != CTRL_READ_DATA_RESP {
        return Err(Dl645Error::FrameError);
    }

    // Decode data (remove +0x33 encoding)
    let encoded_data = &frame[10..10 + data_len];
    let decoded = decode_data(encoded_data);

    // First 4 bytes are DI echo (reversed)
    if data_len < 4 {
        return Err(Dl645Error::InvalidLength);
    }

    let mut di_bytes = [0u8; 4];
    for i in 0..4 {
        di_bytes[i] = decoded[3 - i];
    }
    let data_id = DataIdentifier::from_bytes(di_bytes);

    // Remaining bytes are actual data
    let mut data = [0u8; 64];
    let actual_data_len = data_len - 4;
    data[..actual_data_len].copy_from_slice(&decoded[4..4 + actual_data_len]);

    Ok(Dl645Frame {
        meter_addr,
        data_id,
        data,
        data_len: actual_data_len,
    })
}

// ============================================================================
// BCD parsing utilities
// ============================================================================

/// Convert BCD bytes to u32.
///
/// BCD format: each nibble is a decimal digit (0-9).
pub fn bcd_to_u32(data: &[u8]) -> u32 {
    let mut value: u32 = 0;
    let mut multiplier: u32 = 1;

    for &byte in data {
        let low = (byte & 0x0F) as u32;
        let high = ((byte >> 4) & 0x0F) as u32;

        value += low * multiplier;
        multiplier *= 10;
        value += high * multiplier;
        multiplier *= 10;
    }

    value
}

/// Parse energy value (BCD, 4 bytes, unit: 0.01 kWh).
///
/// Returns energy in kWh.
pub fn parse_energy(data: &[u8]) -> f64 {
    if data.len() < 4 {
        return 0.0;
    }
    let raw = bcd_to_u32(&data[0..4]);
    raw as f64 * 0.01
}

/// Parse voltage value (BCD, 2 bytes, unit: 0.1 V).
///
/// Returns voltage in V.
pub fn parse_voltage(data: &[u8]) -> f64 {
    if data.len() < 2 {
        return 0.0;
    }
    let raw = bcd_to_u32(&data[0..2]);
    raw as f64 * 0.1
}

/// Parse current value (BCD, 3 bytes, unit: 0.001 A).
///
/// Returns current in A.
pub fn parse_current(data: &[u8]) -> f64 {
    if data.len() < 3 {
        return 0.0;
    }
    let raw = bcd_to_u32(&data[0..3]);
    raw as f64 * 0.001
}

/// Parse power value (BCD, 3 bytes, unit: 0.0001 kW).
///
/// Returns power in kW.
pub fn parse_power(data: &[u8]) -> f64 {
    if data.len() < 3 {
        return 0.0;
    }
    let raw = bcd_to_u32(&data[0..3]);
    raw as f64 * 0.0001
}

/// Parse power factor (BCD, 2 bytes, unit: 0.001).
pub fn parse_power_factor(data: &[u8]) -> f64 {
    if data.len() < 2 {
        return 0.0;
    }
    let raw = bcd_to_u32(&data[0..2]);
    raw as f64 * 0.001
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_meter_address_parse() {
        let addr = MeterAddress::parse("123456789012").unwrap();
        assert_eq!(addr.bytes, [0x12, 0x34, 0x56, 0x78, 0x90, 0x12]);
    }

    #[test]
    fn test_meter_address_wire_format() {
        let addr = MeterAddress::parse("123456789012").unwrap();
        let wire = addr.to_wire_bytes();
        // Wire format is little-endian (reversed)
        assert_eq!(wire, [0x12, 0x90, 0x78, 0x56, 0x34, 0x12]);

        // Round-trip
        let restored = MeterAddress::from_wire_bytes(&wire).unwrap();
        assert_eq!(restored.bytes, addr.bytes);
    }

    #[test]
    fn test_data_identifier() {
        let di = DataIdentifier::from_hex_str("00010000").unwrap();
        assert_eq!(di.bytes, [0x00, 0x01, 0x00, 0x00]);

        let di2 = DataIdentifier::from_u32(0x00010000);
        assert_eq!(di, di2);
    }

    #[test]
    fn test_encode_decode_data() {
        let original = [0x00, 0x01, 0x02, 0x03];
        let encoded = encode_data(&original);
        assert_eq!(&encoded[0..4], [0x33, 0x34, 0x35, 0x36]);

        let decoded = decode_data(&encoded[0..4]);
        assert_eq!(&decoded[0..4], original);
    }

    #[test]
    fn test_checksum() {
        let data = [0x68, 0x12, 0x34];
        let cs = calculate_checksum(&data);
        assert_eq!(cs, (0x68 + 0x12 + 0x34) as u8);
    }

    #[test]
    fn test_encode_read_request() {
        let addr = MeterAddress::parse("123456789012").unwrap();
        let di = DataIdentifier::from_hex_str("00010000").unwrap();

        let (frame, len) = encode_read_request(&addr, &di);

        // Verify structure
        assert_eq!(frame[0], FRAME_START);
        assert_eq!(frame[7], FRAME_START);
        assert_eq!(frame[8], CTRL_READ_DATA);
        assert_eq!(frame[9], 4); // Data length
        assert_eq!(frame[len - 1], FRAME_END);

        // Verify checksum
        let cs = calculate_checksum(&frame[..len - 2]);
        assert_eq!(frame[len - 2], cs);
    }

    #[test]
    fn test_bcd_to_u32() {
        // 0x12 0x34 -> 3412 (little-endian BCD)
        assert_eq!(bcd_to_u32(&[0x12, 0x34]), 3412);
        // 0x00 0x01 0x00 0x00 -> 00000100
        assert_eq!(bcd_to_u32(&[0x00, 0x01, 0x00, 0x00]), 100);
    }

    #[test]
    fn test_parse_energy() {
        // 12345.67 kWh = 1234567 * 0.01
        // BCD: 0x67 0x45 0x23 0x01 (little-endian)
        let data = [0x67, 0x45, 0x23, 0x01];
        let energy = parse_energy(&data);
        assert!((energy - 12345.67).abs() < 0.01);
    }

    #[test]
    fn test_parse_voltage() {
        // 220.5 V = 2205 * 0.1
        // BCD: 0x05 0x22 (little-endian)
        let data = [0x05, 0x22];
        let voltage = parse_voltage(&data);
        assert!((voltage - 220.5).abs() < 0.1);
    }
}
