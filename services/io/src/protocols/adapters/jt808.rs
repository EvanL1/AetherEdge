//! JT/T 808 authenticated vehicle terminal server adapter.
//!
//! Supports the 2013-style six-byte terminal identity and the 2019 versioned
//! ten-byte identity header. Each connection must register or authenticate
//! with a configured terminal token before location reports are accepted.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use aether_core::PointType;
use async_trait::async_trait;
use bytes::{Buf, BytesMut};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::TransformConfig;
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
    data_event_channel,
};
use crate::protocols::runtime::ChannelRuntime;

use super::tcp_terminal_server::{
    MAX_CONNECTIONS, ReadDeadlines, ReadOutcome, ServerContext, read_bounded, run_accept_loop,
};

const DELIMITER: u8 = 0x7e;
const ESCAPE: u8 = 0x7d;
const MSG_TERMINAL_GENERAL_RESPONSE: u16 = 0x0001;
const MSG_HEARTBEAT: u16 = 0x0002;
const MSG_REGISTER: u16 = 0x0100;
const MSG_AUTHENTICATE: u16 = 0x0102;
const MSG_LOCATION: u16 = 0x0200;
const MSG_PLATFORM_GENERAL_RESPONSE: u16 = 0x8001;
const MSG_REGISTER_RESPONSE: u16 = 0x8100;
const RESULT_SUCCESS: u8 = 0;
const RESULT_FAILURE: u8 = 1;
const DEFAULT_BIND: &str = "127.0.0.1:6808";
const MAX_FRAME_BYTES: usize = 8_192;

fn default_bind() -> String {
    DEFAULT_BIND.to_owned()
}

/// JT/T 808 server parameters. Map keys are 12- or 20-digit terminal IDs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Jt808ParamsConfig {
    #[serde(default = "default_bind")]
    pub(crate) bind: String,
    pub(crate) auth_tokens: HashMap<String, String>,
}

impl Jt808ParamsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        self.bind.parse::<std::net::SocketAddr>().map_err(|error| {
            GatewayError::Config(format!("invalid JT/T 808 bind address: {error}"))
        })?;
        // A channel carries one device connection, and a point on this channel
        // names a location field with no terminal beside it. Two configured
        // terminals would therefore write the same points, and the later report
        // would silently overwrite the earlier one with another vehicle's
        // readings. The field stays a map so per-point terminal selection can
        // relax this later without breaking configurations written today.
        if self.auth_tokens.len() != 1 {
            return Err(GatewayError::Config(format!(
                "JT/T 808 auth_tokens must name exactly one terminal, found {}; \
                 points on a channel carry no terminal selector, so give each \
                 terminal its own channel",
                self.auth_tokens.len()
            )));
        }
        for (terminal, token) in &self.auth_tokens {
            if !matches!(terminal.len(), 12 | 20)
                || !terminal.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(GatewayError::Config(format!(
                    "JT/T 808 terminal '{terminal}' must contain 12 or 20 decimal digits"
                )));
            }
            if token.is_empty() || token.len() > 128 || !token.is_ascii() {
                return Err(GatewayError::Config(format!(
                    "JT/T 808 terminal '{terminal}' token must contain 1..=128 ASCII bytes"
                )));
            }
        }
        Ok(())
    }
}

/// Supported location-report field selectors.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Jt808Field {
    AlarmFlags,
    StatusFlags,
    Latitude,
    Longitude,
    AltitudeM,
    SpeedKmh,
    DirectionDeg,
    AccOn,
    Positioned,
    LatitudeSouth,
    LongitudeWest,
    OutOfService,
    OilCut,
    CircuitCut,
    DoorLocked,
    MileageKm,
    FuelLiters,
    RecorderSpeedKmh,
    AlarmEventId,
    TemperatureC,
    SignalStatus,
    IoStatus,
    AnalogAd0,
    AnalogAd1,
    WirelessSignal,
    GnssSatellites,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub(crate) struct Jt808PointMapping {
    pub(crate) field: Jt808Field,
}

pub(crate) fn parse_point_mapping(
    mapping: &serde_json::Value,
    point_type: PointType,
) -> Result<Jt808PointMapping> {
    if !matches!(point_type, PointType::Telemetry | PointType::Signal) {
        return Err(GatewayError::Config(
            "JT/T 808 location reports only map telemetry and signal points".to_owned(),
        ));
    }
    serde_json::from_value(mapping.clone())
        .map_err(|error| GatewayError::Config(format!("invalid JT/T 808 mapping: {error}")))
}

#[derive(Debug, Clone)]
pub(crate) struct Jt808PointConfig {
    pub(crate) point_id: u32,
    pub(crate) point_type: PointType,
    pub(crate) mapping: Jt808PointMapping,
    pub(crate) scale: f64,
    pub(crate) offset: f64,
    pub(crate) reverse: bool,
}

#[derive(Debug, Clone)]
struct Jt808Header {
    message_id: u16,
    terminal_id: String,
    sequence: u16,
    version: Option<u8>,
    encrypted: u8,
    subpackaged: bool,
}

#[derive(Debug)]
struct Jt808Message {
    header: Jt808Header,
    body: Vec<u8>,
}

/// Event-driven authenticated JT/T 808 server.
pub struct Jt808Channel {
    channel_id: u32,
    params: Jt808ParamsConfig,
    points: Arc<Vec<Jt808PointConfig>>,
    listener: Option<TcpListener>,
    event_tx: DataEventSender,
    event_rx: Option<DataEventReceiver>,
    state: Arc<AtomicU8>,
    diagnostics: Arc<AtomicDiagnostics>,
    cancellation: CancellationToken,
    server_handle: Option<JoinHandle<()>>,
    deadlines: ReadDeadlines,
}

