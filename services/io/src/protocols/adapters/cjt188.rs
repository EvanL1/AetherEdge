//! CJ/T 188 household meter protocol adapter.
//!
//! Supports the request/response data-read path over a serial bus or a TCP
//! serial gateway. Point mappings select a two-byte data identifier and a
//! typed slice within that identifier's response payload. Valve and meter
//! configuration commands are intentionally not exposed by this read-only
//! adapter.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_core::PointType;
use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_serial::{DataBits, Parity, SerialPortBuilderExt, SerialStream, StopBits};

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler, LogContext};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::TransformConfig;
use crate::protocols::core::traits::{ConnectionState, Diagnostics, PointFailure, PollResult};
use crate::protocols::runtime::ChannelRuntime;

const FRAME_START: u8 = 0x68;
const FRAME_END: u8 = 0x16;
const CONTROL_READ_DATA: u8 = 0x01;
const CONTROL_READ_DATA_RESPONSE: u8 = 0x81;
const MAX_DATA_LENGTH: usize = 255;
const MAX_WAKE_BYTES: usize = 16;
const DEFAULT_PORT: u16 = 8_899;
const DEFAULT_BAUD_RATE: u32 = 2_400;
const DEFAULT_TIMEOUT_MS: u64 = 2_000;

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_baud_rate() -> u32 {
    DEFAULT_BAUD_RATE
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_parity() -> String {
    "even".to_owned()
}

/// CJ/T 188 channel parameters. Exactly one of `host` and `device` is required.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cjt188ParamsConfig {
    #[serde(default)]
    pub(crate) host: Option<String>,
    #[serde(default = "default_port")]
    pub(crate) port: u16,
    #[serde(default)]
    pub(crate) device: Option<String>,
    #[serde(default = "default_baud_rate")]
    pub(crate) baud_rate: u32,
    #[serde(default = "default_parity")]
    pub(crate) parity: String,
    #[serde(default = "default_timeout_ms")]
    pub(crate) timeout_ms: u64,
    pub(crate) meter_type: u8,
    pub(crate) meter_address: String,
}

impl Cjt188ParamsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        let host = self
            .host
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let device = self
            .device
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if host == device {
            return Err(GatewayError::Config(
                "CJ/T 188 requires exactly one nonblank host or serial device".to_owned(),
            ));
        }
        if host && self.port == 0 {
            return Err(GatewayError::Config(
                "CJ/T 188 TCP port must be greater than zero".to_owned(),
            ));
        }
        if self.baud_rate == 0 {
            return Err(GatewayError::Config(
                "CJ/T 188 baud_rate must be greater than zero".to_owned(),
            ));
        }
        if !matches!(
            self.parity.to_ascii_lowercase().as_str(),
            "none" | "even" | "odd"
        ) {
            return Err(GatewayError::Config(
                "CJ/T 188 parity must be none, even, or odd".to_owned(),
            ));
        }
        if self.timeout_ms == 0 || self.timeout_ms > aether_config::io::MAX_CHANNEL_TIMING_MS {
            return Err(GatewayError::Config(format!(
                "CJ/T 188 timeout_ms must be between 1 and {}",
                aether_config::io::MAX_CHANNEL_TIMING_MS
            )));
        }
        parse_meter_address(&self.meter_address)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cjt188DataType {
    BcdLe,
    U8,
    U16Le,
    U32Le,
    I16Le,
    I32Le,
    F32Le,
}

impl Cjt188DataType {
    fn fixed_byte_len(self) -> Option<usize> {
        match self {
            Self::BcdLe => None,
            Self::U8 => Some(1),
            Self::U16Le | Self::I16Le => Some(2),
            Self::U32Le | Self::I32Le | Self::F32Le => Some(4),
        }
    }
}

/// Per-point mapping into one CJ/T 188 data response.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cjt188PointMapping {
    pub(crate) data_id: u16,
    #[serde(default)]
    pub(crate) byte_offset: usize,
    pub(crate) data_type: Cjt188DataType,
    #[serde(default)]
    pub(crate) byte_length: Option<usize>,
}

