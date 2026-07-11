//! DL/T 645-2007 Protocol Adapter
//!
//! This module implements the DL/T 645-2007 protocol for reading data from
//! intelligent electricity meters. The protocol is widely used in China for
//! meter data collection.
//!
//! # Protocol Overview
//!
//! - Frame format: 68H + Address(6B) + 68H + Control + Length + Data + CS + 16H
//! - Data encoding: Each byte in data field is XOR'd with 0x33
//! - Address format: 12-digit BCD (reversed byte order)
//! - Data identifier: 4-byte DI code
//!
//! # Supported Operations
//!
//! - Read data (telemetry): Positive active energy, voltage, current, power, etc.
//! - Control/Adjustment: Not supported (read-only protocol)

use std::sync::{Arc, RwLock};
use std::time::Duration;

use aether_model::PointType;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_serial::{DataBits, Parity, SerialPortBuilderExt, SerialStream, StopBits};
use tracing::{debug, info, warn};

use crate::protocols::ChannelRuntime;
use crate::protocols::core::data::{DataBatch, DataPoint};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::logging::{
    ChannelLogConfig, ChannelLogHandler, ErrorContext, LogContext, LoggableProtocol,
};
use crate::protocols::core::metadata::{DriverMetadata, HasMetadata};
use crate::protocols::core::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, Diagnostics,
    PointFailure, PollResult, Protocol, ProtocolCapabilities, ProtocolClient, WriteResult,
};

// ============================================================================
// Constants
// ============================================================================

/// Frame start/end markers
const FRAME_START: u8 = 0x68;
const FRAME_END: u8 = 0x16;

/// Control codes
const CTRL_READ_DATA: u8 = 0x11; // Read data request
const CTRL_READ_DATA_RESP: u8 = 0x91; // Normal response
const CTRL_READ_DATA_ERR: u8 = 0xD1; // Error response

/// Data encoding offset (XOR value)
const DATA_OFFSET: u8 = 0x33;

/// Broadcast address (for single-meter scenarios)
const BROADCAST_ADDR: [u8; 6] = [0xAA; 6];

/// Default timeouts
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5000;
const DEFAULT_IO_TIMEOUT_MS: u64 = 3000;
const DEFAULT_RETRY_COUNT: u32 = 2;
const DEFAULT_FRAME_DELAY_MS: u64 = 200;

/// Maximum frame size
const MAX_FRAME_SIZE: usize = 256;

/// Default serial port settings (DL/T 645-2007 standard)
const DEFAULT_DATA_BITS: u8 = 8;
const DEFAULT_STOP_BITS: u8 = 1;
const DEFAULT_PARITY: &str = "even"; // DL/T 645 standard requires even parity

// ============================================================================
// Standard Data Points (hardcoded points)
// ============================================================================

/// Standard data points for DL/T 645-2007 protocol.
///
/// Fixed mapping: each DI code maps to a fixed point_id (1-11).
/// No configuration needed - all 11 standard points are always polled.
///
/// Format: (DI code, fixed point_id, name, data format)
const STANDARD_POINTS: &[(u32, u32, &str, DataFormat)] = &[
    // Energy
    (
        0x0001_0000,
        1,
        "Total positive active energy",
        DataFormat::Energy,
    ),
    (
        0x0002_0000,
        2,
        "Total reverse active energy",
        DataFormat::Energy,
    ),
    // Voltage
    (0x0201_0100, 3, "Phase A voltage", DataFormat::Voltage),
    (0x0201_0200, 4, "Phase B voltage", DataFormat::Voltage),
    (0x0201_0300, 5, "Phase C voltage", DataFormat::Voltage),
    // Current
    (0x0202_0100, 6, "Phase A current", DataFormat::Current),
    (0x0202_0200, 7, "Phase B current", DataFormat::Current),
    (0x0202_0300, 8, "Phase C current", DataFormat::Current),
    // Power
    (0x0203_0000, 9, "Total active power", DataFormat::Power),
    (0x0204_0000, 10, "Total reactive power", DataFormat::Power),
    // Power Factor
    (
        0x0206_0000,
        11,
        "Total power factor",
        DataFormat::PowerFactor,
    ),
];

/// Create failure entries for all standard points with the same error message.
fn fail_all_standard_points(msg: &str) -> Vec<PointFailure> {
    STANDARD_POINTS
        .iter()
        .map(|(_, point_id, _, _)| PointFailure::with_error(*point_id, msg.to_string()))
        .collect()
}

// ============================================================================
// Address Types
// ============================================================================

/// 12-digit BCD meter address.
///
/// The meter address is a 12-digit number encoded as 6 bytes in BCD format.
/// In the frame, the address is transmitted in reversed byte order (LSB first).
///
/// # Example
///
/// Address `123456789012` is encoded as bytes `[0x12, 0x90, 0x78, 0x56, 0x34, 0x12]`
/// and transmitted as `[0x12, 0x34, 0x56, 0x78, 0x90, 0x12]` (reversed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterAddress {
    /// 6 bytes in BCD format (high byte first, as logical representation)
    bytes: [u8; 6],
}

impl MeterAddress {
    /// Create a meter address from a 12-digit string.
    ///
    /// # Errors
    ///
    /// Returns error if the string is not exactly 12 digits.
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 12 {
            return Err(GatewayError::Config(format!(
                "Meter address must be 12 digits, got {} digits",
                s.len()
            )));
        }

        if !s.chars().all(|c| c.is_ascii_digit()) {
            return Err(GatewayError::Config(
                "Meter address must contain only digits".into(),
            ));
        }

        let mut bytes = [0u8; 6];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let high = chunk[0] - b'0';
            let low = chunk[1] - b'0';
            bytes[i] = (high << 4) | low;
        }

        Ok(Self { bytes })
    }

    /// Create a broadcast address (0xAAAAAAAAAAAAAA).
    #[must_use]
    pub fn broadcast() -> Self {
        Self {
            bytes: BROADCAST_ADDR,
        }
    }

    /// Convert to bytes for transmission (reversed byte order).
    #[must_use]
    pub fn to_wire_bytes(&self) -> [u8; 6] {
        let mut wire = [0u8; 6];
        for (i, &b) in self.bytes.iter().rev().enumerate() {
            wire[i] = b;
        }
        wire
    }

    /// Parse from wire bytes (reversed byte order).
    pub fn from_wire_bytes(wire: &[u8]) -> Result<Self> {
        if wire.len() != 6 {
            return Err(GatewayError::Protocol(format!(
                "Expected 6 bytes for meter address, got {}",
                wire.len()
            )));
        }
        let mut bytes = [0u8; 6];
        for (i, &b) in wire.iter().rev().enumerate() {
            bytes[i] = b;
        }
        Ok(Self { bytes })
    }
}