impl Jt808Channel {
    pub(crate) fn new(
        params: Jt808ParamsConfig,
        channel_id: u32,
        points: Vec<Jt808PointConfig>,
    ) -> Result<Self> {
        params.validate()?;
        let (event_tx, event_rx) = data_event_channel();
        Ok(Self {
            channel_id,
            params,
            points: Arc::new(points),
            listener: None,
            event_tx,
            event_rx: Some(event_rx),
            state: Arc::new(AtomicU8::new(ConnectionState::Disconnected.into())),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            cancellation: CancellationToken::new(),
            server_handle: None,
            deadlines: ReadDeadlines::DEFAULT,
        })
    }

    fn set_state(&self, state: ConnectionState) {
        self.state.store(state.into(), Ordering::SeqCst);
        let _ = self.event_tx.try_send(DataEvent::ConnectionChanged(state));
    }

    /// Cancels the accept loop, waits for it to exit, and releases the socket.
    async fn release_listener(&mut self) {
        self.cancellation.cancel();
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.await;
        }
        self.listener = None;
    }

    async fn run_server(
        listener: TcpListener,
        auth_tokens: Arc<HashMap<String, String>>,
        points: Arc<Vec<Jt808PointConfig>>,
        context: ServerContext,
        cancellation: CancellationToken,
        deadlines: ReadDeadlines,
    ) {
        let event_tx = context.event_tx.clone();
        let diagnostics = Arc::clone(&context.diagnostics);
        let accept_cancellation = cancellation.clone();
        run_accept_loop(listener, context, accept_cancellation, move |stream| {
            let auth_tokens = Arc::clone(&auth_tokens);
            let points = Arc::clone(&points);
            let event_tx = event_tx.clone();
            let diagnostics = Arc::clone(&diagnostics);
            let cancellation = cancellation.clone();
            async move {
                if let Err(error) = handle_connection(
                    stream,
                    &auth_tokens,
                    &points,
                    &event_tx,
                    &diagnostics,
                    &cancellation,
                    deadlines,
                )
                .await
                {
                    diagnostics.record_error(error.to_string());
                    let _ = event_tx.try_send(DataEvent::Error(error.to_string()));
                }
            }
        })
        .await;
    }
}

impl HasMetadata for Jt808Channel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "jt808",
            display_name: "JT/T 808",
            description: "Authenticated JT/T 808 vehicle positioning terminal TCP server",
            is_recommended: true,
            example_config: serde_json::json!({
                "bind": DEFAULT_BIND,
                "auth_tokens": {"013800138000": "replace-with-device-token"}
            }),
            parameters: vec![
                ParameterMetadata::optional(
                    "bind",
                    "Listen address",
                    "TCP listen socket; defaults to loopback",
                    ParameterType::String,
                    serde_json::json!(DEFAULT_BIND),
                )
                .with_min_length(1),
                ParameterMetadata::required(
                    "auth_tokens",
                    "Terminal token",
                    "The one terminal ID this channel accepts, mapped to its \
                     pre-provisioned token; give each terminal its own channel",
                    ParameterType::Object,
                ),
            ],
        }
    }
}

