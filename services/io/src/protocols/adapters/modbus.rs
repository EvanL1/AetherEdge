//! Modbus protocol adapter.
//!
//! This module provides the `ModbusChannel` adapter that integrates
//! `voltage_modbus` with the protocol layer's `Protocol` and `ProtocolClient` traits.
//!
//! # Module structure
//!
//! - `modbus_config` — Configuration types, constants, builder patterns
//! - `modbus_client` — TCP/RTU client wrapper (transport dispatch)
//! - `modbus_logging` — Raw packet logging bridge
//! - `modbus_poll` — Polling read path (batch register reading)

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tracing::debug;
use voltage_modbus::{ModbusTcpClient, TcpTransport};

#[cfg(feature = "modbus")]
use voltage_modbus::{ModbusRtuClient, RtuTransport};

use aether_model::PointType;

use crate::protocols::core::data::{DataBatch, Value};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::logging::{
    ChannelLogConfig, ChannelLogHandler, ErrorContext, LogContext, LoggableProtocol,
    ModbusTransportType,
};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

use crate::protocols::adapters::command_batcher::{BatchCommand, CommandBatcher};
use crate::protocols::core::point::{PointConfig, ProtocolAddress};
use crate::protocols::core::traits::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, DataEventReceiver,
    Diagnostics, PointFailure, PollResult, Protocol, ProtocolCapabilities, ProtocolClient,
    WriteResult,
};
use crate::protocols::gateway::ChannelRuntime;
use async_trait::async_trait;

// Re-export from extracted modules (preserves external API)
use super::modbus_client::ModbusClientWrapper;
pub use super::modbus_config::{
    ConnectionMode, ModbusChannelConfig, ModbusChannelParamsConfig, ModbusMappingConfig,
    ReconnectConfig,
};
use super::modbus_logging::create_packet_callback;

// Type alias for grouped points: (slave_id, function_code) -> Arc<Vec<PointConfig>>
type GroupedPoints = HashMap<(u8, u8), Arc<Vec<PointConfig>>>;

/// Default polling interval in milliseconds
const DEFAULT_POLLING_INTERVAL_MS: u64 = 1000;

// ============================================================================
// ModbusChannel
// ============================================================================

/// Modbus channel adapter.
///
/// Wraps a `voltage_modbus` client and implements the protocol layer's
/// `Protocol` and `ProtocolClient` traits. Pure protocol implementation
/// that handles device communication — data storage belongs to the service layer.
pub struct ModbusChannel {
    config: ModbusChannelConfig,
    channel_id: u32,
    name: String,
    client: Option<ModbusClientWrapper>,
    state: Arc<std::sync::RwLock<ConnectionState>>,
    diagnostics: Arc<AtomicDiagnostics>,

    // === Polling support ===
    grouped_points: OnceLock<GroupedPoints>,
    polling_interval_ms: u64,

    // === Command batching ===
    command_batcher: CommandBatcher,

    // === Logging ===
    log_context: Arc<LogContext>,
    current_group_id: Arc<std::sync::atomic::AtomicU32>,
}