impl Cjt188PointMapping {
    fn byte_len(&self) -> Result<usize> {
        match (self.data_type.fixed_byte_len(), self.byte_length) {
            (Some(length), None) => Ok(length),
            (Some(_), Some(_)) => Err(GatewayError::Config(
                "CJ/T 188 byte_length is valid only for bcd_le mappings".to_owned(),
            )),
            (None, Some(length @ 1..=8)) => Ok(length),
            (None, _) => Err(GatewayError::Config(
                "CJ/T 188 bcd_le mappings require byte_length between 1 and 8".to_owned(),
            )),
        }
    }
}

pub(crate) fn parse_point_mapping(
    mapping: &serde_json::Value,
    point_type: PointType,
) -> Result<Cjt188PointMapping> {
    if !matches!(point_type, PointType::Telemetry | PointType::Signal) {
        return Err(GatewayError::Config(
            "CJ/T 188 adapter is read-only and supports telemetry and signal mappings".to_owned(),
        ));
    }
    let mapping: Cjt188PointMapping = serde_json::from_value(mapping.clone())
        .map_err(|error| GatewayError::Config(format!("invalid CJ/T 188 mapping: {error}")))?;
    let length = mapping.byte_len()?;
    mapping
        .byte_offset
        .checked_add(length)
        .filter(|end| *end <= MAX_DATA_LENGTH)
        .ok_or_else(|| {
            GatewayError::Config("CJ/T 188 mapping exceeds maximum payload length".to_owned())
        })?;
    Ok(mapping)
}

#[derive(Debug, Clone)]
pub(crate) struct Cjt188PointConfig {
    pub(crate) point_id: u32,
    pub(crate) point_type: PointType,
    pub(crate) mapping: Cjt188PointMapping,
    pub(crate) scale: f64,
    pub(crate) offset: f64,
    pub(crate) reverse: bool,
}

enum Cjt188Transport {
    Tcp(TcpStream),
    Serial(SerialStream),
}

impl Cjt188Transport {
    async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Tcp(stream) => write_all(stream, bytes).await,
            Self::Serial(stream) => write_all(stream, bytes).await,
        }
    }

    async fn read_frame(&mut self, duration: Duration) -> Result<Vec<u8>> {
        match self {
            Self::Tcp(stream) => read_frame(stream, duration).await,
            Self::Serial(stream) => read_frame(stream, duration).await,
        }
    }
}

/// CJ/T 188 polling channel.
pub struct Cjt188Channel {
    channel_id: u32,
    params: Cjt188ParamsConfig,
    meter_address: [u8; 7],
    points: Vec<Cjt188PointConfig>,
    transport: Option<Cjt188Transport>,
    sequence: u8,
    state: ConnectionState,
    diagnostics: AtomicDiagnostics,
    log_context: LogContext,
}

impl Cjt188Channel {
    pub(crate) fn new(
        params: Cjt188ParamsConfig,
        channel_id: u32,
        points: Vec<Cjt188PointConfig>,
    ) -> Result<Self> {
        params.validate()?;
        let meter_address = parse_meter_address(&params.meter_address)?;
        Ok(Self {
            channel_id,
            params,
            meter_address,
            points,
            transport: None,
            sequence: 0,
            state: ConnectionState::Disconnected,
            diagnostics: AtomicDiagnostics::new(),
            log_context: LogContext::new(channel_id),
        })
    }

    async fn read_identifier(&mut self, data_id: u16) -> Result<Vec<u8>> {
        self.sequence = self.sequence.wrapping_add(1);
        let request = encode_read_request(
            self.params.meter_type,
            self.meter_address,
            data_id,
            self.sequence,
        )?;
        let transport = self.transport.as_mut().ok_or(GatewayError::NotConnected)?;
        let duration = Duration::from_millis(self.params.timeout_ms);
        timeout(duration, transport.write_all(&request))
            .await
            .map_err(|_| GatewayError::WriteTimeout)??;
        let response = transport.read_frame(duration).await?;
        decode_read_response(
            &response,
            self.params.meter_type,
            self.meter_address,
            data_id,
            self.sequence,
        )
    }
}

