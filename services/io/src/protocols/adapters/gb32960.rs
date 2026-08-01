//! GB/T 32960 vehicle telemetry server adapter.
//!
//! The adapter accepts terminal-originated TCP reports, validates frame BCC,
//! VIN allow-list membership and the unencrypted payload mode, acknowledges
//! valid reports, and projects common whole-vehicle, position, alarm and drive
//! motor fields into the shared protocol data model.

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

const FRAME_MARKER: [u8; 2] = [0x23, 0x23];
const COMMAND_REALTIME: u8 = 0x02;
const COMMAND_REISSUE: u8 = 0x03;
const RESPONSE_SUCCESS: u8 = 0x01;
const ENCRYPTION_NONE: u8 = 0x01;
const HEADER_LENGTH: usize = 24;
const MAX_FRAME_BYTES: usize = 65_536;
const DEFAULT_BIND: &str = "127.0.0.1:32960";

fn default_bind() -> String {
    DEFAULT_BIND.to_owned()
}

/// GB/T 32960 server parameters.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Gb32960ParamsConfig {
    #[serde(default = "default_bind")]
    pub(crate) bind: String,
    pub(crate) allowed_vins: Vec<String>,
}

impl Gb32960ParamsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        self.bind.parse::<std::net::SocketAddr>().map_err(|error| {
            GatewayError::Config(format!("invalid GB/T 32960 bind address: {error}"))
        })?;
        if self.allowed_vins.is_empty() {
            return Err(GatewayError::Config(
                "GB/T 32960 allowed_vins must contain at least one VIN".to_owned(),
            ));
        }
        for vin in &self.allowed_vins {
            if vin.len() != 17 || !vin.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
                return Err(GatewayError::Config(format!(
                    "GB/T 32960 VIN '{vin}' must contain 17 ASCII letters or digits"
                )));
            }
        }
        Ok(())
    }
}

/// Supported field selectors for GB/T 32960 reports.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Gb32960Field {
    VehicleStatus,
    ChargingStatus,
    OperationMode,
    SpeedKmh,
    MileageKm,
    TotalVoltageV,
    TotalCurrentA,
    SocPercent,
    DcDcStatus,
    Gear,
    InsulationResistanceKohm,
    AcceleratorPercent,
    BrakeStatus,
    Positioned,
    LongitudeWest,
    LatitudeSouth,
    Longitude,
    Latitude,
    AlarmLevel,
    DriveMotorStatus,
    ControllerTemperatureC,
    DriveMotorSpeedRpm,
    DriveMotorTorqueNm,
    DriveMotorTemperatureC,
    ControllerInputVoltageV,
    ControllerDcCurrentA,
}

/// Point mapping into a decoded GB/T 32960 report.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub(crate) struct Gb32960PointMapping {
    pub(crate) field: Gb32960Field,
    #[serde(default)]
    pub(crate) motor_index: Option<u8>,
}

impl Gb32960PointMapping {
    fn validate(self, point_type: PointType) -> Result<Self> {
        if !matches!(point_type, PointType::Telemetry | PointType::Signal) {
            return Err(GatewayError::Config(
                "GB/T 32960 reports only map telemetry and signal points".to_owned(),
            ));
        }
        let motor_field = matches!(
            self.field,
            Gb32960Field::DriveMotorStatus
                | Gb32960Field::ControllerTemperatureC
                | Gb32960Field::DriveMotorSpeedRpm
                | Gb32960Field::DriveMotorTorqueNm
                | Gb32960Field::DriveMotorTemperatureC
                | Gb32960Field::ControllerInputVoltageV
                | Gb32960Field::ControllerDcCurrentA
        );
        if motor_field != self.motor_index.is_some() {
            return Err(GatewayError::Config(
                "GB/T 32960 motor_index is required only for drive-motor fields".to_owned(),
            ));
        }
        Ok(self)
    }
}

pub(crate) fn parse_point_mapping(
    mapping: &serde_json::Value,
    point_type: PointType,
) -> Result<Gb32960PointMapping> {
    serde_json::from_value::<Gb32960PointMapping>(mapping.clone())
        .map_err(|error| GatewayError::Config(format!("invalid GB/T 32960 mapping: {error}")))?
        .validate(point_type)
}