impl std::fmt::Display for MeterAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Format as 12-digit BCD string
        for &b in &self.bytes {
            write!(f, "{}{}", b >> 4, b & 0x0F)?;
        }
        Ok(())
    }
}

/// 4-byte Data Identifier (DI).
///
/// The data identifier specifies which data item to read from the meter.
/// Common DI codes are defined in the DL/T 645-2007 standard.
///
/// # Format
///
/// DI is specified as 4 bytes: DI3-DI2-DI1-DI0 (e.g., `00010000` for total
/// positive active energy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataIdentifier {
    /// DI bytes in order: [DI0, DI1, DI2, DI3]
    bytes: [u8; 4],
}

impl DataIdentifier {
    /// Create from a hex string (8 characters, e.g., "00010000").
    pub fn from_hex_str(s: &str) -> Result<Self> {
        if s.len() != 8 {
            return Err(GatewayError::Config(format!(
                "Data identifier must be 8 hex characters, got {}",
                s.len()
            )));
        }

        let mut bytes = [0u8; 4];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let high = hex_char_to_nibble(chunk[0])?;
            let low = hex_char_to_nibble(chunk[1])?;
            // DI3-DI2-DI1-DI0 in string, store as [DI0, DI1, DI2, DI3] for wire order
            bytes[3 - i] = (high << 4) | low;
        }

        Ok(Self { bytes })
    }

    /// Create from raw bytes [DI0, DI1, DI2, DI3].
    #[must_use]
    pub fn from_bytes(bytes: [u8; 4]) -> Self {
        Self { bytes }
    }

    /// Create from u32 value (e.g., 0x00010000 for total positive active energy).
    ///
    /// The u32 is interpreted as DI3-DI2-DI1-DI0 (big-endian) and stored as
    /// [DI0, DI1, DI2, DI3] for wire transmission.
    #[must_use]
    pub fn from_u32(value: u32) -> Self {
        Self {
            bytes: [
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
                ((value >> 24) & 0xFF) as u8,
            ],
        }
    }

    /// Get bytes for wire transmission (already in correct order).
    #[must_use]
    pub fn to_wire_bytes(&self) -> [u8; 4] {
        self.bytes
    }

    /// Get the DI as hex string (DI3-DI2-DI1-DI0).
    #[must_use]
    pub fn to_hex_string(&self) -> String {
        format!(
            "{:02X}{:02X}{:02X}{:02X}",
            self.bytes[3], self.bytes[2], self.bytes[1], self.bytes[0]
        )
    }
}

impl std::fmt::Display for DataIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex_string())
    }
}

/// Helper function to convert hex character to nibble value.
fn hex_char_to_nibble(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(GatewayError::Config(format!(
            "Invalid hex character: {}",
            char::from(c)
        ))),
    }
}

// Note: Dl645Address is defined in crate::protocols::core::point
// It only contains di_code (u32), as meter_address is a channel-level parameter.

// ============================================================================
// Frame Encoding/Decoding
// ============================================================================

/// Encode data bytes with +0x33 offset.
fn encode_data(data: &[u8]) -> Vec<u8> {
    data.iter().map(|b| b.wrapping_add(DATA_OFFSET)).collect()
}

/// Decode data bytes with -0x33 offset.
fn decode_data(data: &[u8]) -> Vec<u8> {
    data.iter().map(|b| b.wrapping_sub(DATA_OFFSET)).collect()
}

/// Calculate checksum (sum of all bytes from first 0x68 to before CS).
fn calculate_checksum(frame: &[u8]) -> u8 {
    frame.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// Build a read data request frame.
///
/// Frame format:
/// - 68H (1 byte)
/// - Address (6 bytes, reversed)
/// - 68H (1 byte)
/// - Control code (1 byte)
/// - Data length (1 byte)
/// - Data identifier (4 bytes, encoded)
/// - Checksum (1 byte)
/// - 16H (1 byte)
pub fn encode_read_request(meter_addr: &MeterAddress, data_id: &DataIdentifier) -> Vec<u8> {
    let mut frame = Vec::with_capacity(16);

    // Start marker
    frame.push(FRAME_START);

    // Meter address (6 bytes, reversed)
    frame.extend_from_slice(&meter_addr.to_wire_bytes());

    // Second start marker
    frame.push(FRAME_START);

    // Control code (read data)
    frame.push(CTRL_READ_DATA);

    // Data length (4 bytes for DI)
    frame.push(4);

    // Data identifier (encoded with +0x33)
    let di_bytes = data_id.to_wire_bytes();
    frame.extend_from_slice(&encode_data(&di_bytes));

    // Calculate checksum (from first 0x68 to before CS)
    let cs = calculate_checksum(&frame);
    frame.push(cs);

    // End marker
    frame.push(FRAME_END);

    frame
}

/// Response frame parsing result.
#[derive(Debug)]
pub struct Dl645Response {
    /// Meter address from response
    pub meter_addr: MeterAddress,
    /// Data identifier from response
    pub data_id: DataIdentifier,
    /// Decoded data bytes (without DI)
    pub data: Vec<u8>,
}

/// Error codes from DL/T 645-2007.
#[derive(Debug, Clone, Copy)]
pub enum Dl645Error {
    /// Rate/block number does not exist
    NoData = 0x01,
    /// Date/time error
    DateError = 0x02,
    /// No permission
    NoPermission = 0x04,
    /// Frame verification error
    FrameError = 0x08,
    /// Other error
    OtherError = 0x10,
}

impl std::fmt::Display for Dl645Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoData => write!(f, "Data does not exist"),
            Self::DateError => write!(f, "Date/time error"),
            Self::NoPermission => write!(f, "No permission"),
            Self::FrameError => write!(f, "Frame verification error"),
            Self::OtherError => write!(f, "Other error"),
        }
    }
}