impl ModbusChannel {
    /// Create a new Modbus channel.
    pub fn new(config: ModbusChannelConfig, channel_id: u32, name: String) -> Self {
        Self {
            config,
            channel_id,
            name,
            client: None,
            state: Arc::new(std::sync::RwLock::new(ConnectionState::Disconnected)),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            grouped_points: OnceLock::new(),
            polling_interval_ms: DEFAULT_POLLING_INTERVAL_MS,
            command_batcher: CommandBatcher::new(),
            log_context: Arc::new(LogContext::new(channel_id)),
            current_group_id: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Set polling interval.
    pub fn with_polling_interval(mut self, interval_ms: u64) -> Self {
        self.polling_interval_ms = interval_ms;
        self
    }

    fn set_state(&self, state: ConnectionState) {
        if let Ok(mut s) = self.state.write() {
            *s = state;
        }
    }

    fn get_state(&self) -> ConnectionState {
        self.state
            .read()
            .map(|s| *s)
            .unwrap_or(ConnectionState::Error)
    }

    /// Get the point configurations.
    pub fn points(&self) -> &[PointConfig] {
        &self.config.points
    }

    fn record_error(&self, error: String) {
        self.diagnostics.record_error(error);
    }

    /// Pre-group points by (slave_id, function_code) for polling optimization.
    fn get_or_init_grouped_points(&self) -> &GroupedPoints {
        self.grouped_points.get_or_init(|| {
            let mut temp_groups: HashMap<(u8, u8), Vec<PointConfig>> = HashMap::new();

            for point in &self.config.points {
                if let ProtocolAddress::Modbus(addr) = &point.address {
                    let key = (addr.slave_id, addr.function_code);
                    temp_groups.entry(key).or_default().push(point.clone());
                }
            }

            for points in temp_groups.values_mut() {
                points.sort_by_key(|p| {
                    if let ProtocolAddress::Modbus(addr) = &p.address {
                        addr.register
                    } else {
                        u16::MAX
                    }
                });
            }

            let groups: GroupedPoints = temp_groups
                .into_iter()
                .map(|(k, v)| (k, Arc::new(v)))
                .collect();

            debug!(
                "[{}] grouped {} points into {} groups",
                self.config.address,
                self.config.points.len(),
                groups.len()
            );

            groups
        })
    }
}

// ============================================================================
// Command Batching Support
// ============================================================================

impl ModbusChannel {
    /// Execute all pending batched commands with FC16 optimization.
    pub async fn execute_batched_commands(&mut self) -> Result<WriteResult> {
        let batches = self.command_batcher.take_commands();

        if batches.is_empty() {
            return Ok(WriteResult {
                success_count: 0,
                failures: Vec::new(),
            });
        }

        let total_commands: usize = batches.values().map(|v| v.len()).sum();
        let mut success_count = 0;
        let mut failures = Vec::with_capacity(total_commands);
        let client = self.client.as_mut().ok_or(GatewayError::NotConnected)?;

        for ((slave_id, fc), commands) in batches {
            if fc == 16 && commands.len() > 1 && CommandBatcher::are_strictly_consecutive(&commands)
            {
                match execute_merged_fc16(client, slave_id, &commands, &mut failures).await {
                    Ok(count) => success_count += count,
                    Err(e) => {
                        let err_msg = format!(
                            "FC16 slave {} regs [{}-{}]: {}",
                            slave_id,
                            commands[0].register_address,
                            commands.last().map(|c| c.register_address).unwrap_or(0),
                            e
                        );
                        for cmd in &commands {
                            failures.push((cmd.point_id, err_msg.clone()));
                        }
                    },
                }
            } else {
                for cmd in commands {
                    match write_single_command(client, &cmd).await {
                        Ok(_) => success_count += 1,
                        Err(msg) => failures.push((cmd.point_id, msg)),
                    }
                }
            }
        }

        self.diagnostics.add_write(success_count as u64);
        if !failures.is_empty() {
            self.diagnostics.add_error(failures.len() as u64);
        }

        Ok(WriteResult {
            success_count,
            failures,
        })
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl HasMetadata for ModbusChannel {
    #[allow(clippy::disallowed_methods)]
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "modbus_tcp",
            display_name: "Modbus TCP",
            description: "Industrial Modbus TCP protocol for reading/writing registers and coils.",
            is_recommended: true,
            example_config: serde_json::json!({
                "host": "192.168.1.100",
                "port": 502,
                "slave_id": 1,
                "connect_timeout_ms": 5000,
                "read_timeout_ms": 3000,
                "polling_interval_ms": 1000,
                "max_batch_size": 64,
                "max_reconnect_attempts": 5
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "host",
                    "Host",
                    "Modbus device IP address or hostname",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "port",
                    "Port",
                    "Modbus TCP port",
                    ParameterType::Integer,
                    serde_json::json!(502),
                ),
                ParameterMetadata::optional(
                    "slave_id",
                    "Slave ID",
                    "Modbus slave/unit ID (1-247)",
                    ParameterType::Integer,
                    serde_json::json!(1),
                ),
                ParameterMetadata::optional(
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "Connection timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(5000),
                ),
                ParameterMetadata::optional(
                    "read_timeout_ms",
                    "Read Timeout (ms)",
                    "Read operation timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(3000),
                ),
                ParameterMetadata::optional(
                    "polling_interval_ms",
                    "Polling Interval (ms)",
                    "Polling interval in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(1000),
                ),
                ParameterMetadata::optional(
                    "max_batch_size",
                    "Max Batch Size",
                    "Maximum registers per batch read (default 64, max 125)",
                    ParameterType::Integer,
                    serde_json::json!(64),
                ),
                ParameterMetadata::optional(
                    "max_reconnect_attempts",
                    "Max Reconnect Attempts",
                    "Maximum reconnection attempts before giving up",
                    ParameterType::Integer,
                    serde_json::json!(5),
                ),
            ],
        }
    }
}

impl ProtocolCapabilities for ModbusChannel {
    fn name(&self) -> &'static str {
        match self.config.connection_mode {
            ConnectionMode::Tcp => "Modbus TCP",
            #[cfg(feature = "modbus")]
            ConnectionMode::Rtu => "Modbus RTU",
        }
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::Polling]
    }

