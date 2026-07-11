//! Zigbee frame codec for TCP gateway communication.
//!
//! Provides frame encoding/decoding for different Zigbee gateway types.
//! Currently implements Raw mode (direct ZCL frames); ZNP and EZSP are stubs.

use bytes::{Buf, BufMut, BytesMut};

use crate::protocols::core::error::{GatewayError, Result};

// Raw frame constants
const RAW_FRAME_HEADER_LEN: usize = 3; // LEN(2) + TYPE(1)
const RAW_FRAME_CHECKSUM_LEN: usize = 1;
const RAW_FRAME_OVERHEAD: usize = RAW_FRAME_HEADER_LEN + RAW_FRAME_CHECKSUM_LEN;

// Raw frame type bytes
const FRAME_TYPE_ATTR_REPORT: u8 = 0x01;
const FRAME_TYPE_DEVICE_ANNOUNCE: u8 = 0x02;
const FRAME_TYPE_CMD_RESPONSE: u8 = 0x03;

// Minimum payload sizes (excluding type byte)
const ATTR_REPORT_MIN_PAYLOAD: usize = 13; // ieee(8) + ep(1) + cluster(2) + attr(2)
const DEVICE_ANNOUNCE_PAYLOAD: usize = 10; // ieee(8) + short(2)
const CMD_RESPONSE_PAYLOAD: usize = 2; // seq(1) + status(1)

