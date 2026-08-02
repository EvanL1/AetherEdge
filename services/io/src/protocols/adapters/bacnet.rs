//! BACnet/IP protocol adapter.
//!
//! The adapter speaks direct BACnet/IP over UDP and deliberately implements a
//! small, interoperable client surface: confirmed `ReadProperty` and
//! `WriteProperty`. Device discovery, BBMD management, BACnet/SC, COV
//! subscriptions, and protocol model catalogs remain outside this adapter.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use aether_core::PointType;
use async_trait::async_trait;
use serde::Deserialize;
use tokio::net::{UdpSocket, lookup_host};
use tokio::time::timeout;

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

const BACNET_IP_TYPE: u8 = 0x81;
const BVLC_ORIGINAL_UNICAST_NPDU: u8 = 0x0a;
const SERVICE_CONFIRMED_READ_PROPERTY: u8 = 0x0c;
const SERVICE_CONFIRMED_WRITE_PROPERTY: u8 = 0x0f;
const DEFAULT_PORT: u16 = 47_808;
const DEFAULT_TIMEOUT_MS: u64 = 2_000;
const MAX_DATAGRAM: usize = 1_476;

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// BACnet/IP channel parameters.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BacnetParamsConfig {
    pub(crate) host: String,
    #[serde(default = "default_port")]
    pub(crate) port: u16,
    #[serde(default = "default_timeout_ms")]
    pub(crate) timeout_ms: u64,
    #[serde(default)]
    pub(crate) local_port: u16,
}

impl BacnetParamsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            return Err(GatewayError::Config(
                "BACnet/IP host must not be blank".to_owned(),
            ));
        }
        if self.port == 0 {
            return Err(GatewayError::Config(
                "BACnet/IP port must be greater than zero".to_owned(),
            ));
        }
        if self.timeout_ms == 0 || self.timeout_ms > aether_config::io::MAX_CHANNEL_TIMING_MS {
            return Err(GatewayError::Config(format!(
                "BACnet/IP timeout_ms must be between 1 and {}",
                aether_config::io::MAX_CHANNEL_TIMING_MS
            )));
        }
        Ok(())
    }
}

/// Per-point BACnet object/property address.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BacnetPointMapping {
    pub(crate) object_type: u16,
    pub(crate) object_instance: u32,
    pub(crate) property_id: u32,
    #[serde(default)]
    pub(crate) array_index: Option<u32>,
    #[serde(default)]
    pub(crate) priority: Option<u8>,
}