#[derive(Debug, Clone)]
pub(crate) struct Gb32960PointConfig {
    pub(crate) point_id: u32,
    pub(crate) point_type: PointType,
    pub(crate) mapping: Gb32960PointMapping,
    pub(crate) scale: f64,
    pub(crate) offset: f64,
    pub(crate) reverse: bool,
}

#[derive(Debug)]
struct Gb32960Frame {
    command: u8,
    response: u8,
    vin: String,
    encryption: u8,
    data: Vec<u8>,
}

/// Event-driven GB/T 32960 terminal server.
pub struct Gb32960Channel {
    channel_id: u32,
    params: Gb32960ParamsConfig,
    points: Arc<Vec<Gb32960PointConfig>>,
    listener: Option<TcpListener>,
    event_tx: DataEventSender,
    event_rx: Option<DataEventReceiver>,
    state: Arc<AtomicU8>,
    diagnostics: Arc<AtomicDiagnostics>,
    cancellation: CancellationToken,
    server_handle: Option<JoinHandle<()>>,
    deadlines: ReadDeadlines,
}

impl Gb32960Channel {
    pub(crate) fn new(
        params: Gb32960ParamsConfig,
        channel_id: u32,
        points: Vec<Gb32960PointConfig>,
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
        allowed_vins: Arc<Vec<String>>,
        points: Arc<Vec<Gb32960PointConfig>>,
        context: ServerContext,
        cancellation: CancellationToken,
        deadlines: ReadDeadlines,
    ) {
        let event_tx = context.event_tx.clone();
        let diagnostics = Arc::clone(&context.diagnostics);
        let accept_cancellation = cancellation.clone();
        run_accept_loop(listener, context, accept_cancellation, move |stream| {
            let allowed_vins = Arc::clone(&allowed_vins);
            let points = Arc::clone(&points);
            let event_tx = event_tx.clone();
            let diagnostics = Arc::clone(&diagnostics);
            let cancellation = cancellation.clone();
            async move {
                if let Err(error) = handle_connection(
                    stream,
                    &allowed_vins,
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

impl HasMetadata for Gb32960Channel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "gb32960",
            display_name: "GB/T 32960",
            description: "Allow-listed GB/T 32960 vehicle telemetry TCP server",
            is_recommended: true,
            example_config: serde_json::json!({
                "bind": DEFAULT_BIND,
                "allowed_vins": ["LTEST000000000001"]
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
                    "allowed_vins",
                    "Allowed VINs",
                    "Explicit allow-list of 17-character terminal VINs",
                    ParameterType::Array,
                ),
            ],
        }
    }
}

#[async_trait]
impl ChannelRuntime for Gb32960Channel {
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
            Arc::new(self.params.allowed_vins.clone()),
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
            protocol: "gb32960".to_owned(),
            connection_state: self.connection_state(),
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: serde_json::json!({
                "channel_id": self.channel_id,
                "bind": self.params.bind,
                "allowed_vin_count": self.params.allowed_vins.len(),
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
    allowed_vins: &[String],
    points: &[Gb32960PointConfig],
    event_tx: &DataEventSender,
    diagnostics: &AtomicDiagnostics,
    cancellation: &CancellationToken,
    deadlines: ReadDeadlines,
) -> Result<()> {
    let mut buffer = BytesMut::with_capacity(4_096);
    let mut allow_listed = false;
    loop {
        let outcome = read_bounded(
            &mut stream,
            &mut buffer,
            deadlines,
            allow_listed,
            cancellation,
        )
        .await?;
        match outcome {
            ReadOutcome::Bytes => {},
            ReadOutcome::Closed => return Ok(()),
            // A vehicle that reported and then went quiet is an ordinary loss
            // of coverage. One that never named an allowed VIN is a peer
            // occupying a connection slot it never earned, so say so.
            ReadOutcome::TimedOut => {
                return if allow_listed {
                    Ok(())
                } else {
                    Err(GatewayError::Protocol(
                        "GB/T 32960 terminal held a connection without reporting a VIN".to_owned(),
                    ))
                };
            },
        }
        while let Some(frame) = decode_next_frame(&mut buffer)? {
            if !allowed_vins.iter().any(|vin| vin == &frame.vin) {
                return Err(GatewayError::Protocol(format!(
                    "GB/T 32960 terminal VIN {} is not allowed",
                    frame.vin
                )));
            }
            allow_listed = true;
            if frame.encryption != ENCRYPTION_NONE {
                return Err(GatewayError::Unsupported(format!(
                    "GB/T 32960 encryption mode {} is not configured",
                    frame.encryption
                )));
            }
            if matches!(frame.command, COMMAND_REALTIME | COMMAND_REISSUE) && frame.response == 0xfe
            {
                let values = decode_report(&frame.data)?;
                let batch = project_points(points, &values, diagnostics);
                if !batch.is_empty() {
                    diagnostics.add_read(batch.len() as u64);
                    let _ = event_tx.try_send(DataEvent::DataUpdate(batch));
                }
            }
            let acknowledgement = encode_ack(&frame)?;
            stream
                .write_all(&acknowledgement)
                .await
                .map_err(GatewayError::Io)?;
            diagnostics.inc_write();
        }
    }
}

fn decode_next_frame(buffer: &mut BytesMut) -> Result<Option<Gb32960Frame>> {
    let Some(marker) = buffer.windows(2).position(|window| window == FRAME_MARKER) else {
        if buffer.len() > 1 {
            buffer.advance(buffer.len() - 1);
        }
        return Ok(None);
    };
    if marker > 0 {
        buffer.advance(marker);
    }
    if buffer.len() < HEADER_LENGTH {
        return Ok(None);
    }
    let data_length = usize::from(u16::from_be_bytes([buffer[22], buffer[23]]));
    let frame_length = HEADER_LENGTH
        .checked_add(data_length)
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| GatewayError::InvalidResponse("GB/T 32960 length overflow".to_owned()))?;
    if frame_length > MAX_FRAME_BYTES {
        return Err(GatewayError::InvalidResponse(
            "GB/T 32960 frame exceeds configured maximum".to_owned(),
        ));
    }
    if buffer.len() < frame_length {
        return Ok(None);
    }
    let bytes = buffer.split_to(frame_length).freeze();
    let bcc = bytes[2..frame_length - 1]
        .iter()
        .fold(0_u8, |bcc, byte| bcc ^ *byte);
    if bcc != bytes[frame_length - 1] {
        return Err(GatewayError::InvalidResponse(
            "GB/T 32960 BCC mismatch".to_owned(),
        ));
    }
    let vin = std::str::from_utf8(&bytes[4..21])
        .map_err(|_| GatewayError::InvalidResponse("GB/T 32960 VIN is not ASCII".to_owned()))?
        .to_owned();
    Ok(Some(Gb32960Frame {
        command: bytes[2],
        response: bytes[3],
        vin,
        encryption: bytes[21],
        data: bytes[24..frame_length - 1].to_vec(),
    }))
}

fn encode_ack(frame: &Gb32960Frame) -> Result<Vec<u8>> {
    let data = frame.data.get(..6).unwrap_or(&[]);
    encode_frame(
        frame.command,
        RESPONSE_SUCCESS,
        &frame.vin,
        frame.encryption,
        data,
    )
}

fn encode_frame(
    command: u8,
    response: u8,
    vin: &str,
    encryption: u8,
    data: &[u8],
) -> Result<Vec<u8>> {
    if vin.len() != 17 {
        return Err(GatewayError::InvalidData(
            "GB/T 32960 VIN must contain 17 bytes".to_owned(),
        ));
    }
    let length = u16::try_from(data.len()).map_err(|_| {
        GatewayError::InvalidData("GB/T 32960 data unit exceeds u16 length".to_owned())
    })?;
    let mut frame = Vec::with_capacity(HEADER_LENGTH + data.len() + 1);
    frame.extend_from_slice(&FRAME_MARKER);
    frame.push(command);
    frame.push(response);
    frame.extend_from_slice(vin.as_bytes());
    frame.push(encryption);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(data);
    let bcc = frame[2..].iter().fold(0_u8, |bcc, byte| bcc ^ *byte);
    frame.push(bcc);
    Ok(frame)
}

type FieldKey = (Gb32960Field, Option<u8>);

fn decode_report(data: &[u8]) -> Result<HashMap<FieldKey, f64>> {
    if data.len() < 6 {
        return Err(GatewayError::InvalidResponse(
            "GB/T 32960 report is missing collection time".to_owned(),
        ));
    }
    let mut cursor = 6;
    let mut values = HashMap::new();
    while cursor < data.len() {
        let info_type = data[cursor];
        cursor += 1;
        match info_type {
            0x01 => cursor += decode_vehicle(&data[cursor..], &mut values)?,
            0x02 => cursor += decode_motors(&data[cursor..], &mut values)?,
            0x03 => cursor += fuel_cell_block_length(&data[cursor..])?,
            0x04 => cursor += fixed_block_length(&data[cursor..], 5, "engine")?,
            0x05 => cursor += decode_position(&data[cursor..], &mut values)?,
            0x06 => cursor += fixed_block_length(&data[cursor..], 14, "extreme-value")?,
            0x07 => cursor += decode_alarm(&data[cursor..], &mut values)?,
            0x08 => cursor += storage_voltage_block_length(&data[cursor..])?,
            0x09 => cursor += storage_temperature_block_length(&data[cursor..])?,
            _ => {
                return Err(GatewayError::Unsupported(format!(
                    "GB/T 32960 information type {info_type:#04x} is not supported in mixed reports"
                )));
            },
        }
    }
    Ok(values)
}

fn fixed_block_length(data: &[u8], length: usize, name: &str) -> Result<usize> {
    data.get(..length).ok_or_else(|| {
        GatewayError::InvalidResponse(format!("truncated GB/T 32960 {name} block"))
    })?;
    Ok(length)
}

fn fuel_cell_block_length(data: &[u8]) -> Result<usize> {
    let probe_count = usize::from(be_u16(data.get(6..8).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 fuel-cell block".to_owned())
    })?));
    let length = 15_usize
        .checked_add(probe_count.checked_mul(2).ok_or_else(|| {
            GatewayError::InvalidResponse("GB/T 32960 fuel-cell probe count overflow".to_owned())
        })?)
        .ok_or_else(|| GatewayError::InvalidResponse("fuel-cell length overflow".to_owned()))?;
    fixed_block_length(data, length, "fuel-cell")
}

fn storage_voltage_block_length(data: &[u8]) -> Result<usize> {
    let count = usize::from(*data.first().ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 storage-voltage block".to_owned())
    })?);
    let mut cursor = 1;
    for _ in 0..count {
        let cell_count = usize::from(*data.get(cursor + 9).ok_or_else(|| {
            GatewayError::InvalidResponse(
                "truncated GB/T 32960 storage-voltage subsystem".to_owned(),
            )
        })?);
        cursor = cursor
            .checked_add(10 + cell_count * 2)
            .ok_or_else(|| GatewayError::InvalidResponse("storage length overflow".to_owned()))?;
        if cursor > data.len() {
            return Err(GatewayError::InvalidResponse(
                "truncated GB/T 32960 storage-voltage cells".to_owned(),
            ));
        }
    }
    Ok(cursor)
}