/// ZCL frame types received from the gateway.
#[derive(Debug, Clone)]
pub enum ZigbeeFrame {
    /// Attribute report from a device
    AttributeReport(AttributeReport),
    /// Device announce (new device joined)
    DeviceAnnounce(DeviceAnnounce),
    /// Command response/confirmation
    CommandResponse { seq: u8, status: u8 },
    /// Unknown/unparsed frame
    Unknown(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct AttributeReport {
    pub ieee_addr: u64,
    pub endpoint: u8,
    pub cluster_id: u16,
    pub attribute_id: u16,
    pub value: ZclValue,
}

#[derive(Debug, Clone)]
pub struct DeviceAnnounce {
    pub ieee_addr: u64,
    pub short_addr: u16,
}

/// ZCL attribute value types.
#[derive(Debug, Clone, PartialEq)]
pub enum ZclValue {
    Bool(bool),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Float(f32),
    Double(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl ZclValue {
    /// Convert to f64 for DataPoint storage.
    pub fn to_f64(&self) -> f64 {
        match self {
            Self::Bool(v) => {
                if *v {
                    1.0
                } else {
                    0.0
                }
            },
            Self::UInt8(v) => f64::from(*v),
            Self::UInt16(v) => f64::from(*v),
            Self::UInt32(v) => f64::from(*v),
            Self::Int8(v) => f64::from(*v),
            Self::Int16(v) => f64::from(*v),
            Self::Int32(v) => f64::from(*v),
            Self::Float(v) => f64::from(*v),
            Self::Double(v) => *v,
            Self::String(_) | Self::Bytes(_) => 0.0,
        }
    }

    /// ZCL type ID byte for encoding.
    #[cfg(test)]
    fn type_id(&self) -> u8 {
        match self {
            Self::Bool(_) => 0x10,
            Self::UInt8(_) => 0x20,
            Self::UInt16(_) => 0x21,
            Self::UInt32(_) => 0x23,
            Self::Int8(_) => 0x28,
            Self::Int16(_) => 0x29,
            Self::Int32(_) => 0x2B,
            Self::Float(_) => 0x39,
            Self::Double(_) => 0x3A,
            Self::String(_) => 0x42,
            Self::Bytes(_) => 0x41,
        }
    }
}

/// Frame codec trait for different gateway types.
pub trait FrameCodec: Send + Sync {
    /// Try to decode a frame from the buffer.
    /// Returns None if not enough data, Some(frame) if decoded, consuming bytes.
    fn decode(&self, buf: &mut BytesMut) -> Result<Option<ZigbeeFrame>>;

    /// Encode a ZCL command for sending to a device.
    fn encode_command(
        &self,
        ieee_addr: u64,
        endpoint: u8,
        cluster_id: u16,
        command_id: u8,
        payload: &[u8],
    ) -> Vec<u8>;
}

/// Raw frame codec -- simplest protocol, direct ZCL-like frames.
///
/// Frame format: `[LEN:2][TYPE:1][PAYLOAD:LEN-1][CHECKSUM:1]`
///
/// - LEN: payload length including TYPE byte (big-endian u16)
/// - TYPE: frame type identifier
/// - PAYLOAD: frame-type-specific data
/// - CHECKSUM: XOR of all bytes from TYPE through end of PAYLOAD
pub struct RawFrameCodec;

impl RawFrameCodec {
    /// Compute XOR checksum over a byte slice.
    fn checksum(data: &[u8]) -> u8 {
        data.iter().fold(0u8, |acc, b| acc ^ b)
    }

    /// Parse a ZCL value from buffer given a type ID.
    fn parse_zcl_value(buf: &mut BytesMut) -> Result<ZclValue> {
        if buf.is_empty() {
            return Err(GatewayError::InvalidData(
                "Empty ZCL value payload".to_string(),
            ));
        }

        let type_id = buf.get_u8();

        match type_id {
            // Bool
            0x10 => {
                if buf.is_empty() {
                    return Err(GatewayError::InvalidData(
                        "Truncated bool value".to_string(),
                    ));
                }
                Ok(ZclValue::Bool(buf.get_u8() != 0))
            },
            // UInt8
            0x20 => {
                if buf.is_empty() {
                    return Err(GatewayError::InvalidData(
                        "Truncated uint8 value".to_string(),
                    ));
                }
                Ok(ZclValue::UInt8(buf.get_u8()))
            },
            // UInt16
            0x21 => {
                if buf.remaining() < 2 {
                    return Err(GatewayError::InvalidData(
                        "Truncated uint16 value".to_string(),
                    ));
                }
                Ok(ZclValue::UInt16(buf.get_u16_le()))
            },
            // UInt32
            0x23 => {
                if buf.remaining() < 4 {
                    return Err(GatewayError::InvalidData(
                        "Truncated uint32 value".to_string(),
                    ));
                }
                Ok(ZclValue::UInt32(buf.get_u32_le()))
            },
            // Int8
            0x28 => {
                if buf.is_empty() {
                    return Err(GatewayError::InvalidData(
                        "Truncated int8 value".to_string(),
                    ));
                }
                Ok(ZclValue::Int8(buf.get_u8() as i8))
            },
            // Int16
            0x29 => {
                if buf.remaining() < 2 {
                    return Err(GatewayError::InvalidData(
                        "Truncated int16 value".to_string(),
                    ));
                }
                Ok(ZclValue::Int16(buf.get_u16_le() as i16))
            },
            // Int32
            0x2B => {
                if buf.remaining() < 4 {
                    return Err(GatewayError::InvalidData(
                        "Truncated int32 value".to_string(),
                    ));
                }
                Ok(ZclValue::Int32(buf.get_u32_le() as i32))
            },
            // Float (single precision)
            0x39 => {
                if buf.remaining() < 4 {
                    return Err(GatewayError::InvalidData(
                        "Truncated float value".to_string(),
                    ));
                }
                Ok(ZclValue::Float(f32::from_bits(buf.get_u32_le())))
            },
            // Double (double precision)
            0x3A => {
                if buf.remaining() < 8 {
                    return Err(GatewayError::InvalidData(
                        "Truncated double value".to_string(),
                    ));
                }
                Ok(ZclValue::Double(f64::from_bits(buf.get_u64_le())))
            },
            // Octet string
            0x41 => {
                if buf.is_empty() {
                    return Err(GatewayError::InvalidData(
                        "Truncated octet string length".to_string(),
                    ));
                }
                let len = buf.get_u8() as usize;
                if buf.remaining() < len {
                    return Err(GatewayError::InvalidData(format!(
                        "Truncated octet string: need {len}, have {}",
                        buf.remaining()
                    )));
                }
                let data = buf.split_to(len).to_vec();
                Ok(ZclValue::Bytes(data))
            },
            // Character string
            0x42 => {
                if buf.is_empty() {
                    return Err(GatewayError::InvalidData(
                        "Truncated char string length".to_string(),
                    ));
                }
                let len = buf.get_u8() as usize;
                if buf.remaining() < len {
                    return Err(GatewayError::InvalidData(format!(
                        "Truncated char string: need {len}, have {}",
                        buf.remaining()
                    )));
                }
                let data = buf.split_to(len).to_vec();
                let s = String::from_utf8(data)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                Ok(ZclValue::String(s))
            },
            _ => Err(GatewayError::InvalidData(format!(
                "Unsupported ZCL type: 0x{type_id:02X}"
            ))),
        }
    }

    /// Encode a ZCL value to bytes (type_id + value).
    #[cfg(test)]
    fn encode_zcl_value(value: &ZclValue, out: &mut Vec<u8>) {
        out.push(value.type_id());
        match value {
            ZclValue::Bool(v) => out.push(u8::from(*v)),
            ZclValue::UInt8(v) => out.push(*v),
            ZclValue::UInt16(v) => out.extend_from_slice(&v.to_le_bytes()),
            ZclValue::UInt32(v) => out.extend_from_slice(&v.to_le_bytes()),
            ZclValue::Int8(v) => out.push(*v as u8),
            ZclValue::Int16(v) => out.extend_from_slice(&v.to_le_bytes()),
            ZclValue::Int32(v) => out.extend_from_slice(&v.to_le_bytes()),
            ZclValue::Float(v) => out.extend_from_slice(&v.to_bits().to_le_bytes()),
            ZclValue::Double(v) => out.extend_from_slice(&v.to_bits().to_le_bytes()),
            ZclValue::String(s) => {
                let bytes = s.as_bytes();
                out.push(bytes.len() as u8);
                out.extend_from_slice(bytes);
            },
            ZclValue::Bytes(b) => {
                out.push(b.len() as u8);
                out.extend_from_slice(b);
            },
        }
    }
}

impl FrameCodec for RawFrameCodec {
    fn decode(&self, buf: &mut BytesMut) -> Result<Option<ZigbeeFrame>> {
        // Need at least header (LEN:2 + TYPE:1) + checksum(1)
        if buf.remaining() < RAW_FRAME_OVERHEAD {
            return Ok(None);
        }

        // Peek at length without consuming
        let len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        if len == 0 {
            // Invalid frame, skip the 2-byte length
            buf.advance(2);
            return Err(GatewayError::InvalidData(
                "Zero-length Zigbee frame".to_string(),
            ));
        }

        // Total frame size: LEN(2) + payload(len) + checksum(1)
        let total = 2 + len + 1;
        if buf.remaining() < total {
            return Ok(None); // Need more data
        }

        // Extract the full frame
        let frame_bytes = buf.split_to(total);

        // Verify checksum: XOR of bytes from TYPE through end of PAYLOAD
        let payload_region = &frame_bytes[2..2 + len];
        let expected_checksum = frame_bytes[2 + len];
        let actual_checksum = Self::checksum(payload_region);

        if actual_checksum != expected_checksum {
            return Err(GatewayError::InvalidData(format!(
                "Checksum mismatch: expected 0x{expected_checksum:02X}, got 0x{actual_checksum:02X}"
            )));
        }

        // Parse by frame type
        let frame_type = frame_bytes[2];
        let mut payload = BytesMut::from(&frame_bytes[3..2 + len]);

        match frame_type {
            FRAME_TYPE_ATTR_REPORT => {
                if payload.remaining() < ATTR_REPORT_MIN_PAYLOAD {
                    return Err(GatewayError::InvalidData(
                        "Attribute report payload too short".to_string(),
                    ));
                }

                let ieee_addr = payload.get_u64_le();
                let endpoint = payload.get_u8();
                let cluster_id = payload.get_u16_le();
                let attribute_id = payload.get_u16_le();

                let value = Self::parse_zcl_value(&mut payload)?;

                Ok(Some(ZigbeeFrame::AttributeReport(AttributeReport {
                    ieee_addr,
                    endpoint,
                    cluster_id,
                    attribute_id,
                    value,
                })))
            },
            FRAME_TYPE_DEVICE_ANNOUNCE => {
                if payload.remaining() < DEVICE_ANNOUNCE_PAYLOAD {
                    return Err(GatewayError::InvalidData(
                        "Device announce payload too short".to_string(),
                    ));
                }

                let ieee_addr = payload.get_u64_le();
                let short_addr = payload.get_u16_le();

                Ok(Some(ZigbeeFrame::DeviceAnnounce(DeviceAnnounce {
                    ieee_addr,
                    short_addr,
                })))
            },
            FRAME_TYPE_CMD_RESPONSE => {
                if payload.remaining() < CMD_RESPONSE_PAYLOAD {
                    return Err(GatewayError::InvalidData(
                        "Command response payload too short".to_string(),
                    ));
                }

                let seq = payload.get_u8();
                let status = payload.get_u8();

                Ok(Some(ZigbeeFrame::CommandResponse { seq, status }))
            },
            _ => {
                // Unknown frame type — preserve raw payload for debugging
                Ok(Some(ZigbeeFrame::Unknown(payload.to_vec())))
            },
        }
    }

    fn encode_command(
        &self,
        ieee_addr: u64,
        endpoint: u8,
        cluster_id: u16,
        command_id: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        // Build the inner payload: TYPE + ieee(8) + ep(1) + cluster(2) + cmd(1) + payload
        let inner_len = 1 + 8 + 1 + 2 + 1 + payload.len();
        let mut out = Vec::with_capacity(2 + inner_len + 1);

        // Length (big-endian u16) — includes TYPE byte through end of payload
        out.put_u16(inner_len as u16);

        // Frame type: use 0x10 for command frames
        out.push(0x10);

        // IEEE address (little-endian)
        out.extend_from_slice(&ieee_addr.to_le_bytes());

        // Endpoint
        out.push(endpoint);

        // Cluster ID (little-endian)
        out.extend_from_slice(&cluster_id.to_le_bytes());

        // Command ID
        out.push(command_id);

        // Command payload
        out.extend_from_slice(payload);

        // Checksum: XOR of TYPE through end of payload
        let checksum = Self::checksum(&out[2..]);
        out.push(checksum);

        out
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    /// Helper: build a raw frame from type + payload bytes.
    fn build_raw_frame(frame_type: u8, payload: &[u8]) -> Vec<u8> {
        let len = 1 + payload.len(); // TYPE + PAYLOAD
        let mut frame = Vec::with_capacity(2 + len + 1);
        frame.put_u16(len as u16);
        frame.push(frame_type);
        frame.extend_from_slice(payload);

        // Checksum: XOR of TYPE through end of payload
        let checksum = RawFrameCodec::checksum(&frame[2..]);
        frame.push(checksum);

        frame
    }

    #[test]
    fn test_zcl_value_to_f64() {
        assert_eq!(ZclValue::Bool(true).to_f64(), 1.0);
        assert_eq!(ZclValue::Bool(false).to_f64(), 0.0);
        assert_eq!(ZclValue::UInt8(42).to_f64(), 42.0);
        assert_eq!(ZclValue::UInt16(1000).to_f64(), 1000.0);
        assert_eq!(ZclValue::UInt32(100_000).to_f64(), 100_000.0);
        assert_eq!(ZclValue::Int8(-10).to_f64(), -10.0);
        assert_eq!(ZclValue::Int16(-500).to_f64(), -500.0);
        assert_eq!(ZclValue::Int32(-100_000).to_f64(), -100_000.0);
        assert!((ZclValue::Float(3.5).to_f64() - 3.5).abs() < 0.001);
        assert_eq!(ZclValue::Double(2.5).to_f64(), 2.5);
        assert_eq!(ZclValue::String("hello".to_string()).to_f64(), 0.0);
        assert_eq!(ZclValue::Bytes(vec![1, 2, 3]).to_f64(), 0.0);
    }

    #[test]
    fn test_decode_attribute_report_uint16() {
        let codec = RawFrameCodec;

        // Build payload: ieee(8) + ep(1) + cluster(2) + attr(2) + type(1) + value(2)
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x00124B0018ED1234u64.to_le_bytes()); // ieee
        payload.push(1); // endpoint
        payload.extend_from_slice(&0x0402u16.to_le_bytes()); // cluster (temperature)
        payload.extend_from_slice(&0x0000u16.to_le_bytes()); // attribute
        payload.push(0x21); // ZCL type: uint16
        payload.extend_from_slice(&2500u16.to_le_bytes()); // value: 25.00 C * 100

        let raw = build_raw_frame(FRAME_TYPE_ATTR_REPORT, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                assert_eq!(report.ieee_addr, 0x00124B0018ED1234);
                assert_eq!(report.endpoint, 1);
                assert_eq!(report.cluster_id, 0x0402);
                assert_eq!(report.attribute_id, 0x0000);
                assert_eq!(report.value, ZclValue::UInt16(2500));
            },
            _ => panic!("Expected AttributeReport"),
        }

        // Buffer should be fully consumed
        assert!(buf.is_empty());
    }

    #[test]
    fn test_decode_attribute_report_bool() {
        let codec = RawFrameCodec;

        let mut payload = Vec::new();
        payload.extend_from_slice(&0xAABBCCDDEEFF0011u64.to_le_bytes());
        payload.push(2); // endpoint
        payload.extend_from_slice(&0x0006u16.to_le_bytes()); // On/Off cluster
        payload.extend_from_slice(&0x0000u16.to_le_bytes()); // OnOff attribute
        payload.push(0x10); // ZCL type: bool
        payload.push(1); // value: true

        let raw = build_raw_frame(FRAME_TYPE_ATTR_REPORT, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                assert_eq!(report.value, ZclValue::Bool(true));
            },
            _ => panic!("Expected AttributeReport"),
        }
    }

    #[test]
    fn test_decode_device_announce() {
        let codec = RawFrameCodec;

        let mut payload = Vec::new();
        payload.extend_from_slice(&0x00124B0018ED5678u64.to_le_bytes());
        payload.extend_from_slice(&0x1234u16.to_le_bytes());

        let raw = build_raw_frame(FRAME_TYPE_DEVICE_ANNOUNCE, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::DeviceAnnounce(announce) => {
                assert_eq!(announce.ieee_addr, 0x00124B0018ED5678);
                assert_eq!(announce.short_addr, 0x1234);
            },
            _ => panic!("Expected DeviceAnnounce"),
        }
    }

    #[test]
    fn test_decode_command_response() {
        let codec = RawFrameCodec;

        let payload = vec![0x42, 0x00]; // seq=0x42, status=0x00 (success)
        let raw = build_raw_frame(FRAME_TYPE_CMD_RESPONSE, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::CommandResponse { seq, status } => {
                assert_eq!(seq, 0x42);
                assert_eq!(status, 0x00);
            },
            _ => panic!("Expected CommandResponse"),
        }
    }

    #[test]
    fn test_decode_unknown_frame_type() {
        let codec = RawFrameCodec;

        let payload = vec![0xDE, 0xAD];
        let raw = build_raw_frame(0xFF, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::Unknown(data) => {
                assert_eq!(data, vec![0xDE, 0xAD]);
            },
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_decode_incomplete_data() {
        let codec = RawFrameCodec;

        // Only 2 bytes — not enough for header
        let mut buf = BytesMut::from(&[0x00, 0x05][..]);
        assert!(codec.decode(&mut buf).unwrap().is_none());

        // Header says 5 bytes payload + 1 checksum, but only 3 bytes total in buffer
        let mut buf = BytesMut::from(&[0x00, 0x05, 0x01][..]);
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_decode_checksum_mismatch() {
        let codec = RawFrameCodec;

        let payload = vec![0x42, 0x00];
        let mut raw = build_raw_frame(FRAME_TYPE_CMD_RESPONSE, &payload);
        // Corrupt the checksum
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;

        let mut buf = BytesMut::from(raw.as_slice());
        assert!(codec.decode(&mut buf).is_err());
    }

    #[test]
    fn test_decode_zero_length() {
        let codec = RawFrameCodec;

        let mut buf = BytesMut::from(&[0x00, 0x00, 0x00, 0x00][..]);
        assert!(codec.decode(&mut buf).is_err());
    }

    #[test]
    fn test_encode_command() {
        let codec = RawFrameCodec;

        let encoded = codec.encode_command(
            0x00124B0018ED1234,
            1,
            0x0006, // On/Off cluster
            0x01,   // On command
            &[],    // No payload
        );

        // Verify structure: LEN(2) + TYPE(1) + ieee(8) + ep(1) + cluster(2) + cmd(1) + checksum(1)
        assert_eq!(encoded.len(), 2 + 1 + 8 + 1 + 2 + 1 + 1);

        // Verify length field
        let len = u16::from_be_bytes([encoded[0], encoded[1]]) as usize;
        assert_eq!(len, 1 + 8 + 1 + 2 + 1); // TYPE + payload

        // Verify checksum
        let payload_region = &encoded[2..encoded.len() - 1];
        let expected_checksum = RawFrameCodec::checksum(payload_region);
        assert_eq!(encoded[encoded.len() - 1], expected_checksum);
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let codec = RawFrameCodec;

        // Build an attribute report, encode it as if it came from the wire
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x00124B0018ED1234u64.to_le_bytes());
        payload.push(1);
        payload.extend_from_slice(&0x0402u16.to_le_bytes());
        payload.extend_from_slice(&0x0000u16.to_le_bytes());
        // Float value
        payload.push(0x39); // ZCL float type
        payload.extend_from_slice(&25.5f32.to_bits().to_le_bytes());

        let raw = build_raw_frame(FRAME_TYPE_ATTR_REPORT, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                assert_eq!(report.value, ZclValue::Float(25.5));
                assert!((report.value.to_f64() - 25.5).abs() < f64::EPSILON);
            },
            _ => panic!("Expected AttributeReport"),
        }
    }

    #[test]
    fn test_decode_multiple_frames() {
        let codec = RawFrameCodec;

        // Two frames back-to-back
        let payload1 = vec![0x01, 0x00]; // seq=1, status=0
        let payload2 = vec![0x02, 0x01]; // seq=2, status=1
        let raw1 = build_raw_frame(FRAME_TYPE_CMD_RESPONSE, &payload1);
        let raw2 = build_raw_frame(FRAME_TYPE_CMD_RESPONSE, &payload2);

        let mut buf = BytesMut::new();
        buf.extend_from_slice(&raw1);
        buf.extend_from_slice(&raw2);

        // Decode first frame
        let frame1 = codec.decode(&mut buf).unwrap().unwrap();
        match frame1 {
            ZigbeeFrame::CommandResponse { seq, status } => {
                assert_eq!(seq, 0x01);
                assert_eq!(status, 0x00);
            },
            _ => panic!("Expected CommandResponse"),
        }

        // Decode second frame
        let frame2 = codec.decode(&mut buf).unwrap().unwrap();
        match frame2 {
            ZigbeeFrame::CommandResponse { seq, status } => {
                assert_eq!(seq, 0x02);
                assert_eq!(status, 0x01);
            },
            _ => panic!("Expected CommandResponse"),
        }

        // Buffer should be empty
        assert!(buf.is_empty());
    }

    #[test]
    fn test_zcl_value_type_id() {
        assert_eq!(ZclValue::Bool(true).type_id(), 0x10);
        assert_eq!(ZclValue::UInt8(0).type_id(), 0x20);
        assert_eq!(ZclValue::UInt16(0).type_id(), 0x21);
        assert_eq!(ZclValue::UInt32(0).type_id(), 0x23);
        assert_eq!(ZclValue::Int8(0).type_id(), 0x28);
        assert_eq!(ZclValue::Int16(0).type_id(), 0x29);
        assert_eq!(ZclValue::Int32(0).type_id(), 0x2B);
        assert_eq!(ZclValue::Float(0.0).type_id(), 0x39);
        assert_eq!(ZclValue::Double(0.0).type_id(), 0x3A);
        assert_eq!(ZclValue::String(String::new()).type_id(), 0x42);
        assert_eq!(ZclValue::Bytes(Vec::new()).type_id(), 0x41);
    }

    #[test]
    fn test_decode_attribute_report_int8() {
        let codec = RawFrameCodec;

        let mut payload = Vec::new();
        payload.extend_from_slice(&0x00124B0018ED1234u64.to_le_bytes());
        payload.push(1);
        payload.extend_from_slice(&0x0402u16.to_le_bytes());
        payload.extend_from_slice(&0x0001u16.to_le_bytes());
        payload.push(0x28); // ZCL type: int8
        payload.push((-25i8) as u8);

        let raw = build_raw_frame(FRAME_TYPE_ATTR_REPORT, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                assert_eq!(report.value, ZclValue::Int8(-25));
            },
            _ => panic!("Expected AttributeReport"),
        }
    }

    #[test]
    fn test_decode_attribute_report_float() {
        let codec = RawFrameCodec;

        let mut payload = Vec::new();
        payload.extend_from_slice(&0x00124B0018ED1234u64.to_le_bytes());
        payload.push(1);
        payload.extend_from_slice(&0x0402u16.to_le_bytes());
        payload.extend_from_slice(&0x0000u16.to_le_bytes());
        payload.push(0x39); // ZCL type: float
        payload.extend_from_slice(&25.5f32.to_bits().to_le_bytes());

        let raw = build_raw_frame(FRAME_TYPE_ATTR_REPORT, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                assert_eq!(report.value, ZclValue::Float(25.5));
            },
            _ => panic!("Expected AttributeReport"),
        }
    }

    #[test]
    fn test_checksum_xor_properties() {
        // XOR checksum: empty slice → 0
        assert_eq!(RawFrameCodec::checksum(&[]), 0);
        // Single byte → itself
        assert_eq!(RawFrameCodec::checksum(&[0x42]), 0x42);
        // Self-inverse: x ^ x == 0
        assert_eq!(RawFrameCodec::checksum(&[0xAB, 0xAB]), 0);
        // Known value
        assert_eq!(RawFrameCodec::checksum(&[0x01, 0x02, 0x04]), 0x07);
    }

    #[test]
    fn test_encode_decode_large_payload() {
        let codec = RawFrameCodec;

        // Build an attribute report with a large string value
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x00124B0018ED1234u64.to_le_bytes());
        payload.push(1);
        payload.extend_from_slice(&0x0402u16.to_le_bytes());
        payload.extend_from_slice(&0x0000u16.to_le_bytes());
        payload.push(0x42); // ZCL type: string
        let long_string = "A".repeat(200);
        payload.push(long_string.len() as u8);
        payload.extend_from_slice(long_string.as_bytes());

        let raw = build_raw_frame(FRAME_TYPE_ATTR_REPORT, &payload);
        let mut buf = BytesMut::from(raw.as_slice());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        match frame {
            ZigbeeFrame::AttributeReport(report) => {
                assert_eq!(report.value, ZclValue::String(long_string));
            },
            _ => panic!("Expected AttributeReport"),
        }
    }

    #[test]
    fn test_encode_zcl_value_roundtrip() {
        let values = vec![
            ZclValue::Bool(true),
            ZclValue::UInt8(42),
            ZclValue::UInt16(1000),
            ZclValue::UInt32(100_000),
            ZclValue::Int8(-10),
            ZclValue::Int16(-500),
            ZclValue::Int32(-100_000),
            ZclValue::Float(3.5),
            ZclValue::Double(2.5),
            ZclValue::String("hello".to_string()),
            ZclValue::Bytes(vec![1, 2, 3]),
        ];

        for original in values {
            let mut encoded = Vec::new();
            RawFrameCodec::encode_zcl_value(&original, &mut encoded);

            let mut buf = BytesMut::from(encoded.as_slice());
            let decoded = RawFrameCodec::parse_zcl_value(&mut buf).unwrap();

            assert_eq!(original, decoded, "Roundtrip failed for {:?}", original);
        }
    }
}