impl HasMetadata for Cjt188Channel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "cjt188",
            display_name: "CJ/T 188",
            description: "CJ/T 188 household water, gas, and heat meter reader over serial or TCP",
            is_recommended: true,
            example_config: serde_json::json!({
                "device": "/dev/ttyUSB0",
                "baud_rate": DEFAULT_BAUD_RATE,
                "parity": "even",
                "timeout_ms": DEFAULT_TIMEOUT_MS,
                "meter_type": 16,
                "meter_address": "00000000000001"
            }),
            parameters: vec![
                ParameterMetadata::optional(
                    "host",
                    "TCP host",
                    "Serial gateway hostname or IP; mutually exclusive with device",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "port",
                    "TCP port",
                    "Serial gateway TCP port",
                    ParameterType::Integer,
                    serde_json::json!(DEFAULT_PORT),
                )
                .with_integer_range(1, u64::from(u16::MAX)),
                ParameterMetadata::optional(
                    "device",
                    "Serial device",
                    "Serial device path; mutually exclusive with host",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "baud_rate",
                    "Baud rate",
                    "Serial baud rate",
                    ParameterType::Integer,
                    serde_json::json!(DEFAULT_BAUD_RATE),
                )
                .with_integer_range(1, u64::from(u32::MAX)),
                ParameterMetadata::optional(
                    "parity",
                    "Parity",
                    "Serial parity: none, even, or odd",
                    ParameterType::String,
                    serde_json::json!("even"),
                ),
                ParameterMetadata::optional(
                    "timeout_ms",
                    "Timeout",
                    "Frame timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(DEFAULT_TIMEOUT_MS),
                )
                .with_integer_range(1, aether_config::io::MAX_CHANNEL_TIMING_MS),
                ParameterMetadata::required(
                    "meter_type",
                    "Meter type",
                    "CJ/T 188 meter type byte",
                    ParameterType::Integer,
                )
                .with_integer_range(0, u64::from(u8::MAX)),
                ParameterMetadata::required(
                    "meter_address",
                    "Meter address",
                    "Fourteen-digit BCD meter address",
                    ParameterType::String,
                )
                .with_min_length(14),
            ],
        }
    }
}

#[async_trait]
impl ChannelRuntime for Cjt188Channel {
    async fn connect(&mut self) -> Result<()> {
        self.state = ConnectionState::Connecting;
        let transport = if let Some(host) = self.params.host.as_deref() {
            let stream = timeout(
                Duration::from_millis(self.params.timeout_ms),
                TcpStream::connect((host, self.params.port)),
            )
            .await
            .map_err(|_| GatewayError::ConnectionTimeout(self.params.timeout_ms))?
            .map_err(|error| GatewayError::Connection(error.to_string()))?;
            Cjt188Transport::Tcp(stream)
        } else {
            let device = self.params.device.as_deref().ok_or_else(|| {
                GatewayError::Config("CJ/T 188 serial device is missing".to_owned())
            })?;
            let parity = match self.params.parity.to_ascii_lowercase().as_str() {
                "none" => Parity::None,
                "odd" => Parity::Odd,
                _ => Parity::Even,
            };
            let serial = tokio_serial::new(device, self.params.baud_rate)
                .data_bits(DataBits::Eight)
                .parity(parity)
                .stop_bits(StopBits::One)
                .open_native_async()
                .map_err(|error| GatewayError::Connection(error.to_string()))?;
            Cjt188Transport::Serial(serial)
        };
        self.transport = Some(transport);
        self.state = ConnectionState::Connected;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.transport = None;
        self.state = ConnectionState::Disconnected;
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        if self.transport.is_none() {
            return PollResult::failed(
                self.points
                    .iter()
                    .map(|point| PointFailure::new(point.point_id, "Not connected"))
                    .collect(),
            );
        }

        let groups = self.points.iter().cloned().fold(
            BTreeMap::<u16, Vec<Cjt188PointConfig>>::new(),
            |mut groups, point| {
                groups.entry(point.mapping.data_id).or_default().push(point);
                groups
            },
        );
        let mut batch = DataBatch::with_capacity(self.points.len());
        let mut failures = Vec::new();
        for (data_id, points) in groups {
            match self.read_identifier(data_id).await {
                Ok(payload) => {
                    for point in points {
                        match decode_point(&payload, &point) {
                            Ok(value) => {
                                batch.add(DataPoint::new(point.point_id, point.point_type, value));
                                self.diagnostics.inc_read();
                            },
                            Err(error) => {
                                self.diagnostics.record_error(error.to_string());
                                failures.push(PointFailure::with_error(
                                    point.point_id,
                                    error.to_string(),
                                ));
                            },
                        }
                    }
                },
                Err(error) => {
                    self.diagnostics.record_error(error.to_string());
                    failures.extend(
                        points.into_iter().map(|point| {
                            PointFailure::with_error(point.point_id, error.to_string())
                        }),
                    );
                },
            }
        }
        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "cjt188".to_owned(),
            connection_state: self.state,
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: serde_json::json!({
                "channel_id": self.channel_id,
                "meter_type": self.params.meter_type,
                "meter_address": self.params.meter_address,
                "point_count": self.points.len()
            }),
        })
    }

    fn connection_state(&self) -> ConnectionState {
        self.state
    }

    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        self.log_context.set_handler(handler);
    }

    fn set_log_config(&mut self, config: ChannelLogConfig) {
        self.log_context.set_config(config);
    }
}