    fn version(&self) -> &'static str {
        "1.0"
    }
}

impl LoggableProtocol for ModbusChannel {
    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        if let Some(ctx) = Arc::get_mut(&mut self.log_context) {
            ctx.set_handler(handler);
        } else {
            let mut new_ctx = (*self.log_context).clone();
            new_ctx.set_handler(handler);
            self.log_context = Arc::new(new_ctx);
        }
    }

    fn set_log_config(&mut self, config: ChannelLogConfig) {
        if let Some(ctx) = Arc::get_mut(&mut self.log_context) {
            ctx.set_config(config);
        } else {
            let mut new_ctx = (*self.log_context).clone();
            new_ctx.set_config(config);
            self.log_context = Arc::new(new_ctx);
        }
    }

    fn log_config(&self) -> &ChannelLogConfig {
        self.log_context.config()
    }
}

impl Protocol for ModbusChannel {
    fn connection_state(&self) -> ConnectionState {
        self.get_state()
    }

    #[allow(clippy::disallowed_methods)]
    async fn diagnostics(&self) -> Result<Diagnostics> {
        let state = self.get_state();

        Ok(Diagnostics {
            protocol: ProtocolCapabilities::name(self).to_string(),
            connection_state: state,
            read_count: self.diagnostics.read_count(),
            write_count: self.diagnostics.write_count(),
            error_count: self.diagnostics.error_count(),
            last_error: self.diagnostics.last_error(),
            extra: serde_json::json!({
                "address": self.config.address,
                "points": self.config.points.len(),
            }),
        })
    }
}

impl ProtocolClient for ModbusChannel {
    async fn connect(&mut self) -> Result<()> {
        let start_time = std::time::Instant::now();
        let old_state = self.get_state();
        self.set_state(ConnectionState::Connecting);

        self.log_context
            .log_state_changed(old_state, ConnectionState::Connecting)
            .await;

        let endpoint: std::borrow::Cow<'_, str> = match self.config.connection_mode {
            ConnectionMode::Tcp => std::borrow::Cow::Borrowed(&self.config.address),
            #[cfg(feature = "modbus")]
            ConnectionMode::Rtu => std::borrow::Cow::Owned(format!(
                "{}@{}",
                self.config.rtu_device, self.config.baud_rate
            )),
        };

        let connect_result = self.create_client().await;
        let duration_ms = start_time.elapsed().as_millis() as u64;