impl BacnetPointMapping {
    fn validate(&self) -> Result<()> {
        if self.object_type > 0x03ff {
            return Err(GatewayError::Config(
                "BACnet object_type must fit the 10-bit object type field".to_owned(),
            ));
        }
        if self.object_instance > 0x003f_ffff {
            return Err(GatewayError::Config(
                "BACnet object_instance must fit the 22-bit instance field".to_owned(),
            ));
        }
        if self
            .priority
            .is_some_and(|priority| !(1..=16).contains(&priority))
        {
            return Err(GatewayError::Config(
                "BACnet command priority must be between 1 and 16".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn parse_point_mapping(
    mapping: &serde_json::Value,
    _point_type: PointType,
) -> Result<BacnetPointMapping> {
    let mapping: BacnetPointMapping = serde_json::from_value(mapping.clone())
        .map_err(|error| GatewayError::Config(format!("invalid BACnet mapping: {error}")))?;
    mapping.validate()?;
    Ok(mapping)
}

#[derive(Debug, Clone)]
pub(crate) struct BacnetPointConfig {
    pub(crate) point_id: u32,
    pub(crate) point_type: PointType,
    pub(crate) mapping: BacnetPointMapping,
    pub(crate) scale: f64,
    pub(crate) offset: f64,
    pub(crate) reverse: bool,
}

/// One BACnet/IP channel targeting a known device address.
pub struct BacnetChannel {
    channel_id: u32,
    params: BacnetParamsConfig,
    points: Vec<BacnetPointConfig>,
    socket: Option<UdpSocket>,
    state: ConnectionState,
    invoke_id: u8,
    diagnostics: AtomicDiagnostics,
    log_context: LogContext,
}

impl BacnetChannel {
    pub(crate) fn new(
        params: BacnetParamsConfig,
        channel_id: u32,
        points: Vec<BacnetPointConfig>,
    ) -> Result<Self> {
        params.validate()?;
        Ok(Self {
            channel_id,
            params,
            points,
            socket: None,
            state: ConnectionState::Disconnected,
            invoke_id: 0,
            diagnostics: AtomicDiagnostics::new(),
            log_context: LogContext::new(channel_id),
        })
    }

    fn next_invoke_id(&mut self) -> u8 {
        self.invoke_id = self.invoke_id.wrapping_add(1);
        self.invoke_id
    }

    /// Sends one confirmed request and waits for the reply that answers it.
    ///
    /// UDP carries no request/response pairing of its own, so a reply that
    /// arrives after this channel gave up stays queued. Handing it to the next
    /// caller desynchronised the channel permanently: every later read pulled
    /// the previous point's reply, failed its invoke-ID check, and pushed the
    /// mismatch one point further along. One timeout cost every later poll.
    async fn transact(&mut self, invoke_id: u8, apdu: &[u8]) -> Result<Vec<u8>> {
        let socket = self.socket.as_ref().ok_or(GatewayError::NotConnected)?;
        let packet = encode_bvlc(apdu)?;
        let timeout_duration = Duration::from_millis(self.params.timeout_ms);
        timeout(timeout_duration, socket.send(&packet))
            .await
            .map_err(|_| GatewayError::WriteTimeout)??;

        let deadline = tokio::time::Instant::now() + timeout_duration;
        let mut response = vec![0_u8; MAX_DATAGRAM];
        loop {
            let length = tokio::time::timeout_at(deadline, socket.recv(&mut response))
                .await
                .map_err(|_| GatewayError::ReadTimeout)??;
            // Every confirmed-service reply carries its invoke ID in the byte
            // after the PDU type, whichever reply kind it is.
            if let Ok(reply) = decode_bvlc_apdu(&response[..length])
                && reply.get(1) == Some(&invoke_id)
            {
                return Ok(reply.to_owned());
            }
            self.diagnostics
                .record_error("discarded a BACnet datagram that answers no pending request");
        }
    }

    async fn read_point(&mut self, point: &BacnetPointConfig) -> Result<Value> {
        let invoke_id = self.next_invoke_id();
        let request = encode_read_property(invoke_id, &point.mapping)?;
        let response = self.transact(invoke_id, &request).await?;
        let raw = decode_read_property_ack(&response, invoke_id)?;
        transform_value(raw, point)
    }

    async fn write_point(
        &mut self,
        point_type: PointType,
        point_id: u32,
        value: f64,
    ) -> Result<()> {
        let point = self
            .points
            .iter()
            .find(|point| point.point_id == point_id && point.point_type == point_type)
            .cloned()
            .ok_or_else(|| GatewayError::PointNotFound(point_id.to_string()))?;
        let invoke_id = self.next_invoke_id();
        let wire_value = match point_type {
            PointType::Control => {
                if point.reverse {
                    f64::from(value == 0.0)
                } else {
                    value
                }
            },
            PointType::Adjustment => {
                if point.scale == 0.0 {
                    return Err(GatewayError::Config(
                        "BACnet adjustment scale must not be zero".to_owned(),
                    ));
                }
                (value - point.offset) / point.scale
            },
            _ => value,
        };
        let request = encode_write_property(invoke_id, &point.mapping, wire_value)?;
        let response = self.transact(invoke_id, &request).await?;
        decode_simple_ack(&response, invoke_id, SERVICE_CONFIRMED_WRITE_PROPERTY)
    }
}

impl HasMetadata for BacnetChannel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "bacnet_ip",
            display_name: "BACnet/IP",
            description: "BACnet/IP direct UDP client with ReadProperty and WriteProperty",
            is_recommended: true,
            example_config: serde_json::json!({
                "host": "192.168.1.20",
                "port": DEFAULT_PORT,
                "timeout_ms": DEFAULT_TIMEOUT_MS,
                "local_port": 0
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "host",
                    "Device host",
                    "BACnet/IP device hostname or IP address",
                    ParameterType::String,
                )
                .with_min_length(1),
                ParameterMetadata::optional(
                    "port",
                    "UDP port",
                    "BACnet/IP UDP port",
                    ParameterType::Integer,
                    serde_json::json!(DEFAULT_PORT),
                )
                .with_integer_range(1, u64::from(u16::MAX)),
                ParameterMetadata::optional(
                    "timeout_ms",
                    "Timeout",
                    "Read and write timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(DEFAULT_TIMEOUT_MS),
                )
                .with_integer_range(1, aether_config::io::MAX_CHANNEL_TIMING_MS),
                ParameterMetadata::optional(
                    "local_port",
                    "Local UDP port",
                    "Local UDP port; zero selects an ephemeral port",
                    ParameterType::Integer,
                    serde_json::json!(0),
                )
                .with_integer_range(0, u64::from(u16::MAX)),
            ],
        }
    }
}

#[async_trait]
impl ChannelRuntime for BacnetChannel {
    async fn connect(&mut self) -> Result<()> {
        self.state = ConnectionState::Connecting;
        let mut addresses = lookup_host((self.params.host.as_str(), self.params.port))
            .await
            .map_err(|error| GatewayError::Connection(error.to_string()))?;
        let remote = addresses.next().ok_or_else(|| {
            GatewayError::Connection("BACnet/IP host resolved to no addresses".to_owned())
        })?;
        let local_ip = match remote.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        };
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, self.params.local_port))
            .await
            .map_err(|error| GatewayError::Connection(error.to_string()))?;
        socket
            .connect(remote)
            .await
            .map_err(|error| GatewayError::Connection(error.to_string()))?;
        self.socket = Some(socket);
        self.state = ConnectionState::Connected;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.socket = None;
        self.state = ConnectionState::Disconnected;
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        if self.socket.is_none() {
            return PollResult::failed(
                self.points
                    .iter()
                    .map(|point| PointFailure::new(point.point_id, "Not connected"))
                    .collect(),
            );
        }

