//! Aether-485 Private RS-485 Protocol Adapter
//!
//! Implements the company's private RS-485 communication protocol (Aether-485-V1.0)
//! for data collection from embedded devices, smart terminals, and industrial controllers.
//!
//! # Protocol Frame Format
//!
//! | Field   | Bytes | Description                              |
//! |---------|-------|------------------------------------------|
//! | Header  | 2     | Fixed: 0x5A 0xA5                         |
//! | Len     | 1     | Byte count of Data (ID + CMD + Payload)  |
//! | Data    | Var   | Device ID (1B) + CMD (1B) + Payload      |
//! | CRC16   | 2     | CRC-16 MODBUS over Len+Data, LE          |
//!
//! # CRC Scope
//!
//! CRC-16 MODBUS is computed over **Len byte + Data bytes** (excludes header and CRC itself).
//!
//! # Supported Commands
//!
//! - `0x01` Query device info — response carries power data (mW, big-endian u16)

use std::sync::Arc;
use std::time::Duration;

use aether_model::PointType;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

const FRAME_HEADER: [u8; 2] = [0x5A, 0xA5];
const CMD_QUERY_DEVICE: u8 = 0x01;

const DEFAULT_BAUD_RATE: u32 = 115_200;
const DEFAULT_IO_TIMEOUT_MS: u64 = 1000;
const DEFAULT_RETRY_COUNT: u32 = 2;
const DEFAULT_FRAME_DELAY_MS: u64 = 50;

const MAX_FRAME_SIZE: usize = 264;

// ============================================================================
// CRC-16 MODBUS
// ============================================================================

/// CRC-16 MODBUS: polynomial 0x8005 (reflected 0xA001), init 0xFFFF, LE output.
fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

// ============================================================================
// Frame Encoding / Decoding
// ============================================================================

/// Build a request frame.
///
/// Layout: `[0x5A, 0xA5, Len, ID, CMD, ...payload, CRC_L, CRC_H]`
fn build_request(device_id: u8, cmd: u8) -> Vec<u8> {
    let len: u8 = 2; // ID + CMD (no payload for CMD 0x01)
    let crc_input = [len, device_id, cmd];
    let crc = crc16_modbus(&crc_input);

    let mut frame = Vec::with_capacity(7);
    frame.extend_from_slice(&FRAME_HEADER);
    frame.extend_from_slice(&crc_input);
    frame.push((crc & 0xFF) as u8);
    frame.push((crc >> 8) as u8);
    frame
}

/// Parsed response from a slave device.
#[derive(Debug)]
pub struct DeviceResponse {
    pub device_id: u8,
    pub cmd: Option<u8>,
    pub power_mw: u16,
}

/// Parse and verify a complete response frame.
fn parse_response(frame: &[u8]) -> Result<DeviceResponse> {
    if frame.len() < 8 {
        return Err(GatewayError::Protocol(format!(
            "Frame too short: {} bytes",
            frame.len()
        )));
    }

    if frame[0] != FRAME_HEADER[0] || frame[1] != FRAME_HEADER[1] {
        return Err(GatewayError::Protocol(format!(
            "Bad header: {:02X} {:02X}",
            frame[0], frame[1]
        )));
    }

    let len = frame[2] as usize;
    let expected_total = 2 + 1 + len + 2;
    if frame.len() < expected_total {
        return Err(GatewayError::Protocol(format!(
            "Frame length mismatch: need {}, got {}",
            expected_total,
            frame.len()
        )));
    }

    let crc_data = &frame[2..3 + len];
    let crc_calc = crc16_modbus(crc_data);
    let crc_rx = u16::from_le_bytes([frame[3 + len], frame[3 + len + 1]]);
    if crc_calc != crc_rx {
        return Err(GatewayError::Protocol(format!(
            "CRC mismatch: calc {:04X}, recv {:04X}",
            crc_calc, crc_rx
        )));
    }

    let data = &frame[3..3 + len];

    match len {
        // Legacy fallback: ID + powerH + powerL (older firmware without CMD echo)
        3 => {
            let power = (u16::from(data[1]) << 8) | u16::from(data[2]);
            Ok(DeviceResponse {
                device_id: data[0],
                cmd: None,
                power_mw: power,
            })
        },
        // Standard format (V1.0 spec): ID + CMD + powerH + powerL [+ extra]
        n if n >= 4 => {
            let power = (u16::from(data[2]) << 8) | u16::from(data[3]);
            Ok(DeviceResponse {
                device_id: data[0],
                cmd: Some(data[1]),
                power_mw: power,
            })
        },
        _ => Err(GatewayError::Protocol(format!(
            "Unexpected data length: {}",
            len
        ))),
    }
}