/// Parse a response frame.
///
/// Returns the parsed response or an error description.
pub fn decode_response(frame: &[u8]) -> Result<Dl645Response> {
    // Minimum frame length: 68 + 6 + 68 + C + L + 4(DI) + CS + 16 = 14 bytes + data
    if frame.len() < 14 {
        return Err(GatewayError::Protocol(format!(
            "Frame too short: {} bytes",
            frame.len()
        )));
    }

    // Verify frame markers
    if frame[0] != FRAME_START || frame[7] != FRAME_START {
        return Err(GatewayError::Protocol("Invalid frame start markers".into()));
    }

    let frame_end_idx = frame.len() - 1;
    if frame[frame_end_idx] != FRAME_END {
        return Err(GatewayError::Protocol("Invalid frame end marker".into()));
    }

    // Verify checksum
    let cs_idx = frame_end_idx - 1;
    let expected_cs = calculate_checksum(&frame[..cs_idx]);
    if frame[cs_idx] != expected_cs {
        return Err(GatewayError::Protocol(format!(
            "Checksum mismatch: expected {:02X}, got {:02X}",
            expected_cs, frame[cs_idx]
        )));
    }

    // Parse meter address (bytes 1-6, reversed)
    let meter_addr = MeterAddress::from_wire_bytes(&frame[1..7])?;

    // Parse control code
    let ctrl = frame[8];
    let data_len = frame[9] as usize;

    // Verify data length
    let expected_len = 10 + data_len + 2; // header + data + CS + end
    if frame.len() != expected_len {
        return Err(GatewayError::Protocol(format!(
            "Frame length mismatch: expected {}, got {}",
            expected_len,
            frame.len()
        )));
    }

    // Check for error response
    if ctrl == CTRL_READ_DATA_ERR {
        // Error response: data[0] contains error code
        if data_len > 0 {
            let error_code = frame[10].wrapping_sub(DATA_OFFSET);
            let error = match error_code {
                0x01 => Dl645Error::NoData,
                0x02 => Dl645Error::DateError,
                0x04 => Dl645Error::NoPermission,
                0x08 => Dl645Error::FrameError,
                _ => Dl645Error::OtherError,
            };
            return Err(GatewayError::Protocol(format!("Meter error: {}", error)));
        }
        return Err(GatewayError::Protocol(
            "Meter returned error without code".into(),
        ));
    }

    // Verify normal response control code
    if ctrl != CTRL_READ_DATA_RESP {
        return Err(GatewayError::Protocol(format!(
            "Unexpected control code: {:02X}",
            ctrl
        )));
    }

    // Decode data (remove +0x33 encoding)
    let encoded_data = &frame[10..10 + data_len];
    let decoded_data = decode_data(encoded_data);

    // First 4 bytes are DI echo
    if decoded_data.len() < 4 {
        return Err(GatewayError::Protocol(
            "Response data too short for DI".into(),
        ));
    }

    let di_bytes: [u8; 4] = decoded_data[0..4]
        .try_into()
        .map_err(|_| GatewayError::Protocol("Failed to extract DI bytes".into()))?;
    let data_id = DataIdentifier::from_bytes(di_bytes);

    // Remaining bytes are actual data
    let data = decoded_data[4..].to_vec();

    Ok(Dl645Response {
        meter_addr,
        data_id,
        data,
    })
}

// ============================================================================
// Data Parsing
// ============================================================================

/// Data format specification for a data item.
#[derive(Debug, Clone, Copy)]
pub enum DataFormat {
    /// XXXXXX.XX (6 integer digits + 2 decimal digits)
    Energy,
    /// XXX.X (3 integer digits + 1 decimal digit)
    Voltage,
    /// XXX.XXX (3 integer digits + 3 decimal digits)
    Current,
    /// XX.XXXX (2 integer digits + 4 decimal digits, can be negative)
    Power,
    /// X.XXX (1 integer digit + 3 decimal digits, can be negative)
    PowerFactor,
    /// Raw bytes (no parsing)
    Raw,
}

impl DataFormat {
    /// Get the format based on data identifier.
    #[must_use]
    pub fn from_data_id(di: &DataIdentifier) -> Self {
        Self::from_di_code(u32::from_le_bytes(di.bytes))
    }

    /// Get the format based on DI code (u32).
    ///
    /// Uses the high-order bytes to determine data type:
    /// - 0x0001, 0x0002: Energy (kWh)
    /// - 0x0201: Voltage (V)
    /// - 0x0202: Current (A)
    /// - 0x0203, 0x0204: Power (kW/kVar)
    /// - 0x0206: Power Factor
    #[must_use]
    pub fn from_di_code(di_code: u32) -> Self {
        // Extract the high 16 bits for type classification
        let high_word = (di_code >> 16) & 0xFFFF;
        match high_word {
            0x0001 | 0x0002 => Self::Energy, // Energy
            0x0201 => Self::Voltage,         // Voltage
            0x0202 => Self::Current,         // Current
            0x0203..=0x0205 => Self::Power,  // Power
            0x0206 => Self::PowerFactor,     // Power factor
            0x0207 => Self::PowerFactor,     // Also power factor related
            _ => Self::Raw,
        }
    }
}

/// Parse BCD-encoded data bytes to f64 value.
///
/// BCD format: each nibble represents a digit (0-9).
/// Data is transmitted LSB first.
pub fn parse_bcd_data(data: &[u8], format: DataFormat) -> Result<f64> {
    match format {
        DataFormat::Energy => parse_energy(data),
        DataFormat::Voltage => parse_voltage(data),
        DataFormat::Current => parse_current(data),
        DataFormat::Power => parse_power(data),
        DataFormat::PowerFactor => parse_power_factor(data),
        DataFormat::Raw => {
            // Return first byte as value
            Ok(data.first().copied().unwrap_or(0) as f64)
        },
    }
}

/// Parse energy value: XXXXXX.XX kWh (4 bytes)
fn parse_energy(data: &[u8]) -> Result<f64> {
    if data.len() < 4 {
        return Err(GatewayError::Protocol(
            "Insufficient data for energy".into(),
        ));
    }

    // BCD: [XX.XX] [XX.00] [00.XX] [XX.00] (LSB first)
    // Bytes: data[0]=decimal part, data[1..3]=integer part
    let decimal = bcd_to_u32(data[0]);
    let integer = bcd_to_u32(data[1]) + bcd_to_u32(data[2]) * 100 + bcd_to_u32(data[3]) * 10000;

    Ok(f64::from(integer) + f64::from(decimal) / 100.0)
}

/// Parse voltage value: XXX.X V (2 bytes)
fn parse_voltage(data: &[u8]) -> Result<f64> {
    if data.len() < 2 {
        return Err(GatewayError::Protocol(
            "Insufficient data for voltage".into(),
        ));
    }

    let decimal = bcd_to_u32(data[0] & 0x0F);
    let integer =
        bcd_to_u32(data[0] >> 4) + bcd_to_u32(data[1] & 0x0F) * 10 + bcd_to_u32(data[1] >> 4) * 100;

    Ok(f64::from(integer) + f64::from(decimal) / 10.0)
}

/// Parse current value: XXX.XXX A (3 bytes)
fn parse_current(data: &[u8]) -> Result<f64> {
    if data.len() < 3 {
        return Err(GatewayError::Protocol(
            "Insufficient data for current".into(),
        ));
    }

    let d1 = bcd_to_u32(data[0] & 0x0F);
    let d2 = bcd_to_u32(data[0] >> 4);
    let d3 = bcd_to_u32(data[1] & 0x0F);
    let integer =
        bcd_to_u32(data[1] >> 4) + bcd_to_u32(data[2] & 0x0F) * 10 + bcd_to_u32(data[2] >> 4) * 100;

    let decimal = d3 * 100 + d2 * 10 + d1;
    Ok(f64::from(integer) + f64::from(decimal) / 1000.0)
}