        let mut batch = DataBatch::with_capacity(self.points.len());
        let mut failures = Vec::new();
        for point in
            self.points.clone().into_iter().filter(|point| {
                matches!(point.point_type, PointType::Telemetry | PointType::Signal)
            })
        {
            match self.read_point(&point).await {
                Ok(value) => {
                    batch.add(DataPoint::new(point.point_id, point.point_type, value));
                    self.diagnostics.inc_read();
                },
                Err(error) => {
                    self.diagnostics.record_error(error.to_string());
                    failures.push(PointFailure::with_error(point.point_id, error.to_string()));
                },
            }
        }
        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let mut written = 0;
        for &(point_id, value) in commands {
            self.write_point(PointType::Control, point_id, value)
                .await?;
            self.diagnostics.inc_write();
            written += 1;
        }
        Ok(written)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let mut written = 0;
        for &(point_id, value) in adjustments {
            self.write_point(PointType::Adjustment, point_id, value)
                .await?;
            self.diagnostics.inc_write();
            written += 1;
        }
        Ok(written)
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "bacnet_ip".to_owned(),
            connection_state: self.state,
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: serde_json::json!({
                "channel_id": self.channel_id,
                "host": self.params.host,
                "port": self.params.port,
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

fn encode_bvlc(apdu: &[u8]) -> Result<Vec<u8>> {
    let length = 6_usize
        .checked_add(apdu.len())
        .ok_or_else(|| GatewayError::Protocol("BACnet packet length overflow".to_owned()))?;
    let length = u16::try_from(length)
        .map_err(|_| GatewayError::Protocol("BACnet packet exceeds u16 length".to_owned()))?;
    let mut packet = Vec::with_capacity(usize::from(length));
    packet.extend_from_slice(&[
        BACNET_IP_TYPE,
        BVLC_ORIGINAL_UNICAST_NPDU,
        (length >> 8) as u8,
        length as u8,
        0x01,
        0x04,
    ]);
    packet.extend_from_slice(apdu);
    Ok(packet)
}

fn decode_bvlc_apdu(packet: &[u8]) -> Result<&[u8]> {
    if packet.len() < 6 || packet[0] != BACNET_IP_TYPE {
        return Err(GatewayError::InvalidResponse(
            "invalid BACnet/IP BVLC header".to_owned(),
        ));
    }
    let declared = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if declared != packet.len() {
        return Err(GatewayError::InvalidResponse(format!(
            "BACnet/IP length mismatch: declared {declared}, received {}",
            packet.len()
        )));
    }
    if packet[4] != 0x01 {
        return Err(GatewayError::InvalidResponse(
            "unsupported BACnet NPDU version".to_owned(),
        ));
    }

    let control = packet[5];
    let mut cursor = 6;
    if control & 0x20 != 0 {
        skip_npdu_address(packet, &mut cursor)?;
        cursor = cursor
            .checked_add(1)
            .ok_or_else(|| GatewayError::InvalidResponse("NPDU cursor overflow".to_owned()))?;
    }
    if control & 0x08 != 0 {
        skip_npdu_address(packet, &mut cursor)?;
    }
    if control & 0x80 != 0 {
        return Err(GatewayError::InvalidResponse(
            "BACnet network-layer message is not an application response".to_owned(),
        ));
    }
    packet
        .get(cursor..)
        .filter(|apdu| !apdu.is_empty())
        .ok_or_else(|| GatewayError::InvalidResponse("missing BACnet APDU".to_owned()))
}

fn skip_npdu_address(packet: &[u8], cursor: &mut usize) -> Result<()> {
    let length_index = cursor
        .checked_add(2)
        .ok_or_else(|| GatewayError::InvalidResponse("NPDU cursor overflow".to_owned()))?;
    let length = usize::from(*packet.get(length_index).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated BACnet NPDU address".to_owned())
    })?);
    *cursor = length_index
        .checked_add(1 + length)
        .ok_or_else(|| GatewayError::InvalidResponse("NPDU cursor overflow".to_owned()))?;
    if *cursor > packet.len() {
        return Err(GatewayError::InvalidResponse(
            "truncated BACnet NPDU address".to_owned(),
        ));
    }
    Ok(())
}

fn object_identifier(mapping: &BacnetPointMapping) -> u32 {
    (u32::from(mapping.object_type) << 22) | mapping.object_instance
}

fn encode_read_property(invoke_id: u8, mapping: &BacnetPointMapping) -> Result<Vec<u8>> {
    let mut apdu = vec![0x00, 0x05, invoke_id, SERVICE_CONFIRMED_READ_PROPERTY, 0x0c];
    apdu.extend_from_slice(&object_identifier(mapping).to_be_bytes());
    encode_context_unsigned(1, mapping.property_id, &mut apdu)?;
    if let Some(array_index) = mapping.array_index {
        encode_context_unsigned(2, array_index, &mut apdu)?;
    }
    Ok(apdu)
}

fn encode_write_property(
    invoke_id: u8,
    mapping: &BacnetPointMapping,
    value: f64,
) -> Result<Vec<u8>> {
    if !value.is_finite() {
        return Err(GatewayError::InvalidData(
            "BACnet write value must be finite".to_owned(),
        ));
    }
    let mut apdu = vec![
        0x00,
        0x05,
        invoke_id,
        SERVICE_CONFIRMED_WRITE_PROPERTY,
        0x0c,
    ];
    apdu.extend_from_slice(&object_identifier(mapping).to_be_bytes());
    encode_context_unsigned(1, mapping.property_id, &mut apdu)?;
    if let Some(array_index) = mapping.array_index {
        encode_context_unsigned(2, array_index, &mut apdu)?;
    }
    apdu.push(0x3e);
    if matches!(mapping.object_type, 4 | 5) {
        apdu.extend_from_slice(&[0x91, u8::from(value != 0.0)]);
    } else {
        // A BACnet Real is 32-bit, and an `as` cast saturates: a finite f64
        // above f32::MAX narrows to +inf and would command the device with an
        // infinity it never asked for. Refuse instead of writing one.
        let real = value as f32;
        if !real.is_finite() {
            return Err(GatewayError::DataConversion(format!(
                "BACnet write value {value} does not fit a 32-bit BACnet Real"
            )));
        }
        apdu.push(0x44);
        apdu.extend_from_slice(&real.to_bits().to_be_bytes());
    }
    apdu.push(0x3f);
    if let Some(priority) = mapping.priority {
        encode_context_unsigned(4, u32::from(priority), &mut apdu)?;
    }
    Ok(apdu)
}

fn encode_context_unsigned(tag: u8, value: u32, output: &mut Vec<u8>) -> Result<()> {
    if tag > 14 {
        return Err(GatewayError::Internal(
            "extended BACnet context tags are unsupported".to_owned(),
        ));
    }
    let bytes = if value <= u32::from(u8::MAX) {
        &value.to_be_bytes()[3..]
    } else if value <= u32::from(u16::MAX) {
        &value.to_be_bytes()[2..]
    } else if value <= 0x00ff_ffff {
        &value.to_be_bytes()[1..]
    } else {
        &value.to_be_bytes()[..]
    };
    output.push((tag << 4) | 0x08 | u8::try_from(bytes.len()).unwrap_or(4));
    output.extend_from_slice(bytes);
    Ok(())
}

/// Skips one primitive context-tagged element and returns the remaining bytes.
///
/// A ReadProperty ACK precedes its opening tag with an object identifier and a
/// property identifier, optionally followed by an array index. All three are
/// primitive context tags, so their length lives in the low three tag bits.
fn skip_context_element(body: &[u8], expected_tag: u8) -> Result<&[u8]> {
    let &tag = body.first().ok_or_else(|| {
        GatewayError::InvalidResponse("truncated BACnet ReadProperty ACK".to_owned())
    })?;
    if tag & 0x08 == 0 || tag >> 4 != expected_tag {
        return Err(GatewayError::InvalidResponse(format!(
            "unexpected BACnet ACK element, wanted context tag {expected_tag}"
        )));
    }
    let length = usize::from(tag & 0x07);
    if length == 5 {
        return Err(GatewayError::InvalidResponse(
            "extended-length BACnet context tags are not supported".to_owned(),
        ));
    }
    body.get(1 + length..).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated BACnet ReadProperty ACK".to_owned())
    })
}