fn storage_temperature_block_length(data: &[u8]) -> Result<usize> {
    let count = usize::from(*data.first().ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 storage-temperature block".to_owned())
    })?);
    let mut cursor = 1;
    for _ in 0..count {
        let probe_count = usize::from(be_u16(data.get(cursor + 1..cursor + 3).ok_or_else(
            || {
                GatewayError::InvalidResponse(
                    "truncated GB/T 32960 storage-temperature subsystem".to_owned(),
                )
            },
        )?));
        cursor = cursor
            .checked_add(3 + probe_count)
            .ok_or_else(|| GatewayError::InvalidResponse("storage length overflow".to_owned()))?;
        if cursor > data.len() {
            return Err(GatewayError::InvalidResponse(
                "truncated GB/T 32960 storage-temperature probes".to_owned(),
            ));
        }
    }
    Ok(cursor)
}

fn decode_vehicle(data: &[u8], values: &mut HashMap<FieldKey, f64>) -> Result<usize> {
    let data = data.get(..20).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 vehicle block".to_owned())
    })?;
    insert(values, Gb32960Field::VehicleStatus, f64::from(data[0]));
    insert(values, Gb32960Field::ChargingStatus, f64::from(data[1]));
    insert(values, Gb32960Field::OperationMode, f64::from(data[2]));
    insert(
        values,
        Gb32960Field::SpeedKmh,
        f64::from(be_u16(&data[3..5])) / 10.0,
    );
    insert(
        values,
        Gb32960Field::MileageKm,
        f64::from(be_u32(&data[5..9])) / 10.0,
    );
    insert(
        values,
        Gb32960Field::TotalVoltageV,
        f64::from(be_u16(&data[9..11])) / 10.0,
    );
    insert(
        values,
        Gb32960Field::TotalCurrentA,
        (f64::from(be_u16(&data[11..13])) - 10_000.0) / 10.0,
    );
    insert(values, Gb32960Field::SocPercent, f64::from(data[13]));
    insert(values, Gb32960Field::DcDcStatus, f64::from(data[14]));
    insert(values, Gb32960Field::Gear, f64::from(data[15]));
    insert(
        values,
        Gb32960Field::InsulationResistanceKohm,
        f64::from(be_u16(&data[16..18])),
    );
    insert(
        values,
        Gb32960Field::AcceleratorPercent,
        f64::from(data[18]),
    );
    insert(values, Gb32960Field::BrakeStatus, f64::from(data[19]));
    Ok(20)
}