/// Parse power value: XX.XXXX kW (3 bytes, can be negative)
fn parse_power(data: &[u8]) -> Result<f64> {
    if data.len() < 3 {
        return Err(GatewayError::Protocol("Insufficient data for power".into()));
    }

    // Sign is in the highest bit of the last byte
    let is_negative = (data[2] & 0x80) != 0;
    let last_byte = data[2] & 0x7F;

    let d1 = bcd_to_u32(data[0] & 0x0F);
    let d2 = bcd_to_u32(data[0] >> 4);
    let d3 = bcd_to_u32(data[1] & 0x0F);
    let d4 = bcd_to_u32(data[1] >> 4);
    let integer = bcd_to_u32(last_byte & 0x0F) + bcd_to_u32(last_byte >> 4) * 10;

    let decimal = d4 * 1000 + d3 * 100 + d2 * 10 + d1;
    let value = f64::from(integer) + f64::from(decimal) / 10000.0;

    Ok(if is_negative { -value } else { value })
}

/// Parse power factor: X.XXX (2 bytes, can be negative)
fn parse_power_factor(data: &[u8]) -> Result<f64> {
    if data.len() < 2 {
        return Err(GatewayError::Protocol(
            "Insufficient data for power factor".into(),
        ));
    }

    // Sign is in the highest bit
    let is_negative = (data[1] & 0x80) != 0;
    let last_byte = data[1] & 0x7F;

    let d1 = bcd_to_u32(data[0] & 0x0F);
    let d2 = bcd_to_u32(data[0] >> 4);
    let d3 = bcd_to_u32(last_byte & 0x0F);
    let integer = bcd_to_u32(last_byte >> 4);

    let decimal = d3 * 100 + d2 * 10 + d1;
    let value = f64::from(integer) + f64::from(decimal) / 1000.0;

    Ok(if is_negative { -value } else { value })
}

/// Convert a BCD byte to u32 (treats each nibble as a decimal digit).
fn bcd_to_u32(bcd: u8) -> u32 {
    u32::from((bcd >> 4) * 10 + (bcd & 0x0F))
}

// ============================================================================
// Transport Layer
// ============================================================================

/// Transport wrapper for DL/T 645 communication.
///
/// Supports both TCP (via gateway) and Serial (direct connection) transports.
pub enum Dl645Transport {
    /// TCP connection (typically through RS485-to-TCP gateway)
    Tcp(TcpStream),
    /// Serial connection (direct RS485/RS232)
    Serial(SerialStream),
}

impl Dl645Transport {
    /// Create a new TCP transport.
    pub async fn connect_tcp(host: &str, port: u16, timeout_ms: u64) -> Result<Self> {
        let addr = format!("{}:{}", host, port);
        let stream = timeout(Duration::from_millis(timeout_ms), TcpStream::connect(&addr))
            .await
            .map_err(|_| GatewayError::Connection(format!("Connection timeout to {}", addr)))?
            .map_err(|e| {
                GatewayError::Connection(format!("Failed to connect to {}: {}", addr, e))
            })?;

        // Disable Nagle's algorithm for low-latency
        stream.set_nodelay(true).ok();

        Ok(Self::Tcp(stream))
    }

    /// Create a new Serial transport.
    pub fn connect_serial(config: &Dl645ChannelConfig) -> Result<Self> {
        let device = config
            .device
            .as_ref()
            .ok_or_else(|| GatewayError::Config("Serial device not specified".into()))?;

        let parity = match config.parity.as_str() {
            "none" => Parity::None,
            "even" => Parity::Even,
            "odd" => Parity::Odd,
            _ => {
                return Err(GatewayError::Config(format!(
                    "Invalid parity: {}",
                    config.parity
                )));
            },
        };

        let data_bits = match config.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            _ => {
                return Err(GatewayError::Config(format!(
                    "Invalid data bits: {}",
                    config.data_bits
                )));
            },
        };

        let stop_bits = match config.stop_bits {
            1 => StopBits::One,
            2 => StopBits::Two,
            _ => {
                return Err(GatewayError::Config(format!(
                    "Invalid stop bits: {}",
                    config.stop_bits
                )));
            },
        };

        let port = tokio_serial::new(device, config.baud_rate)
            .parity(parity)
            .data_bits(data_bits)
            .stop_bits(stop_bits)
            .open_native_async()
            .map_err(|e| {
                GatewayError::Connection(format!("Failed to open serial port {}: {}", device, e))
            })?;

        Ok(Self::Serial(port))
    }

    /// Send a frame and receive the response (unified interface).
    pub async fn transact(&mut self, frame: &[u8], timeout_ms: u64) -> Result<Vec<u8>> {
        match self {
            Self::Tcp(stream) => {
                transact_stream(stream, frame, timeout_ms, "Connection closed by remote").await
            },
            Self::Serial(stream) => {
                transact_stream(stream, frame, timeout_ms, "Serial port closed").await
            },
        }
    }

    /// Close the transport.
    pub async fn close(&mut self) -> Result<()> {
        match self {
            Self::Tcp(stream) => {
                stream.shutdown().await.ok();
                Ok(())
            },
            Self::Serial(_) => {
                // SerialStream is closed when dropped
                Ok(())
            },
        }
    }
}

/// Send frame and receive response over any async stream.
async fn transact_stream<T: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut T,
    frame: &[u8],
    timeout_ms: u64,
    closed_msg: &str,
) -> Result<Vec<u8>> {
    timeout(Duration::from_millis(timeout_ms), stream.write_all(frame))
        .await
        .map_err(|_| GatewayError::WriteTimeout)?
        .map_err(GatewayError::Io)?;
    receive_frame(stream, timeout_ms, closed_msg).await
}