#[async_trait]
impl ChannelRuntime for Jt808Channel {
    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        self.set_state(ConnectionState::Connecting);
        // Release the socket this channel may still be listening on. Binding
        // first collides with our own accept loop, and the resulting AddrInUse
        // strands the channel in Connecting, where every later reconnect
        // attempt repeats the same collision against the same live listener.
        self.release_listener().await;
        let bound = TcpListener::bind(&self.params.bind).await;
        let listener = match bound {
            Ok(listener) => listener,
            Err(error) => {
                self.set_state(ConnectionState::Error);
                return Err(GatewayError::Connection(error.to_string()));
            },
        };
        self.listener = Some(listener);
        self.cancellation = CancellationToken::new();
        self.set_state(ConnectionState::Connected);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.release_listener().await;
        self.set_state(ConnectionState::Disconnected);
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        PollResult::success(DataBatch::new())
    }

    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        self.event_rx.take()
    }

    async fn start_events(&mut self) -> Result<()> {
        if self
            .server_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Ok(());
        }
        let listener = self.listener.take().ok_or(GatewayError::NotConnected)?;
        self.server_handle = Some(tokio::spawn(Self::run_server(
            listener,
            Arc::new(self.params.auth_tokens.clone()),
            Arc::clone(&self.points),
            ServerContext {
                state: Arc::clone(&self.state),
                event_tx: self.event_tx.clone(),
                diagnostics: Arc::clone(&self.diagnostics),
                max_connections: MAX_CONNECTIONS,
            },
            self.cancellation.clone(),
            self.deadlines,
        )));
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        self.cancellation.cancel();
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.await;
        }
        Ok(())
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "jt808".to_owned(),
            connection_state: self.connection_state(),
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: serde_json::json!({
                "channel_id": self.channel_id,
                "bind": self.params.bind,
                "terminal_count": self.params.auth_tokens.len(),
                "point_count": self.points.len()
            }),
        })
    }

    fn connection_state(&self) -> ConnectionState {
        ConnectionState::from(self.state.load(Ordering::SeqCst))
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    auth_tokens: &HashMap<String, String>,
    points: &[Jt808PointConfig],
    event_tx: &DataEventSender,
    diagnostics: &AtomicDiagnostics,
    cancellation: &CancellationToken,
    deadlines: ReadDeadlines,
) -> Result<()> {
    let mut buffer = BytesMut::with_capacity(4_096);
    let mut authenticated_terminal: Option<String> = None;
    let mut platform_sequence = 0_u16;
    loop {
        let outcome = read_bounded(
            &mut stream,
            &mut buffer,
            deadlines,
            authenticated_terminal.is_some(),
            cancellation,
        )
        .await?;
        match outcome {
            ReadOutcome::Bytes => {},
            ReadOutcome::Closed => return Ok(()),
            // A terminal that authenticated and then went quiet is an ordinary
            // vehicle losing coverage. One that never authenticated is a peer
            // occupying a connection slot it never earned, so say so.
            ReadOutcome::TimedOut => {
                return match authenticated_terminal {
                    Some(_) => Ok(()),
                    None => Err(GatewayError::Protocol(
                        "JT/T 808 terminal held a connection without authenticating".to_owned(),
                    )),
                };
            },
        }
        while let Some(message) = decode_next_message(&mut buffer)? {
            if message.header.encrypted != 0 {
                return Err(GatewayError::Unsupported(format!(
                    "JT/T 808 encryption mode {} is not configured",
                    message.header.encrypted
                )));
            }
            if message.header.subpackaged {
                return Err(GatewayError::Unsupported(
                    "JT/T 808 subpackage reassembly is not configured".to_owned(),
                ));
            }
            let Some(token) = auth_tokens.get(&message.header.terminal_id) else {
                return Err(GatewayError::Protocol(format!(
                    "JT/T 808 terminal {} is not configured",
                    message.header.terminal_id
                )));
            };
            platform_sequence = platform_sequence.wrapping_add(1);
            let response = match message.header.message_id {
                // Registration acknowledges the terminal but never returns the
                // token. A terminal id is BCD-decoded straight off the wire and
                // is a printed, often sequential SIM-style number, so echoing
                // the configured secret here would hand it to any peer that
                // names a configured id, and the authentication gate below
                // would then admit that peer. Provision the token out of band.
                MSG_REGISTER => {
                    encode_register_response(&message.header, platform_sequence, RESULT_SUCCESS)?
                },
                MSG_AUTHENTICATE => {
                    if !tokens_match(&message.body, token.as_bytes()) {
                        // Answer once, then close. An unconfigured terminal id
                        // is already dropped on sight, so leaving the socket
                        // open after a bad token would make a known id the one
                        // thing on this server that answers guesses forever.
                        let response = encode_general_response(
                            &message.header,
                            platform_sequence,
                            RESULT_FAILURE,
                        )?;
                        stream
                            .write_all(&response)
                            .await
                            .map_err(GatewayError::Io)?;
                        diagnostics.inc_write();
                        return Err(GatewayError::Protocol(format!(
                            "JT/T 808 terminal {} supplied an incorrect token",
                            message.header.terminal_id
                        )));
                    }
                    authenticated_terminal = Some(message.header.terminal_id.clone());
                    encode_general_response(&message.header, platform_sequence, RESULT_SUCCESS)?
                },
                MSG_HEARTBEAT | MSG_LOCATION => {
                    if authenticated_terminal.as_deref()
                        != Some(message.header.terminal_id.as_str())
                    {
                        return Err(GatewayError::Protocol(
                            "JT/T 808 terminal sent data before authentication".to_owned(),
                        ));
                    }
                    if message.header.message_id == MSG_LOCATION {
                        let values = decode_location(&message.body)?;
                        let batch = project_points(points, &values, diagnostics);
                        if !batch.is_empty() {
                            diagnostics.add_read(batch.len() as u64);
                            let _ = event_tx.try_send(DataEvent::DataUpdate(batch));
                        }
                    }
                    encode_general_response(&message.header, platform_sequence, RESULT_SUCCESS)?
                },
                MSG_TERMINAL_GENERAL_RESPONSE => continue,
                _ => encode_general_response(&message.header, platform_sequence, RESULT_FAILURE)?,
            };
            stream
                .write_all(&response)
                .await
                .map_err(GatewayError::Io)?;
            diagnostics.inc_write();
        }
    }
}

fn decode_next_message(buffer: &mut BytesMut) -> Result<Option<Jt808Message>> {
    let Some(start) = buffer.iter().position(|byte| *byte == DELIMITER) else {
        buffer.clear();
        return Ok(None);
    };
    if start > 0 {
        buffer.advance(start);
    }
    let Some(relative_end) = buffer[1..].iter().position(|byte| *byte == DELIMITER) else {
        if buffer.len() > MAX_FRAME_BYTES {
            return Err(GatewayError::InvalidResponse(
                "JT/T 808 frame exceeds configured maximum".to_owned(),
            ));
        }
        return Ok(None);
    };
    let end = relative_end + 1;
    let escaped = buffer[1..end].to_vec();
    buffer.advance(end + 1);
    let decoded = unescape(&escaped)?;
    decode_message(&decoded).map(Some)
}