fn decode_read_property_ack(apdu: &[u8], invoke_id: u8) -> Result<Value> {
    let body = parse_ack_header(apdu, invoke_id, SERVICE_CONFIRMED_READ_PROPERTY)?;
    // Walk the structure instead of scanning for the 0x3e opening tag. Object
    // instance 62 encodes as `00 00 00 3e`, and instance 0x3E1234 puts 0x3e in
    // the first identifier byte, so a scan either fails on an ordinary object
    // or decodes the wrong bytes and writes a bogus value into live state.
    let after_object = skip_context_element(body, 0)?;
    let after_property = skip_context_element(after_object, 1)?;
    let after_index = match after_property.first() {
        Some(tag) if tag >> 4 == 2 && tag & 0x08 != 0 => skip_context_element(after_property, 2)?,
        _ => after_property,
    };
    match after_index.split_first() {
        Some((&0x3e, value)) => decode_application_value(value),
        _ => Err(GatewayError::InvalidResponse(
            "missing BACnet property value".to_owned(),
        )),
    }
}

fn decode_simple_ack(apdu: &[u8], invoke_id: u8, service: u8) -> Result<()> {
    match apdu {
        [pdu, response_invoke, response_service, ..]
            if pdu >> 4 == 0x02
                && *response_invoke == invoke_id
                && *response_service == service =>
        {
            Ok(())
        },
        _ => Err(decode_apdu_error(apdu, invoke_id)),
    }
}