/// Common frame receiving logic for both TCP and Serial transports.
async fn receive_frame<T: AsyncReadExt + Unpin>(
    stream: &mut T,
    timeout_ms: u64,
    closed_msg: &str,
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; MAX_FRAME_SIZE];
    let mut total_read = 0;

    // Read until we have a complete frame
    loop {
        let n = timeout(
            Duration::from_millis(timeout_ms),
            stream.read(&mut buf[total_read..]),
        )
        .await
        .map_err(|_| GatewayError::ReadTimeout)?
        .map_err(GatewayError::Io)?;

        if n == 0 {
            return Err(GatewayError::Connection(closed_msg.into()));
        }

        total_read += n;

        // Check if we have a complete frame (minimum 14 bytes)
        if total_read >= 14 {
            // Find frame start marker (0x68)
            let start_idx = buf[..total_read].iter().position(|&b| b == FRAME_START);
            if let Some(start) = start_idx {
                // Check if we have the data length byte (offset 9 from start)
                if total_read > start + 9 {
                    let data_len = buf[start + 9] as usize;
                    let expected_len = 10 + data_len + 2; // header + data + CS + end

                    if total_read >= start + expected_len {
                        // We have a complete frame
                        buf.truncate(start + expected_len);
                        if start > 0 {
                            buf.drain(..start);
                        }
                        return Ok(buf);
                    }
                }
            }
        }

        // Prevent buffer overflow
        if total_read >= MAX_FRAME_SIZE {
            return Err(GatewayError::Protocol("Response too large".into()));
        }
    }
}

// ============================================================================
// Channel Configuration
// ============================================================================

/// Configuration for channel parameters (from TOML config).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dl645ChannelParamsConfig {
    /// Meter address (12 BCD digits, required)
    pub meter_address: String,

    // TCP mode
    /// TCP host address
    #[serde(default)]
    pub host: Option<String>,
    /// TCP port
    #[serde(default = "default_port")]
    pub port: u16,

    // Serial RTU mode
    /// Serial device path (e.g., "/dev/ttyUSB0" or "/dev/tty.usbserial-*")
    #[serde(default)]
    pub device: Option<String>,
    /// Serial baud rate (default: 2400 for older meters, 9600 for newer)
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,
    /// Data bits (default: 8)
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    /// Stop bits (default: 1)
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    /// Parity: "none", "even", "odd" (default: "even" per DL/T 645 standard)
    #[serde(default = "default_parity")]
    pub parity: String,

    // Common parameters
    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// I/O timeout in milliseconds
    #[serde(default = "default_io_timeout_ms")]
    pub timeout_ms: u64,
    /// Number of retries on failure
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
    /// Delay between frames in milliseconds
    #[serde(default = "default_frame_delay_ms")]
    pub frame_delay_ms: u64,
}

fn default_port() -> u16 {
    8899
}
fn default_baud_rate() -> u32 {
    2400
}
fn default_data_bits() -> u8 {
    DEFAULT_DATA_BITS
}
fn default_stop_bits() -> u8 {
    DEFAULT_STOP_BITS
}
fn default_parity() -> String {
    DEFAULT_PARITY.to_string()
}
fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_MS
}
fn default_io_timeout_ms() -> u64 {
    DEFAULT_IO_TIMEOUT_MS
}
fn default_retry_count() -> u32 {
    DEFAULT_RETRY_COUNT
}
fn default_frame_delay_ms() -> u64 {
    DEFAULT_FRAME_DELAY_MS
}

impl Dl645ChannelParamsConfig {
    /// Check if this is a TCP configuration.
    #[must_use]
    pub fn is_tcp(&self) -> bool {
        self.host.is_some()
    }

    /// Get the TCP address string.
    pub fn tcp_address(&self) -> Option<String> {
        self.host.as_ref().map(|h| format!("{}:{}", h, self.port))
    }

    /// Convert to internal channel config.
    pub fn to_channel_config(&self) -> Dl645ChannelConfig {
        Dl645ChannelConfig {
            meter_address: self.meter_address.clone(),
            host: self.host.clone(),
            port: self.port,
            device: self.device.clone(),
            baud_rate: self.baud_rate,
            data_bits: self.data_bits,
            stop_bits: self.stop_bits,
            parity: self.parity.clone(),
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
            io_timeout: Duration::from_millis(self.timeout_ms),
            retry_count: self.retry_count,
            frame_delay: Duration::from_millis(self.frame_delay_ms),
        }
    }
}

/// Internal channel configuration.
#[derive(Debug, Clone)]
pub struct Dl645ChannelConfig {
    /// Meter address (12 BCD digits)
    pub meter_address: String,
    /// TCP host
    pub host: Option<String>,
    /// TCP port
    pub port: u16,
    /// Serial device path
    pub device: Option<String>,
    /// Serial baud rate
    pub baud_rate: u32,
    /// Data bits
    pub data_bits: u8,
    /// Stop bits
    pub stop_bits: u8,
    /// Parity: "none", "even", "odd"
    pub parity: String,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// I/O timeout
    pub io_timeout: Duration,
    /// Retry count
    pub retry_count: u32,
    /// Frame delay
    pub frame_delay: Duration,
}

// ============================================================================
// Channel Implementation
// ============================================================================

/// DL/T 645 channel adapter.
///
/// This implements the `ProtocolClient` trait for DL/T 645-2007 protocol.
/// The channel supports reading data from intelligent electricity meters
/// but does not support control or adjustment commands.
pub struct Dl645Channel {
    /// Channel configuration
    config: Dl645ChannelConfig,
    /// Channel identifier
    channel_id: u32,
    /// Channel name
    name: String,
    /// Transport (TCP or Serial)
    transport: Option<Dl645Transport>,
    /// Connection state
    state: Arc<RwLock<ConnectionState>>,
    /// Diagnostics
    diagnostics: Arc<AtomicDiagnostics>,
    /// Logging context
    log_context: Arc<LogContext>,
}

impl Dl645Channel {
    /// Create a new DL/T 645 channel.
    pub fn new(config: Dl645ChannelConfig, channel_id: u32, name: String) -> Self {
        Self {
            config,
            channel_id,
            name,
            transport: None,
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            diagnostics: Arc::new(AtomicDiagnostics::default()),
            log_context: Arc::new(LogContext::new(channel_id)),
        }
    }

    /// Get current connection state.
    fn get_state(&self) -> ConnectionState {
        self.state
            .read()
            .map(|state| *state)
            .unwrap_or(ConnectionState::Error)
    }

    /// Set connection state.
    fn set_state(&self, state: ConnectionState) {
        if let Ok(mut current_state) = self.state.write() {
            *current_state = state;
        }
    }

    /// Get a description of the current endpoint for logging.
    fn endpoint_description(&self) -> String {
        if let Some(host) = &self.config.host {
            format!("{}:{}", host, self.config.port)
        } else if let Some(device) = &self.config.device {
            format!("{} @ {} baud", device, self.config.baud_rate)
        } else {
            "unknown".to_string()
        }
    }

