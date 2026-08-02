//! IEC 60870-5-101 unbalanced-master protocol adapter.
//!
//! Supports general interrogation over a native serial port or a transparent
//! TCP/serial gateway. The parser accepts the common single-point,
//! double-point, step-position, bit-string, normalized, scaled, floating-point,
//! and integrated-total ASDUs, with or without CP56Time2a timestamps.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_core::PointType;
use aether_domain::PointQuality;
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

const START_FIXED: u8 = 0x10;
const START_VARIABLE: u8 = 0x68;
const SINGLE_ACK: u8 = 0xe5;
const FRAME_END: u8 = 0x16;
const TYPE_GENERAL_INTERROGATION: u8 = 100;
const CAUSE_ACTIVATION: u8 = 6;
const CAUSE_ACTIVATION_CONFIRMATION: u8 = 7;
const CAUSE_ACTIVATION_TERMINATION: u8 = 10;
const DEFAULT_PORT: u16 = 2_404;
const DEFAULT_BAUD_RATE: u32 = 9_600;
const DEFAULT_TIMEOUT_MS: u64 = 3_000;
const MAX_LINK_PAYLOAD: usize = 255;
const MAX_INTERROGATION_FRAMES: usize = 256;

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

fn default_link_address_size() -> u8 {
    1
}

fn default_common_address_size() -> u8 {
    2
}

fn default_cause_size() -> u8 {
    2
}

fn default_ioa_size() -> u8 {
    3
}

/// IEC 101 channel parameters. Exactly one of `host` and `device` is required.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Iec101ParamsConfig {
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
    pub(crate) link_address: u16,
    #[serde(default = "default_link_address_size")]
    pub(crate) link_address_size: u8,
    pub(crate) common_address: u16,
    #[serde(default = "default_common_address_size")]
    pub(crate) common_address_size: u8,
    #[serde(default = "default_cause_size")]
    pub(crate) cause_size: u8,
    #[serde(default = "default_ioa_size")]
    pub(crate) ioa_size: u8,
}