fn parse_ack_header(apdu: &[u8], invoke_id: u8, service: u8) -> Result<&[u8]> {
    if apdu.len() < 3 {
        return Err(GatewayError::InvalidResponse(
            "truncated BACnet APDU".to_owned(),
        ));
    }
    if apdu[0] >> 4 != 0x03 {
        return Err(decode_apdu_error(apdu, invoke_id));
    }
    let segmented = apdu[0] & 0x08 != 0;
    let (response_invoke, response_service, body_index) = if segmented {
        if apdu.len() < 5 {
            return Err(GatewayError::InvalidResponse(
                "truncated segmented BACnet ComplexACK".to_owned(),
            ));
        }
        (apdu[1], apdu[4], 5)
    } else {
        (apdu[1], apdu[2], 3)
    };
    if response_invoke != invoke_id || response_service != service {
        return Err(GatewayError::InvalidResponse(
            "BACnet response does not match request".to_owned(),
        ));
    }
    apdu.get(body_index..)
        .ok_or_else(|| GatewayError::InvalidResponse("missing BACnet ComplexACK body".to_owned()))
}

fn decode_apdu_error(apdu: &[u8], invoke_id: u8) -> GatewayError {
    let pdu_type = apdu.first().map_or(0xff, |byte| byte >> 4);
    let response_invoke = apdu.get(1).copied();
    GatewayError::Protocol(format!(
        "BACnet APDU type {pdu_type:#x} for invoke {response_invoke:?}, expected acknowledgement for {invoke_id}"
    ))
}