        match connect_result {
            Ok(wrapper) => {
                self.client = Some(wrapper);
                self.set_state(ConnectionState::Connected);

                self.log_context
                    .log_connected(&*endpoint, duration_ms)
                    .await;
                self.log_context
                    .log_state_changed(ConnectionState::Connecting, ConnectionState::Connected)
                    .await;

                Ok(())
            },
            Err(e) => {
                self.set_state(ConnectionState::Error);
                let err_msg = e.to_string();
                self.record_error(err_msg.clone());

                self.log_context
                    .log_error(&err_msg, ErrorContext::Connection)
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

        if let Some(mut client) = self.client.take() {
            let _ = client.close().await;
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

        if self.client.is_none() {
            self.log_context
                .log_error("Not connected", ErrorContext::Polling)
                .await;
            let failures: Vec<_> = self
                .config
                .points
                .iter()
                .map(|p| PointFailure::new(p.id, "Not connected"))
                .collect();
            return PollResult::failed(failures);
        }

        let groups: Vec<_> = self
            .get_or_init_grouped_points()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();

        let Some(client) = self.client.as_mut() else {
            self.log_context
                .log_error("Client unavailable after check", ErrorContext::Polling)
                .await;
            let failures: Vec<_> = self
                .config
                .points
                .iter()
                .map(|p| PointFailure::new(p.id, "Client unavailable"))
                .collect();
            return PollResult::failed(failures);
        };

        let mut batch = DataBatch::default();
        let mut read_count = 0u64;
        let mut error_count = 0u64;

        let total_points: usize = groups.iter().map(|(_, pts)| pts.len()).sum();
        let mut failures = Vec::with_capacity(total_points);

        for ((_slave_id, _fc), points) in groups.iter() {
            let group_id = self
                .current_group_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;

            let results = super::modbus_poll::read_point_group(
                client,
                points,
                self.config.max_batch_size,
                self.config.max_gap,
            )
            .await;

            if results.is_empty() && !points.is_empty() {
                error_count += 1;
                for point in points.iter() {
                    failures.push(PointFailure::new(point.id, "Read failed - no response"));
                }
            }

            if !results.is_empty() {
                let point_summaries: Vec<(u32, aether_model::PointType, Value, _)> = results
                    .iter()
                    .map(|(_, dp)| (dp.id, dp.point_type, dp.value.clone(), dp.quality))
                    .collect();
                self.log_context
                    .log_point_values(&point_summaries, Some(group_id))
                    .await;
            }

            for (_point_id, data_point) in results {
                batch.add(data_point);
                read_count += 1;
            }
        }

        self.diagnostics.add_read(read_count);
        self.diagnostics.add_error(error_count);

        let duration_ms = start_time.elapsed().as_millis() as u64;

        debug!(
            "[{}] poll_once: read {} points, {} failures",
            self.config.address,
            batch.len(),
            failures.len()
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

    async fn write_control(&mut self, commands: &[ControlCommand]) -> Result<WriteResult> {
        let start_time = std::time::Instant::now();
        let mut success_count = 0;
        let mut failures: Vec<(u32, String)> = Vec::with_capacity(commands.len());

        let client = match self.client.as_mut() {
            Some(c) => c,
            None => {
                let err = GatewayError::NotConnected;
                self.log_context
                    .log_control_write(
                        commands,
                        Err(err.to_string()),
                        start_time.elapsed().as_millis() as u64,
                    )
                    .await;
                return Err(err);
            },
        };

        for cmd in commands {
            let point = match self
                .config
                .points
                .iter()
                .find(|p| p.id == cmd.id && p.point_type == PointType::Control)
            {
                Some(p) => p,
                None => {
                    failures.push((cmd.id, "Point not found".into()));
                    continue;
                },
            };

            let modbus_addr = match &point.address {
                ProtocolAddress::Modbus(addr) => addr,
                _ => {
                    failures.push((cmd.id, "Invalid address type".into()));
                    continue;
                },
            };

            let value = point.transform.apply_bool(cmd.value);

            let result = match modbus_addr.function_code {
                5 => {
                    client
                        .write_05(modbus_addr.slave_id, modbus_addr.register, value)
                        .await
                },
                6 => {
                    let reg_value = if value { 1u16 } else { 0u16 };
                    client
                        .write_06(modbus_addr.slave_id, modbus_addr.register, reg_value)
                        .await
                },
                16 => {
                    let reg_value = if value { 1u16 } else { 0u16 };
                    client
                        .write_10(modbus_addr.slave_id, modbus_addr.register, &[reg_value])
                        .await
                },
                fc => {
                    failures.push((
                        cmd.id,
                        format!("Unsupported function code {} for control", fc),
                    ));
                    continue;
                },
            };

            match result {
                Ok(_) => success_count += 1,
                Err(e) => failures.push((
                    cmd.id,
                    format!(
                        "Control write slave {} reg {}: {}",
                        modbus_addr.slave_id, modbus_addr.register, e
                    ),
                )),
            }
        }

        self.diagnostics.add_write(success_count as u64);
        let error_count = failures.len();
        if error_count > 0 {
            self.diagnostics.add_error(error_count as u64);
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let result = WriteResult {
            success_count,
            failures,
        };

        self.log_context
            .log_control_write(commands, Ok(result.clone()), duration_ms)
            .await;

        Ok(result)
    }

    async fn write_adjustment(&mut self, adjustments: &[AdjustmentCommand]) -> Result<WriteResult> {
        let start_time = std::time::Instant::now();
        let mut success_count = 0;
        let mut failures: Vec<(u32, String)> = Vec::with_capacity(adjustments.len());

        let client = match self.client.as_mut() {
            Some(c) => c,
            None => {
                let err = GatewayError::NotConnected;
                self.log_context
                    .log_adjustment_write(
                        adjustments,
                        Err(err.to_string()),
                        start_time.elapsed().as_millis() as u64,
                    )
                    .await;
                return Err(err);
            },
        };

        for adj in adjustments {
            let point = match self
                .config
                .points
                .iter()
                .find(|p| p.id == adj.id && p.point_type == PointType::Adjustment)
            {
                Some(p) => p,
                None => {
                    failures.push((adj.id, "Point not found".into()));
                    continue;
                },
            };

            let modbus_addr = match &point.address {
                ProtocolAddress::Modbus(addr) => addr,
                _ => {
                    failures.push((adj.id, "Invalid address type".into()));
                    continue;
                },
            };

            let raw_value = match reverse_transform(adj.value, &point.transform) {
                Ok(v) => v,
                Err(e) => {
                    failures.push((adj.id, e.to_string()));
                    continue;
                },
            };

            match write_single_value(client, modbus_addr, raw_value).await {
                Ok(_) => success_count += 1,
                Err(msg) => failures.push((adj.id, msg)),
            }
        }

        self.diagnostics.add_write(success_count as u64);
        let error_count = failures.len();
        if error_count > 0 {
            self.diagnostics.add_error(error_count as u64);
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let result = WriteResult {
            success_count,
            failures,
        };

        self.log_context
            .log_adjustment_write(adjustments, Ok(result.clone()), duration_ms)
            .await;

        Ok(result)
    }
}

// ============================================================================
// Connection helpers (extracted from connect() to reduce function length)
// ============================================================================

impl ModbusChannel {
    /// Create a Modbus client based on the configured connection mode.
    async fn create_client(&self) -> Result<ModbusClientWrapper> {
        match self.config.connection_mode {
            ConnectionMode::Tcp => self.create_tcp_client().await,
            #[cfg(feature = "modbus")]
            ConnectionMode::Rtu => self.create_rtu_client(),
        }
    }

    async fn create_tcp_client(&self) -> Result<ModbusClientWrapper> {
        let socket_addr: std::net::SocketAddr = self
            .config
            .address
            .parse()
            .map_err(|e| GatewayError::Connection(format!("Invalid address: {}", e)))?;

        match TcpTransport::new(socket_addr, self.config.connect_timeout).await {
            Ok(mut transport) => {
                let callback = create_packet_callback(
                    self.log_context.clone(),
                    ModbusTransportType::Tcp,
                    self.current_group_id.clone(),
                );
                transport.set_packet_callback(callback);
                let client = ModbusTcpClient::from_transport(transport);
                Ok(ModbusClientWrapper::Tcp(client))
            },
            Err(e) => Err(GatewayError::Connection(e.to_string())),
        }
    }

    #[cfg(feature = "modbus")]
    fn create_rtu_client(&self) -> Result<ModbusClientWrapper> {
        match RtuTransport::new(&self.config.rtu_device, self.config.baud_rate) {
            Ok(mut transport) => {
                let callback = create_packet_callback(
                    self.log_context.clone(),
                    ModbusTransportType::Rtu,
                    self.current_group_id.clone(),
                );
                transport.set_packet_callback(callback);
                let client = ModbusRtuClient::from_transport(transport);
                Ok(ModbusClientWrapper::Rtu(client))
            },
            Err(e) => Err(GatewayError::Connection(e.to_string())),
        }
    }
}

// ============================================================================
// Write helpers (deduplicated from execute_batched_commands + write_adjustment)
// ============================================================================

/// Write a single encoded value to a Modbus register.
///
/// Uses FC06 for single-register formats, FC10 for multi-register.
async fn write_single_value(
    client: &mut ModbusClientWrapper,
    addr: &crate::protocols::core::point::ModbusAddress,
    raw_value: f64,
) -> std::result::Result<(), String> {
    let regs = encode_value(raw_value, addr.format, addr.byte_order).map_err(|e| {
        format!(
            "Encode error for slave {} reg {}: {}",
            addr.slave_id, addr.register, e
        )
    })?;

    write_registers(client, addr.slave_id, addr.register, &regs)
        .await
        .map_err(|e| format!("Write slave {} reg {}: {}", addr.slave_id, addr.register, e))
}

/// Write a single batch command (used in execute_batched_commands individual path).
async fn write_single_command(
    client: &mut ModbusClientWrapper,
    cmd: &BatchCommand,
) -> std::result::Result<(), String> {
    // Refuse to encode a non-numeric command value into register 0 — silent
    // 0 writes to a Modbus setpoint can stop a generator, open a contactor,
    // or otherwise act as a destructive command. Caller surfaces this as a
    // command failure to the requester (automation → upstream).
    let raw_value = cmd.value.as_f64().ok_or_else(|| {
        format!(
            "Refusing write: non-numeric command value {:?} for slave {} reg {}",
            cmd.value, cmd.slave_id, cmd.register_address
        )
    })?;
    let regs = encode_value(raw_value, cmd.data_format, cmd.byte_order).map_err(|e| {
        format!(
            "Encode error for slave {} reg {}: {}",
            cmd.slave_id, cmd.register_address, e
        )
    })?;

    write_registers(client, cmd.slave_id, cmd.register_address, &regs)
        .await
        .map_err(|e| {
            format!(
                "Write slave {} reg {}: {}",
                cmd.slave_id, cmd.register_address, e
            )
        })
}

/// FC06 for single register, FC10 for multiple registers.
async fn write_registers(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    address: u16,
    regs: &[u16],
) -> voltage_modbus::ModbusResult<()> {
    if regs.len() == 1 {
        client.write_06(slave_id, address, regs[0]).await
    } else {
        client.write_10(slave_id, address, regs).await
    }
}

/// Execute merged FC16 (Write Multiple Registers) for consecutive addresses.
async fn execute_merged_fc16(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    commands: &[BatchCommand],
    failures: &mut Vec<(u32, String)>,
) -> std::result::Result<usize, voltage_modbus::ModbusError> {
    let mut indices: Vec<usize> = (0..commands.len()).collect();
    indices.sort_by_key(|&i| commands[i].register_address);

    let total_regs: usize = commands
        .iter()
        .map(|c| c.data_format.register_count() as usize)
        .sum();

    let start_addr = commands[indices[0]].register_address;
    let mut registers = Vec::with_capacity(total_regs);

    for &i in &indices {
        let cmd = &commands[i];
        // Same as write_single_command: never silently encode a non-numeric
        // command as register 0. Abort the whole merged batch — partial
        // writes from a multi-register FC16 with one bad value are worse
        // than skipping all of them.
        let raw_value = match cmd.value.as_f64() {
            Some(v) => v,
            None => {
                for &j in &indices {
                    failures.push((
                        commands[j].point_id,
                        format!(
                            "FC16 merge aborted: non-numeric value {:?} at reg {}",
                            cmd.value, cmd.register_address
                        ),
                    ));
                }
                return Ok(0);
            },
        };
        match encode_value(raw_value, cmd.data_format, cmd.byte_order) {
            Ok(regs) => registers.extend(regs),
            Err(e) => {
                for &j in &indices {
                    failures.push((
                        commands[j].point_id,
                        format!(
                            "FC16 merge aborted: encode error at reg {}: {}",
                            cmd.register_address, e
                        ),
                    ));
                }
                return Ok(0);
            },
        }
    }

    if registers.is_empty() {
        return Ok(0);
    }

    debug!(
        "FC16 merge: {} commands → {} registers starting at {}",
        commands.len(),
        registers.len(),
        start_addr
    );

    client.write_10(slave_id, start_addr, &registers).await?;

    Ok(commands.len())
}

// ============================================================================
// Value encoding/transform helpers
// ============================================================================

/// Encode a Value to Modbus registers.
fn encode_value(
    value: f64,
    format: crate::protocols::core::point::DataFormat,
    byte_order: crate::protocols::core::point::ByteOrder,
) -> Result<Vec<u16>> {
    use crate::protocols::codec::byte_order::encode_registers;
    encode_registers(&Value::Float(value), format, byte_order)
}

/// Reverse transform to get raw value.
fn reverse_transform(
    value: f64,
    transform: &crate::protocols::core::point::TransformConfig,
) -> Result<f64> {
    transform.reverse_apply(value)
}

// ============================================================================
// ChannelRuntime implementation
// ============================================================================

#[async_trait]
impl ChannelRuntime for ModbusChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "modbus"
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

    fn subscribe(&self) -> Option<DataEventReceiver> {
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
    use std::time::Duration;

    #[test]
    fn test_modbus_channel_config() {
        let config = ModbusChannelConfig::tcp("127.0.0.1:502")
            .with_connect_timeout(Duration::from_secs(10))
            .with_io_timeout(Duration::from_secs(5));

        assert_eq!(config.address, "127.0.0.1:502");
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
        assert_eq!(config.io_timeout, Duration::from_secs(5));
    }

    #[test]
    fn test_modbus_channel_capabilities() {
        let config = ModbusChannelConfig::tcp("127.0.0.1:502");
        let channel = ModbusChannel::new(config, 1, "test_modbus".to_string());

        assert_eq!(ProtocolCapabilities::name(&channel), "Modbus TCP");
        assert_eq!(channel.supported_modes(), &[CommunicationMode::Polling]);
    }

    #[test]
    fn test_polling_interval_builder() {
        let config = ModbusChannelConfig::tcp("127.0.0.1:502");
        let channel = ModbusChannel::new(config, 1, "test".to_string()).with_polling_interval(500);

        assert_eq!(channel.polling_interval_ms, 500);
    }

    #[test]
    fn test_reconnect_config_defaults() {
        let config = ReconnectConfig::default();

        assert_eq!(config.cooldown_ms, 60_000);
        assert_eq!(config.max_attempts, 0);
        assert_eq!(config.zero_data_threshold, 5);
    }

    #[test]
    fn test_reconnect_config_builder() {
        let config = ReconnectConfig::new()
            .with_cooldown_ms(30_000)
            .with_max_attempts(10)
            .with_zero_data_threshold(3);

        assert_eq!(config.cooldown_ms, 30_000);
        assert_eq!(config.max_attempts, 10);
        assert_eq!(config.zero_data_threshold, 3);
    }

    #[test]
    fn test_modbus_channel_with_reconnect() {
        let reconnect = ReconnectConfig::new().with_cooldown_ms(10_000);
        let config = ModbusChannelConfig::tcp("127.0.0.1:502").with_reconnect(reconnect);

        let channel = ModbusChannel::new(config, 1, "test".to_string());
        assert_eq!(channel.config.reconnect.cooldown_ms, 10_000);
    }

    #[test]
    fn test_tcp_config_connection_mode() {
        let config = ModbusChannelConfig::tcp("192.168.1.100:502");
        assert_eq!(config.connection_mode, ConnectionMode::Tcp);
        assert_eq!(config.address, "192.168.1.100:502");

        let channel = ModbusChannel::new(config, 1, "test_tcp".to_string());
        assert_eq!(ProtocolCapabilities::name(&channel), "Modbus TCP");
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn test_rtu_config() {
        let config = ModbusChannelConfig::rtu("/dev/ttyUSB0", 9600);

        assert_eq!(config.connection_mode, ConnectionMode::Rtu);
        assert_eq!(config.rtu_device, "/dev/ttyUSB0");
        assert_eq!(config.baud_rate, 9600);

        let channel = ModbusChannel::new(config, 1, "test_rtu".to_string());
        assert_eq!(ProtocolCapabilities::name(&channel), "Modbus RTU");
    }
}