impl Iec101ParamsConfig {
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
                "IEC 101 requires exactly one nonblank host or serial device".to_owned(),
            ));
        }
        if host && self.port == 0 {
            return Err(GatewayError::Config(
                "IEC 101 TCP port must be greater than zero".to_owned(),
            ));
        }
        if self.baud_rate == 0 {
            return Err(GatewayError::Config(
                "IEC 101 baud_rate must be greater than zero".to_owned(),
            ));
        }
        if !matches!(
            self.parity.to_ascii_lowercase().as_str(),
            "none" | "even" | "odd"
        ) {
            return Err(GatewayError::Config(
                "IEC 101 parity must be none, even, or odd".to_owned(),
            ));
        }
        if self.timeout_ms == 0 || self.timeout_ms > aether_config::io::MAX_CHANNEL_TIMING_MS {
            return Err(GatewayError::Config(format!(
                "IEC 101 timeout_ms must be between 1 and {}",
                aether_config::io::MAX_CHANNEL_TIMING_MS
            )));
        }
        if !(1..=2).contains(&self.link_address_size) {
            return Err(GatewayError::Config(
                "IEC 101 link_address_size must be 1 or 2".to_owned(),
            ));
        }
        if self.link_address_size == 1 && self.link_address > u16::from(u8::MAX) {
            return Err(GatewayError::Config(
                "IEC 101 link_address exceeds configured one-byte field".to_owned(),
            ));
        }
        if !(1..=2).contains(&self.common_address_size) {
            return Err(GatewayError::Config(
                "IEC 101 common_address_size must be 1 or 2".to_owned(),
            ));
        }
        if self.common_address_size == 1 && self.common_address > u16::from(u8::MAX) {
            return Err(GatewayError::Config(
                "IEC 101 common_address exceeds configured one-byte field".to_owned(),
            ));
        }
        if !(1..=2).contains(&self.cause_size) {
            return Err(GatewayError::Config(
                "IEC 101 cause_size must be 1 or 2".to_owned(),
            ));
        }
        if !(1..=3).contains(&self.ioa_size) {
            return Err(GatewayError::Config(
                "IEC 101 ioa_size must be between 1 and 3".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Per-point IEC 101 information object mapping.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Iec101PointMapping {
    pub(crate) ioa: u32,
    #[serde(default)]
    pub(crate) type_id: u8,
}

pub(crate) fn parse_point_mapping(
    mapping: &serde_json::Value,
    point_type: PointType,
) -> Result<Iec101PointMapping> {
    if !matches!(point_type, PointType::Telemetry | PointType::Signal) {
        return Err(GatewayError::Config(
            "IEC 101 command mappings are not enabled by this polling adapter".to_owned(),
        ));
    }
    let mapping: Iec101PointMapping = serde_json::from_value(mapping.clone())
        .map_err(|error| GatewayError::Config(format!("invalid IEC 101 mapping: {error}")))?;
    if mapping.ioa > 0x00ff_ffff {
        return Err(GatewayError::Config(
            "IEC 101 IOA must fit the maximum three-byte field".to_owned(),
        ));
    }
    if mapping.type_id != 0 && !is_measurement_type(mapping.type_id) {
        return Err(GatewayError::Config(format!(
            "IEC 101 type_id {} is not a supported measurement type",
            mapping.type_id
        )));
    }
    Ok(mapping)
}

#[derive(Debug, Clone)]
pub(crate) struct Iec101PointConfig {
    pub(crate) point_id: u32,
    pub(crate) point_type: PointType,
    pub(crate) mapping: Iec101PointMapping,
    pub(crate) scale: f64,
    pub(crate) offset: f64,
    pub(crate) reverse: bool,
}

enum Iec101Transport {
    Tcp(TcpStream),
    Serial(SerialStream),
}

impl Iec101Transport {
    async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Tcp(stream) => write_all(stream, bytes).await,
            Self::Serial(stream) => write_all(stream, bytes).await,
        }
    }

    async fn read_frame(&mut self, duration: Duration, link_address_size: u8) -> Result<Vec<u8>> {
        match self {
            Self::Tcp(stream) => read_frame(stream, duration, link_address_size).await,
            Self::Serial(stream) => read_frame(stream, duration, link_address_size).await,
        }
    }
}

/// IEC 101 general-interrogation polling channel.
pub struct Iec101Channel {
    channel_id: u32,
    params: Iec101ParamsConfig,
    points: Vec<Iec101PointConfig>,
    transport: Option<Iec101Transport>,
    frame_count_bit: bool,
    state: ConnectionState,
    diagnostics: AtomicDiagnostics,
    log_context: LogContext,
}

impl Iec101Channel {
    pub(crate) fn new(
        params: Iec101ParamsConfig,
        channel_id: u32,
        points: Vec<Iec101PointConfig>,
    ) -> Result<Self> {
        params.validate()?;
        for point in &points {
            let max_ioa = match params.ioa_size {
                1 => u32::from(u8::MAX),
                2 => u32::from(u16::MAX),
                _ => 0x00ff_ffff,
            };
            if point.mapping.ioa > max_ioa {
                return Err(GatewayError::Config(format!(
                    "IEC 101 point {} IOA exceeds configured field width",
                    point.point_id
                )));
            }
        }
        Ok(Self {
            channel_id,
            params,
            points,
            transport: None,
            frame_count_bit: false,
            state: ConnectionState::Disconnected,
            diagnostics: AtomicDiagnostics::new(),
            log_context: LogContext::new(channel_id),
        })
    }

    async fn general_interrogation(&mut self) -> Result<(DataBatch, Vec<PointFailure>)> {
        let asdu = encode_general_interrogation_asdu(&self.params)?;
        let control = 0x53 | if self.frame_count_bit { 0x20 } else { 0 };
        self.frame_count_bit = !self.frame_count_bit;
        let request = encode_variable_frame(
            control,
            self.params.link_address,
            self.params.link_address_size,
            &asdu,
        )?;
        let duration = Duration::from_millis(self.params.timeout_ms);
        let transport = self.transport.as_mut().ok_or(GatewayError::NotConnected)?;
        timeout(duration, transport.write_all(&request))
            .await
            .map_err(|_| GatewayError::WriteTimeout)??;

        let mappings = build_mapping_index(&self.points);
        let mut values = BTreeMap::<(u32, u8), (f64, PointQuality)>::new();
        let mut confirmed = false;
        let mut terminated = false;
        for _ in 0..MAX_INTERROGATION_FRAMES {
            let frame = transport
                .read_frame(duration, self.params.link_address_size)
                .await?;
            if frame == [SINGLE_ACK] || frame.first() == Some(&START_FIXED) {
                continue;
            }
            let asdu = decode_variable_frame(
                &frame,
                self.params.link_address,
                self.params.link_address_size,
            )?;
            let header = parse_asdu_header(&asdu, &self.params)?;
            if header.type_id == TYPE_GENERAL_INTERROGATION {
                confirmed |= header.cause == CAUSE_ACTIVATION_CONFIRMATION;
                if header.cause == CAUSE_ACTIVATION_TERMINATION {
                    terminated = true;
                    break;
                }
                continue;
            }
            if is_measurement_type(header.type_id) {
                decode_measurement_asdu(&asdu, &self.params, &header, &mut values)?;
            }
        }
        if !confirmed || !terminated {
            return Err(GatewayError::InvalidResponse(
                "IEC 101 general interrogation did not complete".to_owned(),
            ));
        }

        Ok(project_points(&self.points, &mappings, &values))
    }
}

/// Projects interrogated values onto configured points.
fn project_points(
    points: &[Iec101PointConfig],
    mappings: &BTreeMap<(u32, u8), &Iec101PointConfig>,
    values: &BTreeMap<(u32, u8), (f64, PointQuality)>,
) -> (DataBatch, Vec<PointFailure>) {
    let mut batch = DataBatch::with_capacity(points.len());
    let mut failures = Vec::new();
    for point in points {
        let value = values
            .get(&(point.mapping.ioa, point.mapping.type_id))
            .or_else(|| {
                mappings.get(&(point.mapping.ioa, 0)).and_then(|_| {
                    values
                        .iter()
                        .find(|((ioa, _), _)| *ioa == point.mapping.ioa)
                        .map(|(_, value)| value)
                })
            });
        let Some(&(raw, quality)) = value else {
            failures.push(PointFailure::new(
                point.point_id,
                "IEC 101 interrogation returned no mapped value",
            ));
            continue;
        };
        let value = if point.point_type == PointType::Signal {
            Value::Bool(if point.reverse {
                raw == 0.0
            } else {
                raw != 0.0
            })
        } else {
            let transformed = TransformConfig::linear(point.scale, point.offset).apply(raw);
            if !transformed.is_finite() {
                failures.push(PointFailure::new(
                    point.point_id,
                    "IEC 101 transformed value is not finite",
                ));
                continue;
            }
            Value::Float(transformed)
        };
        let mut data_point = DataPoint::new(point.point_id, point.point_type, value);
        data_point.quality = quality;
        batch.add(data_point);
    }
    (batch, failures)
}

impl HasMetadata for Iec101Channel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "iec101",
            display_name: "IEC 60870-5-101",
            description: "IEC 101 unbalanced master over serial or transparent TCP gateway",
            is_recommended: true,
            example_config: serde_json::json!({
                "device": "/dev/ttyUSB0",
                "baud_rate": DEFAULT_BAUD_RATE,
                "parity": "even",
                "timeout_ms": DEFAULT_TIMEOUT_MS,
                "link_address": 1,
                "link_address_size": 1,
                "common_address": 1,
                "common_address_size": 2,
                "cause_size": 2,
                "ioa_size": 3
            }),
            parameters: vec![
                ParameterMetadata::optional(
                    "host",
                    "TCP host",
                    "Transparent serial gateway host; mutually exclusive with device",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "port",
                    "TCP port",
                    "Transparent serial gateway port",
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
                    "Interrogation frame timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(DEFAULT_TIMEOUT_MS),
                )
                .with_integer_range(1, aether_config::io::MAX_CHANNEL_TIMING_MS),
                integer_parameter("link_address", "Link address", 0, u64::from(u16::MAX)),
                integer_parameter("link_address_size", "Link address size", 1, 2),
                integer_parameter("common_address", "Common address", 0, u64::from(u16::MAX)),
                integer_parameter("common_address_size", "Common address size", 1, 2),
                integer_parameter("cause_size", "Cause size", 1, 2),
                integer_parameter("ioa_size", "IOA size", 1, 3),
            ],
        }
    }
}