fn decode_message(bytes: &[u8]) -> Result<Jt808Message> {
    if bytes.len() < 13 {
        return Err(GatewayError::InvalidResponse(
            "JT/T 808 message is too short".to_owned(),
        ));
    }
    let checksum_index = bytes.len() - 1;
    let checksum = bytes[..checksum_index]
        .iter()
        .fold(0_u8, |checksum, byte| checksum ^ *byte);
    if checksum != bytes[checksum_index] {
        return Err(GatewayError::InvalidResponse(
            "JT/T 808 checksum mismatch".to_owned(),
        ));
    }
    let message_id = u16::from_be_bytes([bytes[0], bytes[1]]);
    let properties = u16::from_be_bytes([bytes[2], bytes[3]]);
    let body_length = usize::from(properties & 0x03ff);
    let encrypted = ((properties >> 10) & 0x07) as u8;
    let subpackaged = properties & 0x2000 != 0;
    let versioned = properties & 0x4000 != 0;
    let (version, terminal_start, terminal_length) = if versioned {
        (Some(bytes[4]), 5, 10)
    } else {
        (None, 4, 6)
    };
    let sequence_start = terminal_start + terminal_length;
    let base_header_length = sequence_start + 2;
    let header_length = base_header_length + if subpackaged { 4 } else { 0 };
    if bytes.len() != header_length + body_length + 1 {
        return Err(GatewayError::InvalidResponse(
            "JT/T 808 body length mismatch".to_owned(),
        ));
    }
    let terminal_id = decode_bcd(&bytes[terminal_start..sequence_start])?;
    let sequence = u16::from_be_bytes([bytes[sequence_start], bytes[sequence_start + 1]]);
    Ok(Jt808Message {
        header: Jt808Header {
            message_id,
            terminal_id,
            sequence,
            version,
            encrypted,
            subpackaged,
        },
        body: bytes[header_length..checksum_index].to_vec(),
    })
}

fn unescape(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != ESCAPE {
            output.push(bytes[cursor]);
            cursor += 1;
            continue;
        }
        let escaped = *bytes
            .get(cursor + 1)
            .ok_or_else(|| GatewayError::InvalidResponse("truncated JT/T 808 escape".to_owned()))?;
        output.push(match escaped {
            0x01 => ESCAPE,
            0x02 => DELIMITER,
            _ => {
                return Err(GatewayError::InvalidResponse(
                    "invalid JT/T 808 escape sequence".to_owned(),
                ));
            },
        });
        cursor += 2;
    }
    Ok(output)
}

fn escape_and_frame(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len() + 2);
    output.push(DELIMITER);
    for byte in bytes {
        match *byte {
            DELIMITER => output.extend_from_slice(&[ESCAPE, 0x02]),
            ESCAPE => output.extend_from_slice(&[ESCAPE, 0x01]),
            value => output.push(value),
        }
    }
    output.push(DELIMITER);
    output
}

fn encode_message(
    message_id: u16,
    terminal: &Jt808Header,
    sequence: u16,
    body: &[u8],
) -> Result<Vec<u8>> {
    let body_length = u16::try_from(body.len())
        .map_err(|_| GatewayError::InvalidData("JT/T 808 body exceeds u16 length".to_owned()))?;
    if body_length > 0x03ff {
        return Err(GatewayError::InvalidData(
            "JT/T 808 body exceeds non-subpackaged limit".to_owned(),
        ));
    }
    let mut message = Vec::new();
    message.extend_from_slice(&message_id.to_be_bytes());
    let properties = body_length
        | if terminal.version.is_some() {
            0x4000
        } else {
            0
        };
    message.extend_from_slice(&properties.to_be_bytes());
    if let Some(version) = terminal.version {
        message.push(version);
    }
    message.extend_from_slice(&encode_bcd(&terminal.terminal_id)?);
    message.extend_from_slice(&sequence.to_be_bytes());
    message.extend_from_slice(body);
    let checksum = message.iter().fold(0_u8, |checksum, byte| checksum ^ *byte);
    message.push(checksum);
    Ok(escape_and_frame(&message))
}

fn encode_general_response(
    terminal: &Jt808Header,
    platform_sequence: u16,
    result: u8,
) -> Result<Vec<u8>> {
    let mut body = Vec::with_capacity(5);
    body.extend_from_slice(&terminal.sequence.to_be_bytes());
    body.extend_from_slice(&terminal.message_id.to_be_bytes());
    body.push(result);
    encode_message(
        MSG_PLATFORM_GENERAL_RESPONSE,
        terminal,
        platform_sequence,
        &body,
    )
}

/// Encodes a registration response that carries no credential.
///
/// The parameter that used to append the configured token is gone on purpose:
/// keeping it would leave a callable path that discloses an edge-local secret
/// to an unauthenticated peer.
fn encode_register_response(
    terminal: &Jt808Header,
    platform_sequence: u16,
    result: u8,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    body.extend_from_slice(&terminal.sequence.to_be_bytes());
    body.push(result);
    encode_message(MSG_REGISTER_RESPONSE, terminal, platform_sequence, &body)
}

fn decode_bcd(bytes: &[u8]) -> Result<String> {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let high = byte >> 4;
        let low = byte & 0x0f;
        if high > 9 || low > 9 {
            return Err(GatewayError::InvalidResponse(
                "JT/T 808 terminal ID contains invalid BCD".to_owned(),
            ));
        }
        output.push(char::from(b'0' + high));
        output.push(char::from(b'0' + low));
    }
    Ok(output)
}

fn encode_bcd(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(GatewayError::InvalidData(
            "JT/T 808 terminal ID must contain an even number of digits".to_owned(),
        ));
    }
    Ok(value
        .as_bytes()
        .chunks_exact(2)
        .map(|digits| ((digits[0] - b'0') << 4) | (digits[1] - b'0'))
        .collect())
}