// ============================================================================
// Frame I/O Helpers
// ============================================================================

/// Read one complete protocol frame (Header + Len + Data + CRC) from the serial port.
async fn read_one_frame(serial: &mut SerialStream, timeout_dur: Duration) -> Result<Vec<u8>> {
    let mut hdr = [0u8; 3];
    timeout(timeout_dur, serial.read_exact(&mut hdr))
        .await
        .map_err(|_| GatewayError::ReadTimeout)?
        .map_err(GatewayError::Io)?;

    if hdr[0] != FRAME_HEADER[0] || hdr[1] != FRAME_HEADER[1] {
        return Err(GatewayError::Protocol(format!(
            "Bad response header: {:02X} {:02X}",
            hdr[0], hdr[1]
        )));
    }

    let data_len = hdr[2] as usize;
    if data_len == 0 || data_len > MAX_FRAME_SIZE - 5 {
        return Err(GatewayError::Protocol(format!(
            "Invalid response Len: {}",
            data_len
        )));
    }

    let mut rest = vec![0u8; data_len + 2];
    timeout(timeout_dur, serial.read_exact(&mut rest))
        .await
        .map_err(|_| GatewayError::ReadTimeout)?
        .map_err(GatewayError::Io)?;

    let mut full = Vec::with_capacity(3 + data_len + 2);
    full.extend_from_slice(&hdr);
    full.extend_from_slice(&rest);
    Ok(full)
}

// ============================================================================
// Point Mapping (from protocol_mappings JSON)
// ============================================================================

/// Per-point protocol mapping stored in the `protocol_mappings` column.
///
/// Example JSON: `{"device_id": 1}` or `{"device_id": 1, "cmd": 1}`
#[derive(Debug, Clone, Deserialize)]
pub struct Aether485PointMapping {
    pub device_id: u8,
    #[serde(default = "default_cmd")]
    pub cmd: u8,
}

fn default_cmd() -> u8 {
    CMD_QUERY_DEVICE
}

/// Resolved poll target derived from a configured point.
#[derive(Debug, Clone)]
pub struct PollTarget {
    pub point_id: u32,
    pub point_type: PointType,
    pub device_id: u8,
    pub cmd: u8,
}

// ============================================================================
// Channel Configuration
// ============================================================================

/// Serde-friendly params config parsed from the channel `parameters` JSON.
///
/// Example:
/// ```json
/// {
///   "device": "/dev/ttyAP0",
///   "baud_rate": 115200,
///   "timeout_ms": 1000,
///   "retry_count": 2,
///   "frame_delay_ms": 50,
///   "poll_interval_ms": 1000
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aether485ParamsConfig {
    #[serde(default = "default_device")]
    pub device: String,

    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,

    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    #[serde(default = "default_retry_count")]
    pub retry_count: u32,

    #[serde(default = "default_frame_delay_ms")]
    pub frame_delay_ms: u64,
}

fn default_device() -> String {
    "/dev/ttyAP0".to_string()
}
fn default_baud_rate() -> u32 {
    DEFAULT_BAUD_RATE
}
fn default_timeout_ms() -> u64 {
    DEFAULT_IO_TIMEOUT_MS
}
fn default_retry_count() -> u32 {
    DEFAULT_RETRY_COUNT
}
fn default_frame_delay_ms() -> u64 {
    DEFAULT_FRAME_DELAY_MS
}

impl Aether485ParamsConfig {
    pub fn to_channel_config(&self) -> Aether485ChannelConfig {
        Aether485ChannelConfig {
            device: self.device.clone(),
            baud_rate: self.baud_rate,
            io_timeout: Duration::from_millis(self.timeout_ms),
            retry_count: self.retry_count,
            frame_delay: Duration::from_millis(self.frame_delay_ms),
        }
    }
}

/// Internal channel configuration.
#[derive(Debug, Clone)]
pub struct Aether485ChannelConfig {
    pub device: String,
    pub baud_rate: u32,
    pub io_timeout: Duration,
    pub retry_count: u32,
    pub frame_delay: Duration,
}

// ============================================================================
// Channel Implementation
// ============================================================================

/// Aether-485 channel adapter.
///
/// Implements `ProtocolClient` + `ChannelRuntime` for the company's private
/// RS-485 protocol. Polling-based — sends CMD 0x01 queries to each configured
/// device on the bus and collects power readings (mW).
pub struct Aether485Channel {
    config: Aether485ChannelConfig,
    channel_id: u32,
    name: String,
    serial: Option<SerialStream>,
    state: Arc<RwLock<ConnectionState>>,
    diagnostics: Arc<AtomicDiagnostics>,
    log_context: Arc<LogContext>,
    poll_targets: Vec<PollTarget>,
}