fn decode_application_value(input: &[u8]) -> Result<Value> {
    let header = *input
        .first()
        .ok_or_else(|| GatewayError::InvalidResponse("missing BACnet value tag".to_owned()))?;
    if header & 0x08 != 0 {
        return Err(GatewayError::InvalidResponse(
            "BACnet property value is not an application tag".to_owned(),
        ));
    }
    let tag = header >> 4;
    let lvt = usize::from(header & 0x07);
    if tag == 1 {
        return Ok(Value::Bool(lvt != 0));
    }
    if lvt == 5 {
        return Err(GatewayError::Unsupported(
            "extended-length BACnet application values are unsupported".to_owned(),
        ));
    }
    let data = input.get(1..1 + lvt).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated BACnet application value".to_owned())
    })?;
    match tag {
        2 | 9 => decode_unsigned(data).map(Value::Integer),
        3 => decode_signed(data).map(Value::Integer),
        4 if data.len() == 4 => Ok(Value::Float(f64::from(f32::from_bits(u32::from_be_bytes(
            [data[0], data[1], data[2], data[3]],
        ))))),
        5 if data.len() == 8 => Ok(Value::Float(f64::from_bits(u64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ])))),
        _ => Err(GatewayError::Unsupported(format!(
            "BACnet application tag {tag} with length {} is not numeric",
            data.len()
        ))),
    }
}

fn decode_unsigned(data: &[u8]) -> Result<i64> {
    if data.is_empty() || data.len() > 8 {
        return Err(GatewayError::InvalidResponse(
            "invalid BACnet unsigned length".to_owned(),
        ));
    }
    let value = data
        .iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte));
    i64::try_from(value)
        .map_err(|_| GatewayError::DataConversion("BACnet unsigned exceeds i64".to_owned()))
}

fn decode_signed(data: &[u8]) -> Result<i64> {
    if data.is_empty() || data.len() > 8 {
        return Err(GatewayError::InvalidResponse(
            "invalid BACnet signed length".to_owned(),
        ));
    }
    let mut bytes = if data[0] & 0x80 == 0 {
        [0_u8; 8]
    } else {
        [0xff; 8]
    };
    bytes[8 - data.len()..].copy_from_slice(data);
    Ok(i64::from_be_bytes(bytes))
}

