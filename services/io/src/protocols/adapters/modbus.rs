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

use aether_core::PointType;

use crate::protocols::core::data::{DataBatch, Value};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::logging::{
    ChannelLogConfig, ChannelLogHandler, ErrorContext, LogContext, ModbusTransportType,
};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

use crate::protocols::core::point::PointConfig;
use crate::protocols::core::traits::{
    AdjustmentCommand, ConnectionState, ControlCommand, Diagnostics, PointFailure, PollResult,
    WriteResult,
};
use crate::protocols::runtime::ChannelRuntime;
use async_trait::async_trait;

// Re-export from extracted modules (preserves external API)
use super::modbus_client::ModbusClientWrapper;
pub(crate) use super::modbus_config::parse_point_mapping;
pub use super::modbus_config::{
    ConnectionMode, ModbusAddress, ModbusChannelConfig, ModbusMappingConfig, ReconnectConfig,
};
use super::modbus_logging::create_packet_callback;

// Point indices grouped by (slave_id, function_code).
type GroupedPoints = HashMap<(u8, u8), Vec<usize>>;

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
    client: Option<ModbusClientWrapper>,
    state: ConnectionState,
    diagnostics: Arc<AtomicDiagnostics>,

    // === Polling support ===
    grouped_points: OnceLock<GroupedPoints>,
    polling_interval_ms: u64,

    // === Logging ===
    log_context: Arc<LogContext>,
    current_group_id: Arc<std::sync::atomic::AtomicU32>,
}