impl Aether485Channel {
    pub fn new(
        config: Aether485ChannelConfig,
        channel_id: u32,
        name: String,
        poll_targets: Vec<PollTarget>,
    ) -> Self {
        Self {
            config,
            channel_id,
            name,
            serial: None,
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            diagnostics: Arc::new(AtomicDiagnostics::default()),
            log_context: Arc::new(LogContext::new(channel_id)),
            poll_targets,
        }
    }

    fn get_state(&self) -> ConnectionState {
        *self.state.read()
    }

    fn set_state(&self, state: ConnectionState) {
        *self.state.write() = state;
    }

    fn open_serial(&self) -> Result<SerialStream> {
        tokio_serial::new(&self.config.device, self.config.baud_rate)
            .parity(Parity::None)
            .data_bits(DataBits::Eight)
            .stop_bits(StopBits::One)
            .open_native_async()
            .map_err(|e| {
                GatewayError::Connection(format!("Failed to open {}: {}", self.config.device, e))
            })
    }

    /// Send a frame and receive the complete response.
    ///
    /// Handles RS-485 half-duplex echo: on many transceivers the TX data is
    /// looped back into the RX buffer.  We drain stale bytes before sending,
    /// then detect and skip the echo frame after sending.
    async fn transact(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        let channel_id = self.channel_id;
        let serial = self.serial.as_mut().ok_or(GatewayError::NotConnected)?;
        let timeout_dur = self.config.io_timeout;

        // 1. Drain stale bytes left over from previous transactions / echo
        let drain_timeout = Duration::from_millis(5);
        let mut drain_buf = [0u8; 256];
        while let Ok(Ok(n)) = timeout(drain_timeout, serial.read(&mut drain_buf)).await {
            if n == 0 {
                break;
            }
            debug!("[V485:{}] drained {} stale bytes", channel_id, n);
        }

        // 2. Write request
        timeout(timeout_dur, serial.write_all(frame))
            .await
            .map_err(|_| GatewayError::WriteTimeout)?
            .map_err(GatewayError::Io)?;

        // 3. Brief wait for the slave to process and respond
        tokio::time::sleep(Duration::from_millis(30)).await;

        // 4. Read one frame (may be echo or response)
        let first = read_one_frame(serial, timeout_dur).await?;

        // 5. If the frame is identical to our TX → RS-485 echo, read the real response
        if first == frame {
            debug!(
                "[V485:{}] skipped RS-485 echo ({} bytes)",
                channel_id,
                first.len()
            );
            return read_one_frame(serial, timeout_dur).await;
        }

        Ok(first)
    }

    /// Query a single device with retries.
    async fn query_device(&mut self, device_id: u8, cmd: u8) -> Result<DeviceResponse> {
        let frame = build_request(device_id, cmd);

        let mut last_err = None;
        for attempt in 0..=self.config.retry_count {
            if attempt > 0 {
                debug!(
                    "[V485:{}] retry {} for dev 0x{:02X}",
                    self.channel_id, attempt, device_id
                );
                tokio::time::sleep(self.config.frame_delay).await;
            }

            match self.transact(&frame).await {
                Ok(raw) => match parse_response(&raw) {
                    Ok(resp) => return Ok(resp),
                    Err(e) => last_err = Some(e),
                },
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| GatewayError::Protocol("Unknown error".into())))
    }

    /// Poll all configured targets and collect data.
    async fn poll_all_targets(&mut self) -> (DataBatch, Vec<PointFailure>, u64, u64) {
        let mut batch = DataBatch::default();
        let mut failures = Vec::new();
        let mut ok_count = 0u64;
        let mut err_count = 0u64;

        let targets = self.poll_targets.clone();
        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(self.config.frame_delay).await;
            }