fn decode_location(body: &[u8]) -> Result<HashMap<Jt808Field, f64>> {
    if body.len() < 28 {
        return Err(GatewayError::InvalidResponse(
            "JT/T 808 location report is too short".to_owned(),
        ));
    }
    let alarm = be_u32(&body[0..4]);
    let status = be_u32(&body[4..8]);
    let mut values = HashMap::new();
    values.insert(Jt808Field::AlarmFlags, f64::from(alarm));
    values.insert(Jt808Field::StatusFlags, f64::from(status));
    values.insert(
        Jt808Field::Latitude,
        f64::from(be_u32(&body[8..12])) / 1_000_000.0,
    );
    values.insert(
        Jt808Field::Longitude,
        f64::from(be_u32(&body[12..16])) / 1_000_000.0,
    );
    values.insert(Jt808Field::AltitudeM, f64::from(be_u16(&body[16..18])));
    values.insert(
        Jt808Field::SpeedKmh,
        f64::from(be_u16(&body[18..20])) / 10.0,
    );
    values.insert(Jt808Field::DirectionDeg, f64::from(be_u16(&body[20..22])));
    insert_status(&mut values, Jt808Field::AccOn, status, 0);
    insert_status(&mut values, Jt808Field::Positioned, status, 1);
    insert_status(&mut values, Jt808Field::LatitudeSouth, status, 2);
    insert_status(&mut values, Jt808Field::LongitudeWest, status, 3);
    insert_status(&mut values, Jt808Field::OutOfService, status, 4);
    insert_status(&mut values, Jt808Field::OilCut, status, 10);
    insert_status(&mut values, Jt808Field::CircuitCut, status, 11);
    insert_status(&mut values, Jt808Field::DoorLocked, status, 12);

    let mut cursor = 28;
    while cursor < body.len() {
        if cursor + 2 > body.len() {
            return Err(GatewayError::InvalidResponse(
                "truncated JT/T 808 location extension".to_owned(),
            ));
        }
        let id = body[cursor];
        let length = usize::from(body[cursor + 1]);
        cursor += 2;
        let data = body.get(cursor..cursor + length).ok_or_else(|| {
            GatewayError::InvalidResponse("truncated JT/T 808 extension data".to_owned())
        })?;
        decode_extension(id, data, &mut values)?;
        cursor += length;
    }
    Ok(values)
}

fn decode_extension(id: u8, data: &[u8], values: &mut HashMap<Jt808Field, f64>) -> Result<()> {
    match (id, data.len()) {
        (0x01, 4) => {
            values.insert(Jt808Field::MileageKm, f64::from(be_u32(data)) / 10.0);
        },
        (0x02, 2) => {
            values.insert(Jt808Field::FuelLiters, f64::from(be_u16(data)) / 10.0);
        },
        (0x03, 2) => {
            values.insert(Jt808Field::RecorderSpeedKmh, f64::from(be_u16(data)) / 10.0);
        },
        (0x04, 2) => {
            values.insert(Jt808Field::AlarmEventId, f64::from(be_u16(data)));
        },
        (0x06, 2) => {
            values.insert(
                Jt808Field::TemperatureC,
                f64::from(i16::from_be_bytes([data[0], data[1]])) / 10.0,
            );
        },
        (0x25, 4) => {
            values.insert(Jt808Field::SignalStatus, f64::from(be_u32(data)));
        },
        (0x2a, 2) => {
            values.insert(Jt808Field::IoStatus, f64::from(be_u16(data)));
        },
        (0x2b, 4) => {
            values.insert(Jt808Field::AnalogAd0, f64::from(be_u16(&data[..2])) / 100.0);
            values.insert(Jt808Field::AnalogAd1, f64::from(be_u16(&data[2..])) / 100.0);
        },
        (0x30, 1) => {
            values.insert(Jt808Field::WirelessSignal, f64::from(data[0]));
        },
        (0x31, 1) => {
            values.insert(Jt808Field::GnssSatellites, f64::from(data[0]));
        },
        _ => {},
    }
    Ok(())
}

fn insert_status(values: &mut HashMap<Jt808Field, f64>, field: Jt808Field, status: u32, bit: u8) {
    values.insert(field, f64::from((status >> bit) & 1));
}

/// Projects a decoded location report, skipping points that cannot be
/// represented rather than failing the whole report.
///
/// One misconfigured scale used to abort the batch, which dropped the vehicle's
/// TCP connection; the terminal reconnected, reported again, and lost the
/// connection again, so a single bad point silently cost every other point on
/// that vehicle.
fn project_points(
    points: &[Jt808PointConfig],
    values: &HashMap<Jt808Field, f64>,
    diagnostics: &AtomicDiagnostics,
) -> DataBatch {
    let mut batch = DataBatch::with_capacity(points.len());
    for point in points {
        let Some(raw) = values.get(&point.mapping.field) else {
            continue;
        };
        let value = if point.point_type == PointType::Signal {
            Value::Bool(if point.reverse {
                *raw == 0.0
            } else {
                *raw != 0.0
            })
        } else {
            let transformed = TransformConfig::linear(point.scale, point.offset).apply(*raw);
            if !transformed.is_finite() {
                diagnostics.record_error(format!(
                    "JT/T 808 point {} transformed to a non-finite value",
                    point.point_id
                ));
                continue;
            }
            Value::Float(transformed)
        };
        batch.add(DataPoint::new(point.point_id, point.point_type, value));
    }
    batch
}