impl ModbusChannel {
    /// Create a new Modbus channel.
    pub fn new(config: ModbusChannelConfig, channel_id: u32) -> Self {
        Self {
            config,
            client: None,
            state: ConnectionState::Disconnected,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            grouped_points: OnceLock::new(),
            polling_interval_ms: DEFAULT_POLLING_INTERVAL_MS,
            log_context: Arc::new(LogContext::new(channel_id)),
            current_group_id: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Set polling interval.
    pub fn with_polling_interval(mut self, interval_ms: u64) -> Self {
        self.polling_interval_ms = interval_ms;
        self
    }

    /// Get the point configurations.
    pub fn points(&self) -> &[PointConfig<ModbusAddress>] {
        &self.config.points
    }

    fn record_error(&self, error: String) {
        self.diagnostics.record_error(error);
    }

    /// Pre-group points by (slave_id, function_code) for polling optimization.
    fn initialize_grouped_points(&self) {
        self.grouped_points.get_or_init(|| {
            let mut groups: GroupedPoints = HashMap::new();

            for (index, point) in self.config.points.iter().enumerate() {
                let key = (point.address.slave_id, point.address.function_code);
                groups.entry(key).or_default().push(index);
            }

            for indices in groups.values_mut() {
                indices.sort_by_key(|index| self.config.points[*index].address.register);
            }

            debug!(
                "[{}] grouped {} points into {} groups",
                self.config.address,
                self.config.points.len(),
                groups.len()
            );

            groups
        });
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl ModbusChannel {
    /// Exact commissioning metadata for the TCP transport.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn tcp_metadata() -> DriverMetadata {
        DriverMetadata {
            name: "modbus_tcp",
            display_name: "Modbus TCP",
            description: "Industrial Modbus TCP protocol for reading/writing registers and coils.",
            is_recommended: true,
            example_config: serde_json::json!({
                "host": "192.168.1.100",
                "port": 502,
                "read_timeout_ms": 3000,
                "poll_interval_ms": 1000
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "host",
                    "Host",
                    "Modbus device IP address or hostname",
                    ParameterType::String,
                )
                .with_min_length(1),
                ParameterMetadata::required(
                    "port",
                    "Port",
                    "Modbus TCP port (1-65535)",
                    ParameterType::Integer,
                )
                .with_integer_range(1, u64::from(u16::MAX)),
                ParameterMetadata::optional(
                    "read_timeout_ms",
                    "Read Timeout (ms)",
                    "Read operation timeout in milliseconds (1-86400000)",
                    ParameterType::Integer,
                    serde_json::json!(3000),
                )
                .with_integer_range(1, 86_400_000),
            ],
        }
    }

    /// Exact commissioning metadata for the serial RTU transport.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn rtu_metadata() -> DriverMetadata {
        DriverMetadata {
            name: "modbus_rtu",
            display_name: "Modbus RTU",
            description: "Industrial Modbus RTU protocol over a serial device.",
            is_recommended: true,
            example_config: serde_json::json!({
                "device": "/dev/ttyUSB0",
                "baud_rate": 9600,
                "read_timeout_ms": 3000,
                "poll_interval_ms": 1000
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "device",
                    "Serial Device",
                    "Non-empty serial device path",
                    ParameterType::String,
                )
                .with_min_length(1),
                ParameterMetadata::required(
                    "baud_rate",
                    "Baud Rate",
                    "Serial baud rate (1-4294967295)",
                    ParameterType::Integer,
                )
                .with_integer_range(1, u64::from(u32::MAX)),
                ParameterMetadata::optional(
                    "read_timeout_ms",
                    "Read Timeout (ms)",
                    "Read operation timeout in milliseconds (1-86400000)",
                    ParameterType::Integer,
                    serde_json::json!(3000),
                )
                .with_integer_range(1, 86_400_000),
            ],
        }
    }
}

impl HasMetadata for ModbusChannel {
    fn metadata() -> DriverMetadata {
        Self::tcp_metadata()
    }
}

impl ModbusChannel {
    fn name(&self) -> &'static str {
        match self.config.connection_mode {
            ConnectionMode::Tcp => "Modbus TCP",
            #[cfg(feature = "modbus")]
            ConnectionMode::Rtu => "Modbus RTU",
        }
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
// Write helpers
// ============================================================================

/// Write a single encoded value to a Modbus register.
///
/// Uses FC06 for single-register formats, FC10 for multi-register.
async fn write_single_value(
    client: &mut ModbusClientWrapper,
    addr: &ModbusAddress,
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

impl ModbusChannel {
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

            let modbus_addr = &point.address;

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
            .log_adjustment_write(adjustments, Ok(&result), duration_ms)
            .await;

        Ok(result)
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

            let modbus_addr = &point.address;

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
            .log_control_write(commands, Ok(&result), duration_ms)
            .await;

        Ok(result)
    }
}

#[async_trait]
impl ChannelRuntime for ModbusChannel {
    async fn connect(&mut self) -> Result<()> {
        let start_time = std::time::Instant::now();
        let old_state = self.state;
        self.state = ConnectionState::Connecting;

        self.log_context
            .log_state_changed(old_state, ConnectionState::Connecting)
            .await;

        let connect_result = self.create_client().await;
        let duration_ms = start_time.elapsed().as_millis() as u64;

        match connect_result {
            Ok(wrapper) => {
                self.client = Some(wrapper);
                self.state = ConnectionState::Connected;

                let endpoint: std::borrow::Cow<'_, str> = match self.config.connection_mode {
                    ConnectionMode::Tcp => std::borrow::Cow::Borrowed(&self.config.address),
                    #[cfg(feature = "modbus")]
                    ConnectionMode::Rtu => std::borrow::Cow::Owned(format!(
                        "{}@{}",
                        self.config.rtu_device, self.config.baud_rate
                    )),
                };
                self.log_context
                    .log_connected(&*endpoint, duration_ms)
                    .await;
                self.log_context
                    .log_state_changed(ConnectionState::Connecting, ConnectionState::Connected)
                    .await;

                Ok(())
            },
            Err(e) => {
                self.state = ConnectionState::Error;
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
        let old_state = self.state;

        if let Some(mut client) = self.client.take() {
            let _ = client.close().await;
        }
        self.state = ConnectionState::Disconnected;

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

        self.initialize_grouped_points();
        let Some(groups) = self.grouped_points.get() else {
            return PollResult::failed(
                self.config
                    .points
                    .iter()
                    .map(|point| PointFailure::new(point.id, "Point grouping unavailable"))
                    .collect(),
            );
        };

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

        let total_points: usize = groups.values().map(Vec::len).sum();
        let mut failures = Vec::with_capacity(total_points);

        for point_indices in groups.values() {
            let group_id = self
                .current_group_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;

            let results = super::modbus_poll::read_point_group(
                client,
                point_indices,
                &self.config.points,
                self.config.max_batch_size,
                self.config.max_gap,
            )
            .await;

            if results.is_empty() && !point_indices.is_empty() {
                error_count += 1;
                for index in point_indices {
                    failures.push(PointFailure::new(
                        self.config.points[*index].id,
                        "Read failed - no response",
                    ));
                }
            }

            if !results.is_empty() {
                self.log_context
                    .log_point_values(&results, Some(group_id))
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

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let cmds: Vec<_> = commands
            .iter()
            .map(|(id, value)| ControlCommand::latching(*id, *value != 0.0))
            .collect();
        let result = Self::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, value)| AdjustmentCommand::new(*id, *value))
            .collect();
        let result = Self::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let state = self.state;

        Ok(Diagnostics {
            protocol: self.name().to_string(),
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

    fn connection_state(&self) -> ConnectionState {
        self.state
    }

    #[allow(clippy::disallowed_methods)]
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
        let channel = ModbusChannel::new(config, 1);

        assert_eq!(channel.name(), "Modbus TCP");
    }

    #[test]
    fn test_polling_interval_builder() {
        let config = ModbusChannelConfig::tcp("127.0.0.1:502");
        let channel = ModbusChannel::new(config, 1).with_polling_interval(500);

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

        let channel = ModbusChannel::new(config, 1);
        assert_eq!(channel.config.reconnect.cooldown_ms, 10_000);
    }

    #[test]
    fn test_tcp_config_connection_mode() {
        let config = ModbusChannelConfig::tcp("192.168.1.100:502");
        assert_eq!(config.connection_mode, ConnectionMode::Tcp);
        assert_eq!(config.address, "192.168.1.100:502");

        let channel = ModbusChannel::new(config, 1);
        assert_eq!(channel.name(), "Modbus TCP");
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn test_rtu_config() {
        let config = ModbusChannelConfig::rtu("/dev/ttyUSB0", 9600);

        assert_eq!(config.connection_mode, ConnectionMode::Rtu);
        assert_eq!(config.rtu_device, "/dev/ttyUSB0");
        assert_eq!(config.baud_rate, 9600);

        let channel = ModbusChannel::new(config, 1);
        assert_eq!(channel.name(), "Modbus RTU");
    }
}