            match self.query_device(target.device_id, target.cmd).await {
                Ok(resp) => {
                    let value = f64::from(resp.power_mw);
                    batch.add(DataPoint::new(target.point_id, target.point_type, value));
                    ok_count += 1;
                    debug!(
                        "[V485:{}] dev 0x{:02X} -> point {} = {} mW",
                        self.channel_id, target.device_id, target.point_id, resp.power_mw
                    );
                },
                Err(e) => {
                    err_count += 1;
                    warn!(
                        "[V485:{}] dev 0x{:02X} (point {}) failed: {}",
                        self.channel_id, target.device_id, target.point_id, e
                    );
                    failures.push(PointFailure::with_error(target.point_id, e.to_string()));
                },
            }
        }

        (batch, failures, ok_count, err_count)
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl HasMetadata for Aether485Channel {
    fn metadata() -> DriverMetadata {
        use serde_json::{Map, Value};
        let mut config = Map::new();
        config.insert(
            "device".to_string(),
            Value::String("/dev/ttyAP0".to_string()),
        );
        config.insert("baud_rate".to_string(), Value::Number(115_200.into()));
        config.insert("timeout_ms".to_string(), Value::Number(1000.into()));
        config.insert("retry_count".to_string(), Value::Number(2.into()));
        config.insert("frame_delay_ms".to_string(), Value::Number(50.into()));

        DriverMetadata {
            name: "aether_485",
            display_name: "Aether-485 Private Protocol",
            description: "RS-485 private protocol for power data collection (Aether-485-V1.0)",
            is_recommended: false,
            example_config: Value::Object(config),
            parameters: vec![],
        }
    }
}

impl ProtocolCapabilities for Aether485Channel {
    fn name(&self) -> &'static str {
        "aether_485"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::Polling]
    }

    fn version(&self) -> &'static str {
        "1.0"
    }
}

impl LoggableProtocol for Aether485Channel {
    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
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

impl Protocol for Aether485Channel {
    fn connection_state(&self) -> ConnectionState {
        self.get_state()
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snap = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "aether_485".to_string(),
            connection_state: self.get_state(),
            read_count: snap.read_count,
            write_count: snap.write_count,
            error_count: snap.error_count,
            last_error: None,
            extra: serde_json::Value::Null,
        })
    }
}

impl ProtocolClient for Aether485Channel {
    async fn connect(&mut self) -> Result<()> {
        let start = std::time::Instant::now();
        let old = self.get_state();
        self.set_state(ConnectionState::Connecting);
        self.log_context
            .log_state_changed(old, ConnectionState::Connecting)
            .await;

        match self.open_serial() {
            Ok(port) => {
                self.serial = Some(port);
                self.set_state(ConnectionState::Connected);

                let endpoint = format!("{} @ {} baud", self.config.device, self.config.baud_rate);
                let dur = start.elapsed().as_millis() as u64;
                info!(
                    "[V485:{}] Connected to {} ({} targets)",
                    self.channel_id,
                    endpoint,
                    self.poll_targets.len()
                );
                self.log_context.log_connected(&endpoint, dur).await;
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
        let old = self.get_state();
        self.serial.take(); // SerialStream dropped on take
        self.set_state(ConnectionState::Disconnected);
        self.log_context.log_disconnected(None).await;
        self.log_context
            .log_state_changed(old, ConnectionState::Disconnected)
            .await;
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        let start = std::time::Instant::now();

        if self.serial.is_none() {
            self.log_context
                .log_error("Not connected", ErrorContext::Polling)
                .await;
            let failures: Vec<PointFailure> = self
                .poll_targets
                .iter()
                .map(|t| PointFailure::new(t.point_id, "Not connected"))
                .collect();
            return PollResult::failed(failures);
        }

        let (batch, failures, ok, err) = self.poll_all_targets().await;

        self.diagnostics.add_read(ok);
        self.diagnostics.add_error(err);

        let dur = start.elapsed().as_millis() as u64;
        debug!(
            "[V485:{}] poll: {} ok, {} fail, {}ms",
            self.channel_id,
            batch.len(),
            failures.len(),
            dur
        );

        self.log_context
            .log_poll_cycle(batch.len(), dur, ok as usize, err as usize)
            .await;

        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    async fn write_control(&mut self, _commands: &[ControlCommand]) -> Result<WriteResult> {
        warn!("Aether-485 protocol does not support control commands");
        Ok(WriteResult {
            success_count: 0,
            failures: vec![(0, "Control not supported by aether_485".into())],
        })
    }

    async fn write_adjustment(
        &mut self,
        _adjustments: &[AdjustmentCommand],
    ) -> Result<WriteResult> {
        warn!("Aether-485 protocol does not support adjustment commands");
        Ok(WriteResult {
            success_count: 0,
            failures: vec![(0, "Adjustment not supported by aether_485".into())],
        })
    }
}

#[async_trait]
impl ChannelRuntime for Aether485Channel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "aether_485"
    }

    fn is_event_driven(&self) -> bool {
        false
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
            .map(|(id, val)| ControlCommand::latching(*id, *val != 0.0))
            .collect();
        let result = <Self as ProtocolClient>::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, val)| AdjustmentCommand::new(*id, *val))
            .collect();
        let result = <Self as ProtocolClient>::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    fn subscribe(&self) -> Option<crate::protocols::core::DataEventReceiver> {
        None
    }