    /// Create transport based on channel config (TCP or Serial).
    async fn create_transport(&self) -> Result<Dl645Transport> {
        if let Some(host) = &self.config.host {
            info!(
                "[DL645:{}] Connecting via TCP to {}:{}",
                self.channel_id, host, self.config.port
            );
            let timeout_ms = self.config.connect_timeout.as_millis() as u64;
            Dl645Transport::connect_tcp(host, self.config.port, timeout_ms).await
        } else if let Some(device) = &self.config.device {
            info!(
                "[DL645:{}] Connecting via Serial to {} @ {} baud",
                self.channel_id, device, self.config.baud_rate
            );
            Dl645Transport::connect_serial(&self.config)
        } else {
            Err(GatewayError::Config(
                "Either 'host' (TCP) or 'device' (Serial) must be specified".into(),
            ))
        }
    }

    /// Read a single data item by data identifier with retries.
    async fn read_single_di(
        &mut self,
        meter_addr: &MeterAddress,
        di: &DataIdentifier,
    ) -> Result<Vec<u8>> {
        let transport = self.transport.as_mut().ok_or(GatewayError::NotConnected)?;

        let request = encode_read_request(meter_addr, di);
        let timeout_ms = self.config.io_timeout.as_millis() as u64;

        let mut last_error = None;
        for attempt in 0..=self.config.retry_count {
            if attempt > 0 {
                debug!("Retry {} for DI {}", attempt, di);
                tokio::time::sleep(self.config.frame_delay).await;
            }

            match transport.transact(&request, timeout_ms).await {
                Ok(response) => match decode_response(&response) {
                    Ok(resp) => return Ok(resp.data),
                    Err(e) => last_error = Some(e),
                },
                Err(e) => last_error = Some(e),
            }
        }

        Err(last_error.unwrap_or_else(|| GatewayError::Protocol("Unknown error".into())))
    }

    /// Read all standard data points sequentially (DL/T 645 is request-response).
    async fn read_standard_points(
        &mut self,
        meter_addr: &MeterAddress,
    ) -> (DataBatch, Vec<PointFailure>, u64, u64) {
        let mut batch = DataBatch::default();
        let mut failures = Vec::new();
        let mut read_count = 0u64;
        let mut error_count = 0u64;

        for (di_code, point_id, _name, format) in STANDARD_POINTS {
            if read_count > 0 || error_count > 0 {
                tokio::time::sleep(self.config.frame_delay).await;
            }

            let di = DataIdentifier::from_u32(*di_code);

            match self.read_single_di(meter_addr, &di).await {
                Ok(raw_data) => match parse_bcd_data(&raw_data, *format) {
                    Ok(value) => {
                        batch.add(DataPoint::new(*point_id, PointType::Telemetry, value));
                        read_count += 1;
                        debug!(
                            "[DL645:{}] Read DI {:08X} -> point {} = {}",
                            self.channel_id, di_code, point_id, value
                        );
                    },
                    Err(e) => {
                        error_count += 1;
                        warn!(
                            "[DL645:{}] Failed to parse DI {:08X} (point {}): {}",
                            self.channel_id, di_code, point_id, e
                        );
                        failures.push(PointFailure::with_error(*point_id, e.to_string()));
                    },
                },
                Err(e) => {
                    error_count += 1;
                    warn!(
                        "[DL645:{}] Failed to read DI {:08X} (point {}): {}",
                        self.channel_id, di_code, point_id, e
                    );
                    failures.push(PointFailure::with_error(*point_id, e.to_string()));
                },
            }
        }

        (batch, failures, read_count, error_count)
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl HasMetadata for Dl645Channel {
    fn metadata() -> DriverMetadata {
        use serde_json::{Map, Value};
        let mut config = Map::new();
        config.insert(
            "meter_address".to_string(),
            Value::String("123456789012".to_string()),
        );
        config.insert(
            "host".to_string(),
            Value::String("192.168.1.200".to_string()),
        );
        config.insert("port".to_string(), Value::Number(8899.into()));
        config.insert("timeout_ms".to_string(), Value::Number(3000.into()));
        config.insert("retry_count".to_string(), Value::Number(2.into()));
        config.insert("frame_delay_ms".to_string(), Value::Number(200.into()));

        DriverMetadata {
            name: "dl645",
            display_name: "DL/T 645-2007",
            description: "DL/T 645-2007 smart meter protocol (read-only, uses standard data points)",
            is_recommended: false,
            example_config: Value::Object(config),
            parameters: vec![],
        }
    }
}

impl ProtocolCapabilities for Dl645Channel {
    fn name(&self) -> &'static str {
        "dl645"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::Polling]
    }

    fn version(&self) -> &'static str {
        "2007"
    }
}

impl LoggableProtocol for Dl645Channel {
    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        // LogContext is immutable after creation, use Arc::make_mut if needed
        if let Some(ctx) = Arc::get_mut(&mut self.log_context) {
            ctx.set_handler(handler);
        }
    }

    fn set_log_config(&mut self, config: ChannelLogConfig) {
        if let Some(ctx) = Arc::get_mut(&mut self.log_context) {
            ctx.set_config(config);
        }
    }

    fn log_config(&self) -> &ChannelLogConfig {
        self.log_context.config()
    }
}

impl Protocol for Dl645Channel {
    fn connection_state(&self) -> ConnectionState {
        self.get_state()
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "dl645".to_string(),
            connection_state: self.get_state(),
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: None,
            extra: serde_json::Value::Null,
        })
    }
}