fn decode_motors(data: &[u8], values: &mut HashMap<FieldKey, f64>) -> Result<usize> {
    let count = usize::from(*data.first().ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 motor block".to_owned())
    })?);
    let total = 1_usize
        .checked_add(count.checked_mul(12).ok_or_else(|| {
            GatewayError::InvalidResponse("GB/T 32960 motor count overflow".to_owned())
        })?)
        .ok_or_else(|| GatewayError::InvalidResponse("motor length overflow".to_owned()))?;
    let data = data.get(..total).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 motor entries".to_owned())
    })?;
    for index in 0..count {
        let motor_index = u8::try_from(index).map_err(|_| {
            GatewayError::InvalidResponse("GB/T 32960 motor index overflow".to_owned())
        })?;
        let start = 1 + index * 12;
        let motor = &data[start..start + 12];
        insert_motor(
            values,
            Gb32960Field::DriveMotorStatus,
            motor_index,
            f64::from(motor[1]),
        );
        insert_motor(
            values,
            Gb32960Field::ControllerTemperatureC,
            motor_index,
            f64::from(motor[2]) - 40.0,
        );
        insert_motor(
            values,
            Gb32960Field::DriveMotorSpeedRpm,
            motor_index,
            f64::from(be_u16(&motor[3..5])) - 20_000.0,
        );
        insert_motor(
            values,
            Gb32960Field::DriveMotorTorqueNm,
            motor_index,
            (f64::from(be_u16(&motor[5..7])) - 20_000.0) / 10.0,
        );
        insert_motor(
            values,
            Gb32960Field::DriveMotorTemperatureC,
            motor_index,
            f64::from(motor[7]) - 40.0,
        );
        insert_motor(
            values,
            Gb32960Field::ControllerInputVoltageV,
            motor_index,
            f64::from(be_u16(&motor[8..10])) / 10.0,
        );
        insert_motor(
            values,
            Gb32960Field::ControllerDcCurrentA,
            motor_index,
            (f64::from(be_u16(&motor[10..12])) - 10_000.0) / 10.0,
        );
    }
    Ok(total)
}