async fn write_all(stream: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    stream.write_all(bytes).await.map_err(GatewayError::Io)?;
    stream.flush().await.map_err(GatewayError::Io)
}

async fn read_frame(stream: &mut (impl AsyncRead + Unpin), duration: Duration) -> Result<Vec<u8>> {
    timeout(duration, read_frame_inner(stream))
        .await
        .map_err(|_| GatewayError::ReadTimeout)?
}

async fn read_frame_inner(stream: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    let mut byte = [0_u8; 1];
    let mut wake_count = 0;
    loop {
        stream
            .read_exact(&mut byte)
            .await
            .map_err(GatewayError::Io)?;
        if byte[0] == FRAME_START {
            break;
        }
        if byte[0] != 0xfe || wake_count >= MAX_WAKE_BYTES {
            return Err(GatewayError::InvalidResponse(
                "CJ/T 188 frame does not start with 0x68".to_owned(),
            ));
        }
        wake_count += 1;
    }

    let mut header_rest = [0_u8; 10];
    stream
        .read_exact(&mut header_rest)
        .await
        .map_err(GatewayError::Io)?;
    let data_length = usize::from(header_rest[9]);
    let mut tail = vec![0_u8; data_length + 2];
    stream
        .read_exact(&mut tail)
        .await
        .map_err(GatewayError::Io)?;
    let mut frame = Vec::with_capacity(11 + tail.len());
    frame.push(FRAME_START);
    frame.extend_from_slice(&header_rest);
    frame.extend_from_slice(&tail);
    Ok(frame)
}

fn parse_meter_address(value: &str) -> Result<[u8; 7]> {
    if value.len() != 14 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(GatewayError::Config(
            "CJ/T 188 meter_address must contain exactly fourteen decimal digits".to_owned(),
        ));
    }
    let bytes = value.as_bytes();
    let mut address = [0_u8; 7];
    for (index, target) in address.iter_mut().enumerate() {
        let source = 6 - index;
        let high = bytes[source * 2] - b'0';
        let low = bytes[source * 2 + 1] - b'0';
        *target = (high << 4) | low;
    }
    Ok(address)
}

fn encode_read_request(
    meter_type: u8,
    address: [u8; 7],
    data_id: u16,
    sequence: u8,
) -> Result<Vec<u8>> {
    encode_frame(
        meter_type,
        address,
        CONTROL_READ_DATA,
        &[(data_id & 0xff) as u8, (data_id >> 8) as u8, sequence],
    )
}