impl ProtocolClient for Dl645Channel {
    async fn connect(&mut self) -> Result<()> {
        let start_time = std::time::Instant::now();
        let old_state = self.get_state();
        self.set_state(ConnectionState::Connecting);
        self.log_context
            .log_state_changed(old_state, ConnectionState::Connecting)
            .await;

        // Validate meter address first
        if let Err(e) = MeterAddress::parse(&self.config.meter_address) {
            self.set_state(ConnectionState::Error);
            let err_msg = format!("Invalid meter address: {}", e);
            self.log_context
                .log_error(&err_msg, ErrorContext::Connection)
                .await;
            return Err(GatewayError::Config(err_msg));
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;

        match self.create_transport().await {
            Ok(transport) => {
                self.transport = Some(transport);
                self.set_state(ConnectionState::Connected);

                let endpoint = self.endpoint_description();
                info!(
                    "[DL645:{}] Connected to {} (meter: {})",
                    self.channel_id, endpoint, self.config.meter_address
                );
                self.log_context.log_connected(&endpoint, duration_ms).await;
                self.log_context
                    .log_state_changed(ConnectionState::Connecting, ConnectionState::Connected)
                    .await;
                Ok(())
            },
            Err(e) => {
                self.set_state(ConnectionState::Error);
                self.log_context
                    .log_error(&e.to_string(), ErrorContext::Connection)
                    .await;
                self.log_context
                    .log_state_changed(ConnectionState::Connecting, ConnectionState::Error)
                    .await;
                Err(e)
            },
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        let old_state = self.get_state();

        if let Some(mut transport) = self.transport.take() {
            let _ = transport.close().await;
        }

        self.set_state(ConnectionState::Disconnected);

        self.log_context.log_disconnected(None).await;
        self.log_context
            .log_state_changed(old_state, ConnectionState::Disconnected)
            .await;

        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        let start_time = std::time::Instant::now();

        // Check connection
        if self.transport.is_none() {
            self.log_context
                .log_error("Not connected", ErrorContext::Polling)
                .await;
            return PollResult::failed(fail_all_standard_points("Not connected"));
        }

        // Parse meter address from config
        let meter_addr = match MeterAddress::parse(&self.config.meter_address) {
            Ok(addr) => addr,
            Err(e) => {
                let err_msg = format!("Invalid meter address: {}", e);
                self.log_context
                    .log_error(&err_msg, ErrorContext::Polling)
                    .await;
                return PollResult::failed(fail_all_standard_points(&err_msg));
            },
        };

        let (batch, failures, read_count, error_count) =
            self.read_standard_points(&meter_addr).await;

        self.diagnostics.add_read(read_count);
        self.diagnostics.add_error(error_count);

        let duration_ms = start_time.elapsed().as_millis() as u64;
        debug!(
            "[DL645:{}] poll_once: read {} points, {} failures in {}ms",
            self.channel_id,
            batch.len(),
            failures.len(),
            duration_ms
        );

        self.log_context
            .log_poll_cycle(
                batch.len(),
                duration_ms,
                read_count as usize,
                error_count as usize,
            )
            .await;

        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    async fn write_control(&mut self, _commands: &[ControlCommand]) -> Result<WriteResult> {
        // DL/T 645 is read-only for data collection
        warn!("DL/T 645 protocol does not support control commands");
        Ok(WriteResult {
            success_count: 0,
            failures: vec![(0, "Control commands not supported by DL/T 645".into())],
        })
    }

    async fn write_adjustment(
        &mut self,
        _adjustments: &[AdjustmentCommand],
    ) -> Result<WriteResult> {
        // DL/T 645 is read-only for data collection
        warn!("DL/T 645 protocol does not support adjustment commands");
        Ok(WriteResult {
            success_count: 0,
            failures: vec![(0, "Adjustment commands not supported by DL/T 645".into())],
        })
    }
}

#[async_trait]
impl ChannelRuntime for Dl645Channel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "dl645"
    }

    fn is_event_driven(&self) -> bool {
        false // DL/T 645 is polling-based
    }

    async fn connect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::connect(self).await
    }