/// Compares a supplied token against the configured one in constant time.
///
/// A short-circuiting comparison lets a peer recover the token one byte at a
/// time by timing how quickly each guess is rejected.
fn tokens_match(supplied: &[u8], expected: &[u8]) -> bool {
    let mut difference = u8::from(supplied.len() != expected.len());
    for (index, byte) in expected.iter().enumerate() {
        difference |= byte ^ supplied.get(index).copied().unwrap_or(0);
    }
    difference == 0
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::time::{Duration, timeout};

    #[test]
    fn token_comparison_examines_every_configured_byte() {
        // Constant time is not observable from a test; what is testable is that
        // the comparison still answers correctly at both ends of the token and
        // on a length mismatch.
        let expected = b"provisioned-token";
        assert!(tokens_match(expected, expected));
        assert!(!tokens_match(b"Xrovisioned-token", expected));
        assert!(!tokens_match(b"provisioned-tokeX", expected));
        assert!(!tokens_match(b"provisioned-toke", expected));
        assert!(!tokens_match(b"provisioned-tokenX", expected));
        assert!(!tokens_match(b"", expected));
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
        let points = vec![Jt808PointConfig {
            point_id: 3,
            point_type: PointType::Telemetry,
            mapping: Jt808PointMapping {
                field: Jt808Field::SpeedKmh,
            },
            scale,
            offset,
            reverse: false,
        }];
        let values = HashMap::from([(Jt808Field::SpeedKmh, raw)]);

        let batch = project_points(&points, &values, &AtomicDiagnostics::new());

        assert_eq!(
            batch.iter().next().expect("point").value,
            Value::Float(TransformConfig::linear(scale, offset).apply(raw))
        );
    }

    #[test]
    fn one_unrepresentable_point_does_not_cost_the_rest_of_the_report() {
        // Failing the batch used to drop the vehicle's connection, so a single
        // misconfigured scale silently cost every other point on that vehicle.
        let point = |point_id, scale, offset| Jt808PointConfig {
            point_id,
            point_type: PointType::Telemetry,
            mapping: Jt808PointMapping {
                field: Jt808Field::SpeedKmh,
            },
            scale,
            offset,
            reverse: false,
        };
        let points = vec![point(1, f64::MAX, f64::MAX), point(2, 1.0, 0.0)];
        let values = HashMap::from([(Jt808Field::SpeedKmh, 88.6)]);
        let diagnostics = AtomicDiagnostics::new();

        let batch = project_points(&points, &values, &diagnostics);

        let survivor = batch.iter().next().expect("the healthy point survives");
        assert_eq!(survivor.id, 2);
        assert_eq!(survivor.value, Value::Float(88.6));
        assert_eq!(batch.len(), 1);
        assert_eq!(diagnostics.error_count(), 1);
    }

    #[tokio::test]
    async fn a_terminal_that_guesses_a_token_is_answered_once_and_closed() {
        // An unconfigured terminal id is already dropped on sight. Leaving the
        // socket open after a bad token made a known id the one thing on this
        // server that answers guesses for as long as the peer cares to ask.
        let bind = free_port().await;
        let mut channel = channel_on(&bind);
        channel.connect().await.expect("connect");
        channel.start_events().await.expect("start events");

        let identity = Jt808Header {
            message_id: MSG_AUTHENTICATE,
            terminal_id: "013800138000".to_owned(),
            sequence: 1,
            version: None,
            encrypted: 0,
            subpackaged: false,
        };
        let mut terminal = TcpStream::connect(&bind).await.expect("terminal");
        let guess = encode_message(MSG_AUTHENTICATE, &identity, 1, b"wrong-token").expect("guess");
        terminal.write_all(&guess).await.expect("send guess");

        let mut answer = Vec::new();
        timeout(Duration::from_secs(5), terminal.read_to_end(&mut answer))
            .await
            .expect("a rejected terminal must be closed, not left to guess again")
            .expect("read");

        let decoded = unescape(&answer[1..answer.len() - 1]).expect("unescape");
        let response = decode_message(&decoded).expect("platform response");
        assert_eq!(response.header.message_id, MSG_PLATFORM_GENERAL_RESPONSE);
        assert_eq!(
            response.body.last(),
            Some(&RESULT_FAILURE),
            "the terminal still learns why it was refused"
        );
        assert!(
            channel
                .diagnostics
                .last_error()
                .is_some_and(|error| error.contains("incorrect token")),
            "a token guess must be visible to an operator"
        );

        channel.disconnect().await.expect("disconnect");
    }

    /// Binds an ephemeral port and hands it back so a channel can claim it.
    async fn free_port() -> String {
        let probe = TcpListener::bind("127.0.0.1:0").await.expect("probe");
        let address = probe.local_addr().expect("address");
        drop(probe);
        address.to_string()
    }

    fn channel_on(bind: &str) -> Jt808Channel {
        Jt808Channel::new(
            Jt808ParamsConfig {
                bind: bind.to_owned(),
                auth_tokens: HashMap::from([(
                    "013800138000".to_owned(),
                    "provisioned-token".to_owned(),
                )]),
            },
            1,
            vec![Jt808PointConfig {
                point_id: 8,
                point_type: PointType::Telemetry,
                mapping: Jt808PointMapping {
                    field: Jt808Field::SpeedKmh,
                },
                scale: 1.0,
                offset: 0.0,
                reverse: false,
            }],
        )
        .expect("channel")
    }

    fn location_body() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0_u32.to_be_bytes());
        body.extend_from_slice(&3_u32.to_be_bytes());
        body.extend_from_slice(&30_901_248_u32.to_be_bytes());
        body.extend_from_slice(&103_767_680_u32.to_be_bytes());
        body.extend_from_slice(&500_u16.to_be_bytes());
        body.extend_from_slice(&886_u16.to_be_bytes());
        body.extend_from_slice(&123_u16.to_be_bytes());
        // BCD collection time; a location report is 28 bytes before extensions.
        body.extend_from_slice(&[0x24, 1, 2, 3, 4, 5]);
        body
    }

    /// Authenticates over `bind` and reports one location, returning the socket
    /// so the caller keeps the connection open.
    async fn report_one_location(bind: &str) -> TcpStream {
        let identity = Jt808Header {
            message_id: MSG_AUTHENTICATE,
            terminal_id: "013800138000".to_owned(),
            sequence: 1,
            version: None,
            encrypted: 0,
            subpackaged: false,
        };
        let mut terminal = TcpStream::connect(bind).await.expect("terminal");
        let authentication =
            encode_message(MSG_AUTHENTICATE, &identity, 1, b"provisioned-token").expect("auth");
        terminal
            .write_all(&authentication)
            .await
            .expect("send auth");
        let location =
            encode_message(MSG_LOCATION, &identity, 2, &location_body()).expect("location");
        terminal.write_all(&location).await.expect("send location");
        terminal
    }

    async fn next_data_update(receiver: &mut DataEventReceiver) -> DataBatch {
        loop {
            let event = timeout(Duration::from_secs(5), receiver.recv())
                .await
                .expect("event timeout")
                .expect("event");
            if let DataEvent::DataUpdate(batch) = event {
                return batch;
            }
        }
    }

    fn header(version: Option<u8>) -> Jt808Header {
        Jt808Header {
            message_id: MSG_LOCATION,
            terminal_id: if version.is_some() {
                "00000000138001380000".to_owned()
            } else {
                "013800138000".to_owned()
            },
            sequence: 7,
            version,
            encrypted: 0,
            subpackaged: false,
        }
    }

    #[test]
    fn registration_response_never_carries_the_configured_token() {
        // A terminal id arrives BCD-decoded off the wire and is a printed,
        // often sequential SIM-style number. Returning the token here would
        // let any peer that names a configured id collect the credential and
        // then pass the authentication gate, making that gate ornamental.
        let mut header = header(None);
        header.message_id = MSG_REGISTER;
        header.sequence = 0x0007;
        let frame =
            encode_register_response(&header, 0x0011, RESULT_SUCCESS).expect("register response");
        let token = b"super-secret-token";

        assert!(
            frame
                .windows(token.len())
                .all(|window| window != token.as_slice()),
            "registration response must not embed a credential"
        );
        let message = decode_message(&frame[1..frame.len() - 1]).expect("decode");
        assert_eq!(message.body, [0x00, 0x07, RESULT_SUCCESS]);
    }

    #[test]
    fn escaped_2013_message_round_trips() {
        let framed =
            encode_message(MSG_HEARTBEAT, &header(None), 7, &[0x7e, 0x7d]).expect("message");
        let mut buffer = BytesMut::from(framed.as_slice());
        let message = decode_next_message(&mut buffer)
            .expect("decode")
            .expect("message");
        assert_eq!(message.header.terminal_id, "013800138000");
        assert_eq!(message.body, [0x7e, 0x7d]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn versioned_header_round_trips_with_twenty_digit_terminal() {
        let framed = encode_message(MSG_HEARTBEAT, &header(Some(1)), 9, &[]).expect("message");
        let mut buffer = BytesMut::from(framed.as_slice());
        let message = decode_next_message(&mut buffer)
            .expect("decode")
            .expect("message");
        assert_eq!(message.header.version, Some(1));
        assert_eq!(message.header.terminal_id, "00000000138001380000");
    }

    #[test]
    fn location_and_extensions_decode_engineering_units() {
        let mut body = Vec::new();
        body.extend_from_slice(&1_u32.to_be_bytes());
        body.extend_from_slice(&3_u32.to_be_bytes());
        body.extend_from_slice(&30_901_248_u32.to_be_bytes());
        body.extend_from_slice(&103_767_680_u32.to_be_bytes());
        body.extend_from_slice(&500_u16.to_be_bytes());
        body.extend_from_slice(&886_u16.to_be_bytes());
        body.extend_from_slice(&123_u16.to_be_bytes());
        body.extend_from_slice(&[0x24, 1, 2, 3, 4, 5]);
        body.extend_from_slice(&[0x01, 4]);
        body.extend_from_slice(&12_345_u32.to_be_bytes());
        body.extend_from_slice(&[0x06, 2]);
        body.extend_from_slice(&(-55_i16).to_be_bytes());
        let values = decode_location(&body).expect("location");
        assert_eq!(values[&Jt808Field::Latitude], 30.901248);
        assert_eq!(values[&Jt808Field::Longitude], 103.76768);
        assert_eq!(values[&Jt808Field::SpeedKmh], 88.6);
        assert_eq!(values[&Jt808Field::MileageKm], 1234.5);
        assert_eq!(values[&Jt808Field::TemperatureC], -5.5);
        assert_eq!(values[&Jt808Field::AccOn], 1.0);
        assert_eq!(values[&Jt808Field::Positioned], 1.0);
    }

    #[test]
    fn a_channel_accepts_exactly_one_terminal() {
        // Points name a location field and nothing else, so two configured
        // terminals would write the same point IDs and the later report would
        // overwrite the earlier one with another vehicle's readings.
        let config = |tokens: serde_json::Value| -> Jt808ParamsConfig {
            serde_json::from_value(serde_json::json!({ "auth_tokens": tokens })).expect("config")
        };

        let missing = config(serde_json::json!({}));
        assert_eq!(missing.bind, DEFAULT_BIND);
        assert!(missing.validate().is_err());

        config(serde_json::json!({"013800138000": "provisioned-token"}))
            .validate()
            .expect("one terminal per channel is the supported shape");

        let two = config(serde_json::json!({
            "013800138000": "provisioned-token",
            "013800138001": "another-token"
        }));
        let error = two.validate().expect_err("two terminals must be refused");
        assert!(
            error.to_string().contains("its own channel"),
            "the error must say what to do instead: {error}"
        );
    }

    #[tokio::test]
    async fn authenticated_tcp_terminal_emits_location_events() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let terminal = "013800138000";
        let token = "provisioned-token";
        let auth_tokens = HashMap::from([(terminal.to_owned(), token.to_owned())]);
        let points = vec![Jt808PointConfig {
            point_id: 8,
            point_type: PointType::Telemetry,
            mapping: Jt808PointMapping {
                field: Jt808Field::SpeedKmh,
            },
            scale: 1.0,
            offset: 0.0,
            reverse: false,
        }];
        let (event_tx, mut event_rx) = data_event_channel();
        let diagnostics = Arc::new(AtomicDiagnostics::new());
        let cancellation = CancellationToken::new();
        let server_cancellation = cancellation.clone();
        let server_diagnostics = Arc::clone(&diagnostics);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_connection(
                stream,
                &auth_tokens,
                &points,
                &event_tx,
                &server_diagnostics,
                &server_cancellation,
                ReadDeadlines::DEFAULT,
            )
            .await
        });

        let mut client = TcpStream::connect(address).await.expect("client");
        let identity = Jt808Header {
            message_id: MSG_AUTHENTICATE,
            terminal_id: terminal.to_owned(),
            sequence: 1,
            version: None,
            encrypted: 0,
            subpackaged: false,
        };
        let authentication = encode_message(MSG_AUTHENTICATE, &identity, 1, token.as_bytes())
            .expect("authentication");
        client
            .write_all(&authentication)
            .await
            .expect("send authentication");

        let mut body = Vec::new();
        body.extend_from_slice(&0_u32.to_be_bytes());
        body.extend_from_slice(&3_u32.to_be_bytes());
        body.extend_from_slice(&30_901_248_u32.to_be_bytes());
        body.extend_from_slice(&103_767_680_u32.to_be_bytes());
        body.extend_from_slice(&500_u16.to_be_bytes());
        body.extend_from_slice(&886_u16.to_be_bytes());
        body.extend_from_slice(&123_u16.to_be_bytes());
        body.extend_from_slice(&[0x24, 1, 2, 3, 4, 5]);
        let location = encode_message(MSG_LOCATION, &identity, 2, &body).expect("location");
        client.write_all(&location).await.expect("send location");

        let event = timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("event timeout")
            .expect("event");
        let DataEvent::DataUpdate(batch) = event else {
            panic!("expected data event");
        };
        let point = batch.iter().next().expect("point");
        assert_eq!(point.id, 8);
        assert_eq!(point.value, Value::Float(88.6));

        cancellation.cancel();
        drop(client);
        server.await.expect("server task").expect("connection");
        assert_eq!(diagnostics.read_count(), 1);
    }

    #[tokio::test]
    async fn connect_reclaims_the_port_its_own_listener_still_holds() {
        // The supervisor reconnects by calling connect() again. Binding before
        // releasing the live listener returns AddrInUse against ourselves, and
        // because connect() has already moved the channel out of Connected,
        // every later attempt collides the same way: a permanent zombie.
        let bind = free_port().await;
        let mut channel = channel_on(&bind);
        let mut events = channel.take_event_receiver().expect("receiver");
        channel.connect().await.expect("first connect");
        channel.start_events().await.expect("start events");

        channel
            .connect()
            .await
            .expect("connect must reclaim the port this channel is listening on");
        channel.start_events().await.expect("restart events");

        // Rebinding is only half the job: the rebuilt server has to serve.
        let terminal = report_one_location(&bind).await;
        let batch = next_data_update(&mut events).await;
        assert_eq!(
            batch.iter().next().expect("point").value,
            Value::Float(88.6)
        );

        drop(terminal);
        channel.disconnect().await.expect("disconnect");
    }

    #[tokio::test]
    async fn a_failed_bind_leaves_a_state_the_supervisor_will_retry() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let bind = occupied.local_addr().expect("address").to_string();
        let mut channel = channel_on(&bind);

        assert!(channel.connect().await.is_err());
        assert!(
            !channel.connection_state().is_connected(),
            "a channel left on Connecting is never rebuilt"
        );
        assert_eq!(channel.connection_state(), ConnectionState::Error);
    }

    #[tokio::test]
    async fn a_terminal_that_never_authenticates_loses_its_connection() {
        // Without this bound a peer needs no credential at all to pin a
        // connection slot open: it just opens a socket and says nothing.
        let bind = free_port().await;
        let mut channel = channel_on(&bind);
        channel.deadlines = ReadDeadlines::new(Duration::from_millis(50), Duration::from_secs(600));
        channel.connect().await.expect("connect");
        channel.start_events().await.expect("start events");

        let mut silent = TcpStream::connect(&bind).await.expect("terminal");
        let mut scratch = [0_u8; 1];
        let read = timeout(Duration::from_secs(5), silent.read(&mut scratch))
            .await
            .expect("silent peer must be dropped at its deadline");
        assert_eq!(read.expect("read"), 0, "silent peer must see EOF");
        assert!(
            channel
                .diagnostics
                .last_error()
                .is_some_and(|error| error.contains("without authenticating")),
            "an unearned connection slot must be visible to an operator"
        );

        channel.disconnect().await.expect("disconnect");
    }
}