fn encode_frame(meter_type: u8, address: [u8; 7], control: u8, data: &[u8]) -> Result<Vec<u8>> {
    let data_length = u8::try_from(data.len())
        .map_err(|_| GatewayError::InvalidData("CJ/T 188 data exceeds 255 bytes".to_owned()))?;
    let mut frame = Vec::with_capacity(13 + data.len());
    frame.push(FRAME_START);
    frame.push(meter_type);
    frame.extend_from_slice(&address);
    frame.push(control);
    frame.push(data_length);
    frame.extend_from_slice(data);
    let checksum = frame.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    frame.push(checksum);
    frame.push(FRAME_END);
    Ok(frame)
}

fn decode_read_response(
    frame: &[u8],
    expected_type: u8,
    expected_address: [u8; 7],
    expected_data_id: u16,
    expected_sequence: u8,
) -> Result<Vec<u8>> {
    if frame.len() < 16 || frame[0] != FRAME_START || frame.last() != Some(&FRAME_END) {
        return Err(GatewayError::InvalidResponse(
            "invalid CJ/T 188 frame markers or length".to_owned(),
        ));
    }
    let data_length = usize::from(frame[10]);
    if frame.len() != 13 + data_length {
        return Err(GatewayError::InvalidResponse(
            "CJ/T 188 frame length mismatch".to_owned(),
        ));
    }
    let checksum_index = frame.len() - 2;
    let checksum = frame[..checksum_index]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    if checksum != frame[checksum_index] {
        return Err(GatewayError::InvalidResponse(
            "CJ/T 188 checksum mismatch".to_owned(),
        ));
    }
    if frame[1] != expected_type || frame[2..9] != expected_address {
        return Err(GatewayError::InvalidResponse(
            "CJ/T 188 response meter identity mismatch".to_owned(),
        ));
    }
    if frame[9] & 0x40 != 0 {
        return Err(GatewayError::Protocol(format!(
            "CJ/T 188 meter returned an exception: {:02x?}",
            &frame[11..checksum_index]
        )));
    }
    if frame[9] != CONTROL_READ_DATA_RESPONSE {
        return Err(GatewayError::InvalidResponse(format!(
            "unexpected CJ/T 188 response control code {:#04x}",
            frame[9]
        )));
    }
    let data = &frame[11..checksum_index];
    if data.len() < 3 {
        return Err(GatewayError::InvalidResponse(
            "truncated CJ/T 188 read response".to_owned(),
        ));
    }
    let data_id = u16::from_le_bytes([data[0], data[1]]);
    if data_id != expected_data_id || data[2] != expected_sequence {
        return Err(GatewayError::InvalidResponse(
            "CJ/T 188 response does not match request".to_owned(),
        ));
    }
    Ok(data[3..].to_vec())
}