    async fn disconnect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::disconnect(self).await
    }

    async fn poll_once(&mut self) -> PollResult {
        <Self as ProtocolClient>::poll_once(self).await
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let cmds: Vec<_> = commands
            .iter()
            .map(|(id, value)| ControlCommand::latching(*id, *value != 0.0))
            .collect();
        let result = <Self as ProtocolClient>::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, value)| AdjustmentCommand::new(*id, *value))
            .collect();
        let result = <Self as ProtocolClient>::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    fn subscribe(&self) -> Option<crate::protocols::core::DataEventReceiver> {
        None // DL/T 645 is polling-only
    }

    async fn start_events(&mut self) -> Result<()> {
        Ok(()) // No-op for polling channel
    }

    async fn stop_events(&mut self) -> Result<()> {
        Ok(()) // No-op for polling channel
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        <Self as Protocol>::diagnostics(self).await
    }

    fn connection_state(&self) -> ConnectionState {
        <Self as Protocol>::connection_state(self)
    }

    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        <Self as LoggableProtocol>::set_log_handler(self, handler);
    }

    fn set_log_config(&mut self, config: ChannelLogConfig) {
        <Self as LoggableProtocol>::set_log_config(self, config);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::protocols::core::point::Dl645Address;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connection_state_query_is_safe_on_a_tokio_runtime_thread() {
        let params = Dl645ChannelParamsConfig {
            meter_address: "123456789012".to_string(),
            host: Some("127.0.0.1".to_string()),
            port: 8899,
            device: None,
            baud_rate: 2400,
            data_bits: 8,
            stop_bits: 1,
            parity: "even".to_string(),
            connect_timeout_ms: 5000,
            timeout_ms: 3000,
            retry_count: 2,
            frame_delay_ms: 200,
        };
        let channel = Dl645Channel::new(params.to_channel_config(), 645, "meter".to_string());

        assert_eq!(
            <Dl645Channel as Protocol>::connection_state(&channel),
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn test_meter_address_from_str() {
        let addr = MeterAddress::parse("123456789012").unwrap();
        assert_eq!(addr.to_string(), "123456789012");
    }

    #[test]
    fn test_meter_address_wire_format() {
        let addr = MeterAddress::parse("123456789012").unwrap();
        let wire = addr.to_wire_bytes();
        // Reversed: 12 90 78 56 34 12 -> wire order
        assert_eq!(wire[0], 0x12);
        assert_eq!(wire[5], 0x12);
    }

    #[test]
    fn test_meter_address_invalid() {
        assert!(MeterAddress::parse("12345").is_err());
        assert!(MeterAddress::parse("12345678901a").is_err());
    }

    #[test]
    fn test_data_identifier_from_hex() {
        let di = DataIdentifier::from_hex_str("00010000").unwrap();
        assert_eq!(di.to_hex_string(), "00010000");
    }

    #[test]
    fn test_data_identifier_wire_format() {
        let di = DataIdentifier::from_hex_str("00010000").unwrap();
        let wire = di.to_wire_bytes();
        // DI3-DI2-DI1-DI0 = 00-01-00-00, wire order is [DI0, DI1, DI2, DI3]
        assert_eq!(wire, [0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn test_dl645_address_parse() {
        // With 0x prefix
        let addr = Dl645Address::parse("0x02010100").unwrap();
        assert_eq!(addr.di_code, 0x02010100);
        assert_eq!(addr.to_hex_string(), "02010100");

        // Without prefix
        let addr = Dl645Address::parse("00010000").unwrap();
        assert_eq!(addr.di_code, 0x00010000);
    }

    #[test]
    fn test_dl645_address_invalid() {
        // Too short
        assert!(Dl645Address::parse("0201").is_err());
        // Too long
        assert!(Dl645Address::parse("0201010000").is_err());
        // Invalid hex
        assert!(Dl645Address::parse("0201GHIJ").is_err());
    }

    #[test]
    fn test_encode_decode_data() {
        let original = vec![0x00, 0x01, 0x02, 0xFF];
        let encoded = encode_data(&original);
        let decoded = decode_data(&encoded);
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_checksum_calculation() {
        let frame = vec![0x68, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x68, 0x11, 0x04];
        let cs = calculate_checksum(&frame);
        // Sum all bytes mod 256
        let expected: u8 = frame.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(cs, expected);
    }

    #[test]
    fn test_encode_read_request() {
        let meter_addr = MeterAddress::parse("123456789012").unwrap();
        let data_id = DataIdentifier::from_hex_str("00010000").unwrap();

        let frame = encode_read_request(&meter_addr, &data_id);

        // Verify frame structure
        assert_eq!(frame[0], FRAME_START);
        assert_eq!(frame[7], FRAME_START);
        assert_eq!(frame[8], CTRL_READ_DATA);
        assert_eq!(frame[9], 4); // Data length
        assert_eq!(*frame.last().unwrap(), FRAME_END);

        // Verify frame length: 1 + 6 + 1 + 1 + 1 + 4 + 1 + 1 = 16
        assert_eq!(frame.len(), 16);
    }

    #[test]
    fn test_decode_response_valid() {
        // Construct a valid response frame
        // Meter address: 123456789012 (reversed)
        // DI: 00010000
        // Data: some energy value

        let mut frame = vec![
            0x68, // Start
            0x12, 0x90, 0x78, 0x56, 0x34, 0x12, // Address (reversed)
            0x68, // Start 2
            0x91, // Control (normal response)
            0x08, // Data length (4 DI + 4 data)
        ];

        // DI + Data encoded with +0x33
        let di_data: [u8; 8] = [0x00, 0x00, 0x01, 0x00, 0x12, 0x34, 0x56, 0x78];
        let encoded: Vec<u8> = di_data.iter().map(|&b| b.wrapping_add(0x33)).collect();
        frame.extend_from_slice(&encoded);

        // Checksum
        let cs = calculate_checksum(&frame);
        frame.push(cs);

        // End
        frame.push(0x16);

        let response = decode_response(&frame).unwrap();
        assert_eq!(response.meter_addr.to_string(), "123456789012");
        assert_eq!(response.data_id.to_hex_string(), "00010000");
        assert_eq!(response.data, vec![0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn test_decode_response_checksum_error() {
        let frame = vec![
            0x68, 0x12, 0x90, 0x78, 0x56, 0x34, 0x12, 0x68, 0x91, 0x04, 0x33, 0x33, 0x34,
            0x33, // Encoded DI
            0x00, // Wrong checksum
            0x16,
        ];

        assert!(decode_response(&frame).is_err());
    }

    #[test]
    fn test_bcd_to_u32() {
        assert_eq!(bcd_to_u32(0x12), 12);
        assert_eq!(bcd_to_u32(0x99), 99);
        assert_eq!(bcd_to_u32(0x00), 0);
    }

    #[test]
    fn test_parse_energy() {
        // 123456.78 kWh -> [0x78, 0x56, 0x34, 0x12] (BCD, LSB first)
        let data = vec![0x78, 0x56, 0x34, 0x12];
        let value = parse_energy(&data).unwrap();
        assert!((value - 123456.78).abs() < 0.001);
    }

    #[test]
    fn test_parse_voltage() {
        // 220.5 V -> [0x05, 0x22] (XXX.X, BCD)
        let data = vec![0x05, 0x22];
        let value = parse_voltage(&data).unwrap();
        assert!((value - 220.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_current() {
        // 10.123 A -> [0x23, 0x11, 0x00] (XXX.XXX, BCD)
        let data = vec![0x23, 0x11, 0x00];
        let value = parse_current(&data).unwrap();
        assert!((value - 1.123).abs() < 0.001);
    }

    #[test]
    fn test_data_format_detection() {
        let energy_di = DataIdentifier::from_hex_str("00010000").unwrap();
        assert!(matches!(
            DataFormat::from_data_id(&energy_di),
            DataFormat::Energy
        ));

        let voltage_di = DataIdentifier::from_hex_str("02010100").unwrap();
        assert!(matches!(
            DataFormat::from_data_id(&voltage_di),
            DataFormat::Voltage
        ));

        let current_di = DataIdentifier::from_hex_str("02020100").unwrap();
        assert!(matches!(
            DataFormat::from_data_id(&current_di),
            DataFormat::Current
        ));
    }

    #[test]
    fn test_channel_config_tcp() {
        let params = Dl645ChannelParamsConfig {
            meter_address: "123456789012".to_string(),
            host: Some("192.168.1.100".to_string()),
            port: 8899,
            device: None,
            baud_rate: 2400,
            data_bits: 8,
            stop_bits: 1,
            parity: "even".to_string(),
            connect_timeout_ms: 5000,
            timeout_ms: 3000,
            retry_count: 2,
            frame_delay_ms: 200,
        };

        assert!(params.is_tcp());
        assert_eq!(params.tcp_address(), Some("192.168.1.100:8899".to_string()));

        let config = params.to_channel_config();
        assert_eq!(config.meter_address, "123456789012");
        assert_eq!(config.connect_timeout, Duration::from_millis(5000));
        assert_eq!(config.io_timeout, Duration::from_millis(3000));
    }

    #[test]
    fn test_channel_config_serial() {
        let params = Dl645ChannelParamsConfig {
            meter_address: "123456789012".to_string(),
            host: None,
            port: 8899,
            device: Some("/dev/ttyUSB0".to_string()),
            baud_rate: 2400,
            data_bits: 8,
            stop_bits: 1,
            parity: "even".to_string(),
            connect_timeout_ms: 5000,
            timeout_ms: 3000,
            retry_count: 2,
            frame_delay_ms: 200,
        };

        assert!(!params.is_tcp());
        assert_eq!(params.tcp_address(), None);

        let config = params.to_channel_config();
        assert_eq!(config.meter_address, "123456789012");
        assert_eq!(config.device, Some("/dev/ttyUSB0".to_string()));
        assert_eq!(config.baud_rate, 2400);
        assert_eq!(config.parity, "even");
    }

    #[test]
    fn test_data_identifier_from_u32() {
        // Test total positive active energy (0x00010000)
        let di = DataIdentifier::from_u32(0x0001_0000);
        assert_eq!(di.to_hex_string(), "00010000");

        // Test phase A voltage (0x02010100)
        let di = DataIdentifier::from_u32(0x0201_0100);
        assert_eq!(di.to_hex_string(), "02010100");

        // Test wire bytes
        let di = DataIdentifier::from_u32(0x0001_0000);
        let wire = di.to_wire_bytes();
        assert_eq!(wire, [0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn test_standard_points_count() {
        // Verify we have 11 standard points
        assert_eq!(STANDARD_POINTS.len(), 11);

        // Verify all DI codes are unique
        let di_codes: Vec<u32> = STANDARD_POINTS.iter().map(|(di, _, _, _)| *di).collect();
        let unique_count = di_codes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert_eq!(di_codes.len(), unique_count);
    }
}