fn decode_position(data: &[u8], values: &mut HashMap<FieldKey, f64>) -> Result<usize> {
    let data = data.get(..9).ok_or_else(|| {
        GatewayError::InvalidResponse("truncated GB/T 32960 position block".to_owned())
    })?;
    let status = data[0];
    insert(
        values,
        Gb32960Field::Positioned,
        f64::from(status & 0x01 == 0),
    );
    insert(
        values,
        Gb32960Field::LongitudeWest,
        f64::from(status & 0x02 != 0),
    );
    insert(
        values,
        Gb32960Field::LatitudeSouth,
        f64::from(status & 0x04 != 0),
    );
    insert(
        values,
        Gb32960Field::Longitude,
        f64::from(be_u32(&data[1..5])) / 1_000_000.0,
    );
    insert(
        values,
        Gb32960Field::Latitude,
        f64::from(be_u32(&data[5..9])) / 1_000_000.0,
    );
    Ok(9)
}

fn decode_alarm(data: &[u8], values: &mut HashMap<FieldKey, f64>) -> Result<usize> {
    if data.len() < 9 {
        return Err(GatewayError::InvalidResponse(
            "truncated GB/T 32960 alarm block".to_owned(),
        ));
    }
    insert(values, Gb32960Field::AlarmLevel, f64::from(data[0]));
    let mut cursor = 5;
    for _ in 0..4 {
        let count = usize::from(*data.get(cursor).ok_or_else(|| {
            GatewayError::InvalidResponse("truncated GB/T 32960 alarm list".to_owned())
        })?);
        cursor = cursor
            .checked_add(1 + count * 4)
            .ok_or_else(|| GatewayError::InvalidResponse("alarm length overflow".to_owned()))?;
        if cursor > data.len() {
            return Err(GatewayError::InvalidResponse(
                "truncated GB/T 32960 alarm list".to_owned(),
            ));
        }
    }
    Ok(cursor)
}