fn decode_point(payload: &[u8], point: &Cjt188PointConfig) -> Result<Value> {
    let length = point.mapping.byte_len()?;
    let data = payload
        .get(point.mapping.byte_offset..point.mapping.byte_offset + length)
        .ok_or_else(|| {
            GatewayError::InvalidResponse(format!(
                "CJ/T 188 payload is too short for point {}",
                point.point_id
            ))
        })?;
    let raw = match point.mapping.data_type {
        Cjt188DataType::BcdLe => decode_bcd_le(data)?,
        Cjt188DataType::U8 => f64::from(data[0]),
        Cjt188DataType::U16Le => f64::from(u16::from_le_bytes([data[0], data[1]])),
        Cjt188DataType::U32Le => {
            f64::from(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        },
        Cjt188DataType::I16Le => f64::from(i16::from_le_bytes([data[0], data[1]])),
        Cjt188DataType::I32Le => {
            f64::from(i32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        },
        Cjt188DataType::F32Le => {
            f64::from(f32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        },
    };
    if !raw.is_finite() {
        return Err(GatewayError::DataConversion(
            "CJ/T 188 value is not finite".to_owned(),
        ));
    }
    if point.point_type == PointType::Signal {
        let value = raw != 0.0;
        Ok(Value::Bool(if point.reverse { !value } else { value }))
    } else {
        let value = TransformConfig::linear(point.scale, point.offset).apply(raw);
        if !value.is_finite() {
            return Err(GatewayError::DataConversion(
                "CJ/T 188 transformed value is not finite".to_owned(),
            ));
        }
        Ok(Value::Float(value))
    }
}

fn decode_bcd_le(data: &[u8]) -> Result<f64> {
    let mut value = 0_u64;
    let mut multiplier = 1_u64;
    for byte in data {
        let low = byte & 0x0f;
        let high = byte >> 4;
        if low > 9 || high > 9 {
            return Err(GatewayError::DataConversion(
                "invalid CJ/T 188 BCD digit".to_owned(),
            ));
        }
        value = value
            .checked_add(u64::from(low) * multiplier)
            .and_then(|value| value.checked_add(u64::from(high) * multiplier * 10))
            .ok_or_else(|| GatewayError::DataConversion("BCD value overflow".to_owned()))?;
        multiplier = multiplier
            .checked_mul(100)
            .ok_or_else(|| GatewayError::DataConversion("BCD value overflow".to_owned()))?;
    }
    Ok(value as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_is_bcd_encoded_low_byte_first() {
        assert_eq!(
            parse_meter_address("12345678901234").expect("address"),
            [0x34, 0x12, 0x90, 0x78, 0x56, 0x34, 0x12]
        );
    }

    #[test]
    fn request_and_response_frames_validate_identity_and_checksum() {
        let address = parse_meter_address("00000000000001").expect("address");
        let request = encode_read_request(0x10, address, 0x901f, 7).expect("request");
        assert_eq!(&request[..11], &[0x68, 0x10, 1, 0, 0, 0, 0, 0, 0, 1, 3]);

        let response = encode_frame(
            0x10,
            address,
            CONTROL_READ_DATA_RESPONSE,
            &[0x1f, 0x90, 7, 0x78, 0x56, 0x34, 0x12],
        )
        .expect("response");
        assert_eq!(
            decode_read_response(&response, 0x10, address, 0x901f, 7).expect("payload"),
            [0x78, 0x56, 0x34, 0x12]
        );
    }

    #[test]
    fn bcd_and_numeric_mappings_decode_with_point_transform() {
        let bcd_point = Cjt188PointConfig {
            point_id: 1,
            point_type: PointType::Telemetry,
            mapping: Cjt188PointMapping {
                data_id: 1,
                byte_offset: 0,
                data_type: Cjt188DataType::BcdLe,
                byte_length: Some(1),
            },
            scale: 0.01,
            offset: 0.0,
            reverse: false,
        };
        assert_eq!(
            decode_point(&[0x78], &bcd_point).expect("value"),
            Value::Float(0.78)
        );

        let signal = Cjt188PointConfig {
            point_id: 2,
            point_type: PointType::Signal,
            mapping: Cjt188PointMapping {
                data_id: 1,
                byte_offset: 0,
                data_type: Cjt188DataType::U8,
                byte_length: None,
            },
            scale: 1.0,
            offset: 0.0,
            reverse: true,
        };
        assert_eq!(
            decode_point(&[1], &signal).expect("value"),
            Value::Bool(false)
        );
    }

    #[test]
    fn mapping_is_read_only_and_strict() {
        let mapping = serde_json::json!({
            "data_id": 36895,
            "byte_offset": 1,
            "data_type": "u32_le"
        });
        assert!(parse_point_mapping(&mapping, PointType::Telemetry).is_ok());
        assert!(parse_point_mapping(&mapping, PointType::Control).is_err());
        assert!(
            parse_point_mapping(
                &serde_json::json!({
                    "data_id": 1,
                    "byte_offset": 254,
                    "data_type": "u32_le"
                }),
                PointType::Telemetry
            )
            .is_err()
        );
        assert!(
            parse_point_mapping(
                &serde_json::json!({"data_id": 1, "data_type": "bcd_le"}),
                PointType::Telemetry
            )
            .is_err()
        );
        let bcd = parse_point_mapping(
            &serde_json::json!({
                "data_id": 1,
                "data_type": "bcd_le",
                "byte_length": 4
            }),
            PointType::Telemetry,
        )
        .expect("multi-byte BCD mapping");
        assert_eq!(bcd.byte_len().expect("length"), 4);
    }
}