fn transform_value(value: Value, point: &BacnetPointConfig) -> Result<Value> {
    match point.point_type {
        PointType::Signal => {
            let value = value.as_bool().ok_or_else(|| {
                GatewayError::DataConversion("BACnet signal is not boolean".to_owned())
            })?;
            Ok(Value::Bool(if point.reverse { !value } else { value }))
        },
        _ => {
            let value = value.as_f64().ok_or_else(|| {
                GatewayError::DataConversion("BACnet value is not numeric".to_owned())
            })?;
            let transformed = TransformConfig::linear(point.scale, point.offset).apply(value);
            if !transformed.is_finite() {
                return Err(GatewayError::DataConversion(
                    "BACnet transformed value is not finite".to_owned(),
                ));
            }
            Ok(Value::Float(transformed))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_that_cannot_narrow_to_a_bacnet_real_is_refused() {
        // A BACnet Real is 32-bit and an `as` cast saturates, so this value
        // used to reach the device as +inf: a command nobody wrote.
        assert!((1e300_f64 as f32).is_infinite());
        let error = encode_write_property(1, &present_value(), 1e300).expect_err("must refuse");
        assert!(
            matches!(error, GatewayError::DataConversion(_)),
            "unexpected error: {error:?}"
        );
        encode_write_property(1, &present_value(), 12.5).expect("an in-range write still encodes");
    }

    #[tokio::test]
    async fn a_reply_to_an_abandoned_request_never_answers_the_next_one() {
        // UDP pairs nothing. A reply that arrives after this channel gave up
        // stays queued, and handing it to the next read desynchronised the
        // channel for good: every later point failed on the previous point's
        // reply.
        fn complex_ack(invoke_id: u8) -> Vec<u8> {
            let apdu = [
                0x30, invoke_id, 0x0c, 0x0c, 0, 0, 0, 7, 0x19, 85, 0x3e, 0x44, 0x41, 0x48, 0, 0,
                0x3f,
            ];
            encode_bvlc(&apdu).expect("ack")
        }

        let device = UdpSocket::bind("127.0.0.1:0").await.expect("device");
        let address = device.local_addr().expect("address");
        tokio::spawn(async move {
            let mut request = vec![0_u8; MAX_DATAGRAM];
            let (_, peer) = device.recv_from(&mut request).await.expect("request");
            // Answer an invoke this channel is no longer waiting on, then the
            // one it is. Both are already queued when the read looks.
            device.send_to(&complex_ack(99), peer).await.expect("stale");
            device.send_to(&complex_ack(1), peer).await.expect("reply");
        });

        let mut channel = BacnetChannel::new(
            BacnetParamsConfig {
                host: address.ip().to_string(),
                port: address.port(),
                timeout_ms: 2_000,
                local_port: 0,
            },
            1,
            vec![BacnetPointConfig {
                point_id: 5,
                point_type: PointType::Telemetry,
                mapping: present_value(),
                scale: 1.0,
                offset: 0.0,
                reverse: false,
            }],
        )
        .expect("channel");
        channel.connect().await.expect("connect");

        let result = tokio::time::timeout(Duration::from_secs(5), channel.poll_once())
            .await
            .expect("poll must not hang");
        assert!(
            result.failures.is_empty(),
            "failures: {:?}",
            result.failures
        );
        assert_eq!(
            result.data.iter().next().expect("point").value,
            Value::Float(12.5)
        );
        assert!(
            channel
                .diagnostics
                .last_error()
                .is_some_and(|error| error.contains("answers no pending request")),
            "the discarded datagram must be visible to an operator"
        );
    }

    #[test]
    fn point_projection_matches_the_shared_transform() {
        // Every adapter has to agree with TransformConfig bit for bit; a fused
        // multiply-add rounds once where the shared transform rounds twice, so
        // the same point read through two adapters would not compare equal.
        let point = BacnetPointConfig {
            point_id: 1,
            point_type: PointType::Telemetry,
            mapping: present_value(),
            scale: 0.1,
            offset: -0.01,
            reverse: false,
        };
        assert_ne!(
            0.1_f64.mul_add(point.scale, point.offset),
            TransformConfig::linear(point.scale, point.offset).apply(0.1),
            "pick constants that actually distinguish the two formulas"
        );
        assert_eq!(
            transform_value(Value::Float(0.1), &point).expect("value"),
            Value::Float(TransformConfig::linear(point.scale, point.offset).apply(0.1))
        );
    }

    fn present_value() -> BacnetPointMapping {
        BacnetPointMapping {
            object_type: 0,
            object_instance: 7,
            property_id: 85,
            array_index: None,
            priority: None,
        }
    }

    #[test]
    fn read_property_request_uses_bacnet_context_tags() {
        let request = encode_read_property(9, &present_value()).expect("request");
        assert_eq!(
            request,
            [0x00, 0x05, 0x09, 0x0c, 0x0c, 0, 0, 0, 7, 0x19, 85]
        );
    }

    #[test]
    fn complex_ack_decodes_real_present_value() {
        let apdu = [
            0x30, 9, 0x0c, 0x0c, 0, 0, 0, 7, 0x19, 85, 0x3e, 0x44, 0x41, 0x48, 0, 0, 0x3f,
        ];
        let value = decode_read_property_ack(&apdu, 9).expect("value");
        assert_eq!(value, Value::Float(12.5));
    }

    #[test]
    fn complex_ack_decodes_instances_whose_identifier_contains_the_opening_tag() {
        // Scanning for the 0x3e opening tag collides with the object
        // identifier that precedes it. Instance 62 encodes as `00 00 00 3e`,
        // which made an ordinary read fail; instance 0x3E1234 puts 0x3e in the
        // first identifier byte, which decoded the wrong bytes and produced a
        // Bool that entered live state as a valid reading.
        for instance in [62_u32, 0x003E_1234] {
            let mut apdu = vec![0x30, 9, 0x0c, 0x0c];
            apdu.extend_from_slice(&instance.to_be_bytes());
            apdu.extend_from_slice(&[0x19, 85, 0x3e, 0x44, 0x41, 0x48, 0, 0, 0x3f]);
            let value = decode_read_property_ack(&apdu, 9)
                .unwrap_or_else(|error| panic!("instance {instance}: {error:?}"));
            assert_eq!(value, Value::Float(12.5), "instance {instance}");
        }
    }

    #[test]
    fn complex_ack_with_array_index_decodes_the_property_value() {
        let apdu = [
            0x30, 9, 0x0c, 0x0c, 0, 0, 0, 62, 0x19, 85, 0x29, 3, 0x3e, 0x44, 0x41, 0x48, 0, 0, 0x3f,
        ];
        let value = decode_read_property_ack(&apdu, 9).expect("value");
        assert_eq!(value, Value::Float(12.5));
    }

    #[test]
    fn bvlc_parser_skips_source_address() {
        let packet = [
            0x81, 0x0a, 0, 16, 1, 0x08, 0, 1, 2, 10, 11, 0x30, 9, 0x0c, 0x3e, 0x10,
        ];
        assert_eq!(decode_bvlc_apdu(&packet).expect("APDU"), &packet[11..]);
    }

    #[test]
    fn point_mapping_rejects_out_of_range_fields() {
        assert!(
            parse_point_mapping(
                &serde_json::json!({
                    "object_type": 1024,
                    "object_instance": 1,
                    "property_id": 85
                }),
                PointType::Telemetry
            )
            .is_err()
        );
        assert!(
            parse_point_mapping(
                &serde_json::json!({
                    "object_type": 0,
                    "object_instance": 1,
                    "property_id": 85,
                    "priority": 17
                }),
                PointType::Control
            )
            .is_err()
        );
    }

    #[test]
    fn binary_output_write_uses_bacnet_enumerated_value() {
        let mapping = BacnetPointMapping {
            object_type: 4,
            object_instance: 2,
            property_id: 85,
            array_index: None,
            priority: Some(8),
        };
        let request = encode_write_property(1, &mapping, 1.0).expect("write request");
        assert!(
            request
                .windows(4)
                .any(|window| window == [0x3e, 0x91, 1, 0x3f])
        );
        assert!(request.ends_with(&[0x49, 8]));
    }
}