fn insert(values: &mut HashMap<FieldKey, f64>, field: Gb32960Field, value: f64) {
    values.insert((field, None), value);
}

fn insert_motor(
    values: &mut HashMap<FieldKey, f64>,
    field: Gb32960Field,
    motor_index: u8,
    value: f64,
) {
    values.insert((field, Some(motor_index)), value);
}

/// Projects a decoded report, skipping points that cannot be represented
/// rather than failing the whole report.
///
/// One misconfigured scale used to abort the batch, which dropped the vehicle's
/// TCP connection; the terminal reconnected, reported again, and lost the
/// connection again, so a single bad point silently cost every other point on
/// that vehicle.
fn project_points(
    points: &[Gb32960PointConfig],
    values: &HashMap<FieldKey, f64>,
    diagnostics: &AtomicDiagnostics,
) -> DataBatch {
    let mut batch = DataBatch::with_capacity(points.len());
    for point in points {
        let Some(raw) = values.get(&(point.mapping.field, point.mapping.motor_index)) else {
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
                    "GB/T 32960 point {} transformed to a non-finite value",
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

    const VIN: &str = "LTEST000000000001";

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
        let points = vec![Gb32960PointConfig {
            point_id: 3,
            point_type: PointType::Telemetry,
            mapping: Gb32960PointMapping {
                field: Gb32960Field::SpeedKmh,
                motor_index: None,
            },
            scale,
            offset,
            reverse: false,
        }];
        let values = HashMap::from([((Gb32960Field::SpeedKmh, None), raw)]);

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
        let point = |point_id, scale, offset| Gb32960PointConfig {
            point_id,
            point_type: PointType::Telemetry,
            mapping: Gb32960PointMapping {
                field: Gb32960Field::SpeedKmh,
                motor_index: None,
            },
            scale,
            offset,
            reverse: false,
        };
        let points = vec![point(1, f64::MAX, f64::MAX), point(2, 1.0, 0.0)];
        let values = HashMap::from([((Gb32960Field::SpeedKmh, None), 50.0)]);
        let diagnostics = AtomicDiagnostics::new();

        let batch = project_points(&points, &values, &diagnostics);

        let survivor = batch.iter().next().expect("the healthy point survives");
        assert_eq!(survivor.id, 2);
        assert_eq!(survivor.value, Value::Float(50.0));
        assert_eq!(batch.len(), 1);
        assert_eq!(diagnostics.error_count(), 1);
    }

    /// Binds an ephemeral port and hands it back so a channel can claim it.
    async fn free_port() -> String {
        let probe = TcpListener::bind("127.0.0.1:0").await.expect("probe");
        let address = probe.local_addr().expect("address");
        drop(probe);
        address.to_string()
    }

    fn channel_on(bind: &str) -> Gb32960Channel {
        Gb32960Channel::new(
            Gb32960ParamsConfig {
                bind: bind.to_owned(),
                allowed_vins: vec![VIN.to_owned()],
            },
            1,
            vec![Gb32960PointConfig {
                point_id: 7,
                point_type: PointType::Telemetry,
                mapping: Gb32960PointMapping {
                    field: Gb32960Field::SpeedKmh,
                    motor_index: None,
                },
                scale: 1.0,
                offset: 0.0,
                reverse: false,
            }],
        )
        .expect("channel")
    }

    fn realtime_report() -> Vec<u8> {
        let mut report = vec![24, 1, 2, 3, 4, 5, 0x01];
        report.extend_from_slice(&[
            1, 2, 3, 0x01, 0xf4, 0, 0, 0x30, 0x39, 0x0e, 0x74, 0x27, 0x74, 80, 1, 4, 0, 100, 50, 1,
        ]);
        report
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

    #[test]
    fn frame_streaming_decoder_validates_bcc_and_preserves_remainder() {
        let frame =
            encode_frame(COMMAND_REALTIME, 0xfe, VIN, ENCRYPTION_NONE, &[1, 2, 3]).expect("frame");
        let mut buffer = BytesMut::from(&[&[0x00][..], &frame[..], &frame[..10]].concat()[..]);
        let decoded = decode_next_frame(&mut buffer)
            .expect("decode")
            .expect("frame");
        assert_eq!(decoded.vin, VIN);
        assert_eq!(decoded.data, [1, 2, 3]);
        assert_eq!(buffer.len(), 10);
    }

    #[test]
    fn vehicle_and_position_blocks_decode_engineering_units() {
        let mut data = vec![24, 1, 2, 3, 4, 5];
        data.extend_from_slice(&[
            0x01, 1, 2, 3, 0x01, 0xf4, 0, 0, 0x30, 0x39, 0x0e, 0x74, 0x27, 0x74, 80, 1, 4, 0, 100,
            50, 1,
        ]);
        data.extend_from_slice(&[0x05, 0, 0x06, 0x2f, 0x5e, 0x80, 0x01, 0xd7, 0x84, 0x00]);
        let values = decode_report(&data).expect("report");
        assert_eq!(values[&(Gb32960Field::SpeedKmh, None)], 50.0);
        assert_eq!(values[&(Gb32960Field::MileageKm, None)], 1234.5);
        assert_eq!(values[&(Gb32960Field::TotalCurrentA, None)], 10.0);
        assert_eq!(values[&(Gb32960Field::Longitude, None)], 103.76768);
        assert_eq!(values[&(Gb32960Field::Latitude, None)], 30.901248);
    }

    #[test]
    fn motor_mapping_requires_an_index() {
        let mapping = serde_json::json!({"field": "drive_motor_speed_rpm"});
        assert!(parse_point_mapping(&mapping, PointType::Telemetry).is_err());
        let mapping = serde_json::json!({
            "field": "drive_motor_speed_rpm",
            "motor_index": 0
        });
        assert!(parse_point_mapping(&mapping, PointType::Telemetry).is_ok());
        assert!(parse_point_mapping(&mapping, PointType::Control).is_err());
    }

    #[test]
    fn server_configuration_is_allow_listed_and_loopback_by_default() {
        let missing: Gb32960ParamsConfig = serde_json::from_value(serde_json::json!({
            "allowed_vins": []
        }))
        .expect("decode");
        assert_eq!(missing.bind, DEFAULT_BIND);
        assert!(missing.validate().is_err());
    }

    #[tokio::test]
    async fn tcp_connection_projects_an_allow_listed_report_to_events() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let allowed = vec![VIN.to_owned()];
        let points = vec![Gb32960PointConfig {
            point_id: 7,
            point_type: PointType::Telemetry,
            mapping: Gb32960PointMapping {
                field: Gb32960Field::SpeedKmh,
                motor_index: None,
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
                &allowed,
                &points,
                &event_tx,
                &server_diagnostics,
                &server_cancellation,
                ReadDeadlines::DEFAULT,
            )
            .await
        });

        let mut client = TcpStream::connect(address).await.expect("client");
        let mut report = vec![24, 1, 2, 3, 4, 5, 0x01];
        report.extend_from_slice(&[
            1, 2, 3, 0x01, 0xf4, 0, 0, 0x30, 0x39, 0x0e, 0x74, 0x27, 0x74, 80, 1, 4, 0, 100, 50, 1,
        ]);
        let frame =
            encode_frame(COMMAND_REALTIME, 0xfe, VIN, ENCRYPTION_NONE, &report).expect("frame");
        client.write_all(&frame).await.expect("send");

        let event = timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("event timeout")
            .expect("event");
        let DataEvent::DataUpdate(batch) = event else {
            panic!("expected data event");
        };
        let point = batch.iter().next().expect("point");
        assert_eq!(point.id, 7);
        assert_eq!(point.value, Value::Float(50.0));

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
        let mut terminal = TcpStream::connect(&bind).await.expect("terminal");
        let frame = encode_frame(
            COMMAND_REALTIME,
            0xfe,
            VIN,
            ENCRYPTION_NONE,
            &realtime_report(),
        )
        .expect("frame");
        terminal.write_all(&frame).await.expect("send");
        let batch = next_data_update(&mut events).await;
        assert_eq!(
            batch.iter().next().expect("point").value,
            Value::Float(50.0)
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
    async fn a_terminal_that_never_names_an_allowed_vin_loses_its_connection() {
        // Without this bound a peer needs no VIN at all to pin a connection
        // slot open: it just opens a socket and says nothing.
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
                .is_some_and(|error| error.contains("without reporting a VIN")),
            "an unearned connection slot must be visible to an operator"
        );

        channel.disconnect().await.expect("disconnect");
    }
}