    async fn start_events(&mut self) -> Result<()> {
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        Ok(())
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

    #[test]
    fn test_crc16_modbus() {
        // Verify against the test script:  data = [0x02, 0x01, 0x01] → Len=2, ID=1, CMD=1
        let data = [0x02u8, 0x01, 0x01];
        let crc = crc16_modbus(&data);
        // CRC-16 MODBUS for [02 01 01] = 0x5010  (verified with Python)
        assert_eq!(crc, 0x5010);
    }

    #[test]
    fn test_build_request() {
        let frame = build_request(0x01, CMD_QUERY_DEVICE);
        assert_eq!(frame[0], 0x5A);
        assert_eq!(frame[1], 0xA5);
        assert_eq!(frame[2], 0x02); // Len
        assert_eq!(frame[3], 0x01); // Device ID
        assert_eq!(frame[4], 0x01); // CMD
        assert_eq!(frame.len(), 7);

        // Verify CRC
        let crc = crc16_modbus(&frame[2..5]);
        assert_eq!(frame[5], (crc & 0xFF) as u8);
        assert_eq!(frame[6], (crc >> 8) as u8);
    }

    #[test]
    fn test_parse_response_len3() {
        // Response: ID + powerH + powerL (no CMD echo)
        let device_id = 0x01u8;
        let power: u16 = 1500; // 1500 mW
        let power_h = (power >> 8) as u8;
        let power_l = (power & 0xFF) as u8;

        let len = 3u8;
        let crc_input = [len, device_id, power_h, power_l];
        let crc = crc16_modbus(&crc_input);

        let mut frame = Vec::new();
        frame.extend_from_slice(&FRAME_HEADER);
        frame.extend_from_slice(&crc_input);
        frame.push((crc & 0xFF) as u8);
        frame.push((crc >> 8) as u8);

        let resp = parse_response(&frame).unwrap();
        assert_eq!(resp.device_id, 0x01);
        assert_eq!(resp.cmd, None);
        assert_eq!(resp.power_mw, 1500);
    }

    #[test]
    fn test_parse_response_len4() {
        // Response: ID + CMD + powerH + powerL
        let device_id = 0x02u8;
        let cmd = 0x01u8;
        let power: u16 = 2700;
        let power_h = (power >> 8) as u8;
        let power_l = (power & 0xFF) as u8;

        let len = 4u8;
        let crc_input = [len, device_id, cmd, power_h, power_l];
        let crc = crc16_modbus(&crc_input);

        let mut frame = Vec::new();
        frame.extend_from_slice(&FRAME_HEADER);
        frame.extend_from_slice(&crc_input);
        frame.push((crc & 0xFF) as u8);
        frame.push((crc >> 8) as u8);

        let resp = parse_response(&frame).unwrap();
        assert_eq!(resp.device_id, 0x02);
        assert_eq!(resp.cmd, Some(0x01));
        assert_eq!(resp.power_mw, 2700);
    }

    #[test]
    fn test_parse_response_bad_crc() {
        let frame = [0x5A, 0xA5, 0x03, 0x01, 0x05, 0xDC, 0x00, 0x00];
        assert!(parse_response(&frame).is_err());
    }

    #[test]
    fn test_parse_response_too_short() {
        let frame = [0x5A, 0xA5, 0x02, 0x01];
        assert!(parse_response(&frame).is_err());
    }

    #[test]
    fn test_params_config_defaults() {
        let json = r#"{}"#;
        let params: Aether485ParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.device, "/dev/ttyAP0");
        assert_eq!(params.baud_rate, 115_200);
        assert_eq!(params.timeout_ms, 1000);
        assert_eq!(params.retry_count, 2);

        let cfg = params.to_channel_config();
        assert_eq!(cfg.baud_rate, 115_200);
    }

    #[test]
    fn test_point_mapping_parse() {
        let json = r#"{"device_id": 1}"#;
        let mapping: Aether485PointMapping = serde_json::from_str(json).unwrap();
        assert_eq!(mapping.device_id, 1);
        assert_eq!(mapping.cmd, CMD_QUERY_DEVICE);

        let json2 = r#"{"device_id": 3, "cmd": 1}"#;
        let mapping2: Aether485PointMapping = serde_json::from_str(json2).unwrap();
        assert_eq!(mapping2.device_id, 3);
        assert_eq!(mapping2.cmd, 1);
    }
}