fn integer_parameter(
    name: &'static str,
    display_name: &'static str,
    minimum: u64,
    maximum: u64,
) -> ParameterMetadata {
    ParameterMetadata::required(
        name,
        display_name,
        "IEC 101 link-layer or ASDU field",
        ParameterType::Integer,
    )
    .with_integer_range(minimum, maximum)
}

#[async_trait]
impl ChannelRuntime for Iec101Channel {
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
            Iec101Transport::Tcp(stream)
        } else {
            let device = self.params.device.as_deref().ok_or_else(|| {
                GatewayError::Config("IEC 101 serial device is missing".to_owned())
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
            Iec101Transport::Serial(serial)
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
        match self.general_interrogation().await {
            Ok((batch, failures)) => {
                self.diagnostics.add_read(batch.len() as u64);
                self.diagnostics.add_error(failures.len() as u64);
                if failures.is_empty() {
                    PollResult::success(batch)
                } else {
                    PollResult::partial(batch, failures)
                }
            },
            Err(error) => {
                self.diagnostics.record_error(error.to_string());
                PollResult::failed(
                    self.points
                        .iter()
                        .map(|point| PointFailure::with_error(point.point_id, error.to_string()))
                        .collect(),
                )
            },
        }
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "iec101".to_owned(),
            connection_state: self.state,
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: serde_json::json!({
                "channel_id": self.channel_id,
                "link_address": self.params.link_address,
                "common_address": self.params.common_address,
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

async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    duration: Duration,
    link_address_size: u8,
) -> Result<Vec<u8>> {
    timeout(duration, read_frame_inner(stream, link_address_size))
        .await
        .map_err(|_| GatewayError::ReadTimeout)?
}

async fn read_frame_inner(
    stream: &mut (impl AsyncRead + Unpin),
    link_address_size: u8,
) -> Result<Vec<u8>> {
    let mut start = [0_u8; 1];
    stream
        .read_exact(&mut start)
        .await
        .map_err(GatewayError::Io)?;
    match start[0] {
        SINGLE_ACK => Ok(vec![SINGLE_ACK]),
        START_FIXED => {
            let mut rest = vec![0_u8; usize::from(link_address_size) + 3];
            stream
                .read_exact(&mut rest)
                .await
                .map_err(GatewayError::Io)?;
            let mut frame = vec![START_FIXED];
            frame.extend_from_slice(&rest);
            Ok(frame)
        },
        START_VARIABLE => {
            let mut length_header = [0_u8; 3];
            stream
                .read_exact(&mut length_header)
                .await
                .map_err(GatewayError::Io)?;
            if length_header[0] != length_header[1] || length_header[2] != START_VARIABLE {
                return Err(GatewayError::InvalidResponse(
                    "invalid IEC 101 variable frame header".to_owned(),
                ));
            }
            let length = usize::from(length_header[0]);
            if length == 0 || length > MAX_LINK_PAYLOAD {
                return Err(GatewayError::InvalidResponse(
                    "invalid IEC 101 variable frame length".to_owned(),
                ));
            }
            let mut rest = vec![0_u8; length + 2];
            stream
                .read_exact(&mut rest)
                .await
                .map_err(GatewayError::Io)?;
            let mut frame = vec![START_VARIABLE];
            frame.extend_from_slice(&length_header);
            frame.extend_from_slice(&rest);
            Ok(frame)
        },
        value => Err(GatewayError::InvalidResponse(format!(
            "unexpected IEC 101 frame start {value:#04x}"
        ))),
    }
}

fn encode_variable_frame(
    control: u8,
    link_address: u16,
    link_address_size: u8,
    asdu: &[u8],
) -> Result<Vec<u8>> {
    let payload_length = 1_usize + usize::from(link_address_size) + asdu.len();
    let length = u8::try_from(payload_length).map_err(|_| {
        GatewayError::InvalidData("IEC 101 link payload exceeds 255 bytes".to_owned())
    })?;
    let mut frame = vec![START_VARIABLE, length, length, START_VARIABLE, control];
    encode_le(link_address.into(), link_address_size, &mut frame);
    frame.extend_from_slice(asdu);
    let checksum = frame[4..]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    frame.push(checksum);
    frame.push(FRAME_END);
    Ok(frame)
}

fn decode_variable_frame(
    frame: &[u8],
    expected_link_address: u16,
    link_address_size: u8,
) -> Result<Vec<u8>> {
    if frame.len() < 8
        || frame[0] != START_VARIABLE
        || frame[1] != frame[2]
        || frame[3] != START_VARIABLE
        || frame.last() != Some(&FRAME_END)
    {
        return Err(GatewayError::InvalidResponse(
            "invalid IEC 101 variable frame".to_owned(),
        ));
    }
    let length = usize::from(frame[1]);
    if frame.len() != length + 6 {
        return Err(GatewayError::InvalidResponse(
            "IEC 101 variable frame length mismatch".to_owned(),
        ));
    }
    // Link user data is one control byte plus the link address. A shorter
    // declared length leaves the ASDU slice below inverted, so a garbled serial
    // line or a hostile transparent-TCP gateway would panic the channel task.
    if length < 1 + usize::from(link_address_size) {
        return Err(GatewayError::InvalidResponse(
            "IEC 101 variable frame is shorter than its link address".to_owned(),
        ));
    }
    let checksum_index = frame.len() - 2;
    let checksum = frame[4..checksum_index]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    if checksum != frame[checksum_index] {
        return Err(GatewayError::InvalidResponse(
            "IEC 101 checksum mismatch".to_owned(),
        ));
    }
    let address_start = 5;
    let address_end = address_start + usize::from(link_address_size);
    let address = decode_le(&frame[address_start..address_end]);
    if address != u32::from(expected_link_address) {
        return Err(GatewayError::InvalidResponse(
            "IEC 101 link address mismatch".to_owned(),
        ));
    }
    Ok(frame[address_end..checksum_index].to_vec())
}

fn encode_general_interrogation_asdu(params: &Iec101ParamsConfig) -> Result<Vec<u8>> {
    let mut asdu = vec![TYPE_GENERAL_INTERROGATION, 1, CAUSE_ACTIVATION];
    if params.cause_size == 2 {
        asdu.push(0);
    }
    encode_le(
        params.common_address.into(),
        params.common_address_size,
        &mut asdu,
    );
    encode_le(0, params.ioa_size, &mut asdu);
    asdu.push(20);
    Ok(asdu)
}

#[derive(Debug, Clone, Copy)]
struct AsduHeader {
    type_id: u8,
    count: usize,
    sequence: bool,
    cause: u8,
    objects_start: usize,
}

fn parse_asdu_header(asdu: &[u8], params: &Iec101ParamsConfig) -> Result<AsduHeader> {
    let header_length = 3 + usize::from(params.cause_size - 1 + params.common_address_size);
    if asdu.len() < header_length {
        return Err(GatewayError::InvalidResponse(
            "truncated IEC 101 ASDU header".to_owned(),
        ));
    }
    let common_start = 2 + usize::from(params.cause_size);
    let common_end = common_start + usize::from(params.common_address_size);
    if decode_le(&asdu[common_start..common_end]) != u32::from(params.common_address) {
        return Err(GatewayError::InvalidResponse(
            "IEC 101 common address mismatch".to_owned(),
        ));
    }
    Ok(AsduHeader {
        type_id: asdu[0],
        count: usize::from(asdu[1] & 0x7f),
        sequence: asdu[1] & 0x80 != 0,
        cause: asdu[2] & 0x3f,
        objects_start: common_end,
    })
}

fn decode_measurement_asdu(
    asdu: &[u8],
    params: &Iec101ParamsConfig,
    header: &AsduHeader,
    output: &mut BTreeMap<(u32, u8), (f64, PointQuality)>,
) -> Result<()> {
    let value_length = measurement_value_length(header.type_id).ok_or_else(|| {
        GatewayError::Unsupported(format!("unsupported IEC 101 ASDU type {}", header.type_id))
    })?;
    let mut cursor = header.objects_start;
    let mut base_ioa = None;
    for index in 0..header.count {
        let ioa = if header.sequence && index > 0 {
            base_ioa
                .and_then(|base: u32| base.checked_add(index as u32))
                .ok_or_else(|| GatewayError::InvalidResponse("IEC 101 IOA overflow".to_owned()))?
        } else {
            let end = cursor + usize::from(params.ioa_size);
            let ioa = decode_le(asdu.get(cursor..end).ok_or_else(|| {
                GatewayError::InvalidResponse("truncated IEC 101 IOA".to_owned())
            })?);
            cursor = end;
            base_ioa.get_or_insert(ioa);
            ioa
        };
        let end = cursor + value_length;
        let value = asdu.get(cursor..end).ok_or_else(|| {
            GatewayError::InvalidResponse("truncated IEC 101 information object".to_owned())
        })?;
        cursor = end;
        output.insert(
            (ioa, header.type_id),
            decode_measurement(header.type_id, value)?,
        );
    }
    Ok(())
}

fn measurement_value_length(type_id: u8) -> Option<usize> {
    match type_id {
        1 | 3 => Some(1),
        5 => Some(2),
        7 | 13 | 15 => Some(5),
        9 | 11 => Some(3),
        30 | 31 => Some(8),
        32 => Some(9),
        33 | 36 | 37 => Some(12),
        34 | 35 => Some(10),
        _ => None,
    }
}

fn is_measurement_type(type_id: u8) -> bool {
    measurement_value_length(type_id).is_some()
}

fn decode_measurement(type_id: u8, value: &[u8]) -> Result<(f64, PointQuality)> {
    let base_type = match type_id {
        30 => 1,
        31 => 3,
        32 => 5,
        33 => 7,
        34 => 9,
        35 => 11,
        36 => 13,
        37 => 15,
        value => value,
    };
    // Every type below carries a QDS descriptor except the integrated total,
    // whose trailing byte is BCR sequence notation: bits 0..=4 are a sequence
    // number that advances with every counter interrogation, and only CY
    // (0x20) and CA (0x40) describe the reading. Masking it with the QDS
    // 0x70 reads sequence-number bit 4 as a quality bit, which marks every
    // counter whose sequence reached 16 as Uncertain — half of all readings.
    let (quality_index, uncertain_mask) = match base_type {
        1 | 3 => (0, 0x70),
        5 => (1, 0x70),
        7 | 13 => (4, 0x70),
        15 => (4, 0x60),
        9 | 11 => (2, 0x70),
        _ => {
            return Err(GatewayError::Unsupported(format!(
                "unsupported IEC 101 measurement type {type_id}"
            )));
        },
    };
    let quality_byte = *value.get(quality_index).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated IEC 101 quality descriptor".to_owned())
    })?;
    let quality = if quality_byte & 0x80 != 0 {
        PointQuality::Bad
    } else if quality_byte & uncertain_mask != 0 {
        PointQuality::Uncertain
    } else {
        PointQuality::Good
    };
    let numeric = match base_type {
        1 => f64::from(value[0] & 0x01),
        3 => f64::from(value[0] & 0x03),
        5 => {
            let raw = value[0] & 0x7f;
            let signed = if raw & 0x40 == 0 { raw } else { raw | 0x80 };
            f64::from(i8::from_ne_bytes([signed]))
        },
        7 => f64::from(u32::from_le_bytes([value[0], value[1], value[2], value[3]])),
        9 => f64::from(i16::from_le_bytes([value[0], value[1]])) / 32_768.0,
        11 => f64::from(i16::from_le_bytes([value[0], value[1]])),
        13 => f64::from(f32::from_le_bytes([value[0], value[1], value[2], value[3]])),
        15 => f64::from(i32::from_le_bytes([value[0], value[1], value[2], value[3]])),
        _ => unreachable!(),
    };
    if !numeric.is_finite() {
        return Err(GatewayError::DataConversion(
            "IEC 101 measurement is not finite".to_owned(),
        ));
    }
    Ok((numeric, quality))
}

fn build_mapping_index(points: &[Iec101PointConfig]) -> BTreeMap<(u32, u8), &Iec101PointConfig> {
    points
        .iter()
        .map(|point| ((point.mapping.ioa, point.mapping.type_id), point))
        .collect()
}

fn encode_le(value: u32, width: u8, output: &mut Vec<u8>) {
    output.extend_from_slice(&value.to_le_bytes()[..usize::from(width)]);
}

fn decode_le(bytes: &[u8]) -> u32 {
    let mut value = [0_u8; 4];
    value[..bytes.len()].copy_from_slice(bytes);
    u32::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_integrated_total_reads_quality_from_bcr_notation_not_a_qds() {
        // M_IT_NA_1 ends in BCR sequence notation, not a QDS: bits 0..=4 hold
        // a sequence number that advances with every counter interrogation.
        // Masking it as a QDS read sequence bit 4 as a quality bit, so every
        // counter whose sequence reached 16 came back Uncertain -- half of
        // every meter's readings, alternating in 16-interrogation blocks.
        for sequence in 0..32_u8 {
            let (numeric, quality) =
                decode_measurement(15, &[0x39, 0x30, 0, 0, sequence]).expect("counter");
            assert_eq!(numeric, 12_345.0, "sequence {sequence}");
            assert_eq!(quality, PointQuality::Good, "sequence {sequence}");
        }

        // Carry, counter-adjusted and invalid still speak for the reading.
        for (flag, expected) in [
            (0x20, PointQuality::Uncertain),
            (0x40, PointQuality::Uncertain),
            (0x80, PointQuality::Bad),
        ] {
            let (_, quality) = decode_measurement(15, &[0, 0, 0, 0, flag]).expect("counter");
            assert_eq!(quality, expected, "flag {flag:#x}");
        }
    }

    #[test]
    fn a_short_float_still_reads_every_qds_quality_bit() {
        // The BCR carve-out must not loosen the descriptor every other type
        // carries: blocked (0x10) is a QDS bit and still means Uncertain.
        for flag in [0x10, 0x20, 0x40] {
            let (_, quality) = decode_measurement(13, &[0, 0, 0, 0, flag]).expect("float");
            assert_eq!(quality, PointQuality::Uncertain, "QDS flag {flag:#x}");
        }
    }

    #[test]
    fn point_projection_matches_the_shared_transform() {
        // Every adapter has to agree with TransformConfig bit for bit; a fused
        // multiply-add rounds once where the shared transform rounds twice, so
        // the same point read through two adapters would not compare equal.
        let (raw, scale, offset) = (0.1_f64, 0.1_f64, -0.01_f64);
        assert_ne!(
            raw.mul_add(scale, offset),
            TransformConfig::linear(scale, offset).apply(raw),
            "pick constants that actually distinguish the two formulas"
        );
        let points = vec![Iec101PointConfig {
            point_id: 4,
            point_type: PointType::Telemetry,
            mapping: Iec101PointMapping {
                ioa: 100,
                type_id: 13,
            },
            scale,
            offset,
            reverse: false,
        }];
        let values = BTreeMap::from([((100, 13), (raw, PointQuality::Good))]);

        let (batch, failures) = project_points(&points, &build_mapping_index(&points), &values);

        assert!(failures.is_empty());
        assert_eq!(
            batch.iter().next().expect("point").value,
            Value::Float(TransformConfig::linear(scale, offset).apply(raw))
        );
    }

    fn params() -> Iec101ParamsConfig {
        Iec101ParamsConfig {
            host: Some("127.0.0.1".to_owned()),
            port: DEFAULT_PORT,
            device: None,
            baud_rate: DEFAULT_BAUD_RATE,
            parity: "even".to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            link_address: 1,
            link_address_size: 1,
            common_address: 1,
            common_address_size: 2,
            cause_size: 2,
            ioa_size: 3,
        }
    }

    #[test]
    fn general_interrogation_is_wrapped_in_ft12_variable_frame() {
        let params = params();
        let asdu = encode_general_interrogation_asdu(&params).expect("ASDU");
        assert_eq!(asdu, [100, 1, 6, 0, 1, 0, 0, 0, 0, 20]);
        let frame = encode_variable_frame(0x53, 1, 1, &asdu).expect("frame");
        assert_eq!(frame[0..5], [0x68, 12, 12, 0x68, 0x53]);
        assert_eq!(decode_variable_frame(&frame, 1, 1).expect("ASDU"), asdu);
    }

    #[test]
    fn variable_frame_shorter_than_its_link_address_is_rejected() {
        // length=2 is a legal eight-byte frame, but with a two-byte link
        // address the ASDU slice below becomes frame[7..6]. Before the guard
        // this panicked the channel task, so a garbled serial line or a hostile
        // transparent-TCP gateway took the channel down instead of dropping one
        // frame. The checksum covers frame[4..6], matching the decoder.
        let frame = vec![0x68, 2, 2, 0x68, 0x53, 0x01, 0x54, FRAME_END];
        let error = decode_variable_frame(&frame, 1, 2).expect_err("must reject");
        assert!(matches!(error, GatewayError::InvalidResponse(_)));
    }

    #[test]
    fn sequential_scaled_values_decode_by_consecutive_ioa() {
        let params = params();
        let asdu = [
            11, 0x82, 20, 0, 1, 0, 5, 0, 0, 0x34, 0x12, 0, 0xfe, 0xff, 0x80,
        ];
        let header = parse_asdu_header(&asdu, &params).expect("header");
        let mut values = BTreeMap::new();
        decode_measurement_asdu(&asdu, &params, &header, &mut values).expect("measurements");
        assert_eq!(values[&(5, 11)], (4660.0, PointQuality::Good));
        assert_eq!(values[&(6, 11)], (-2.0, PointQuality::Bad));
    }

    #[test]
    fn timestamped_float_ignores_time_but_preserves_quality() {
        let mut value = vec![0, 0, 0x48, 0x41, 0x20];
        value.extend_from_slice(&[0; 7]);
        assert_eq!(
            decode_measurement(36, &value).expect("measurement"),
            (12.5, PointQuality::Uncertain)
        );
    }

    #[test]
    fn mapping_rejects_commands_and_unknown_type_ids() {
        let valid = serde_json::json!({"ioa": 100, "type_id": 13});
        assert!(parse_point_mapping(&valid, PointType::Telemetry).is_ok());
        assert!(parse_point_mapping(&valid, PointType::Control).is_err());
        assert!(
            parse_point_mapping(
                &serde_json::json!({"ioa": 100, "type_id": 99}),
                PointType::Telemetry
            )
            .is_err()
        );
    }
}
