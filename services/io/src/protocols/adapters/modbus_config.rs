//! Modbus configuration types and constants.
//!
//! Contains all configuration structs, serde helpers, and builder patterns
//! for Modbus TCP/RTU channel setup.

use std::time::Duration;

use serde::Deserialize;

use crate::protocols::core::point::{ByteOrder, DataFormat, PointConfig};

// ============================================================================
// Constants
// ============================================================================

/// Default connection timeout in milliseconds
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5000;

/// Default I/O operation timeout in milliseconds
pub const DEFAULT_IO_TIMEOUT_MS: u64 = 3000;

/// Default maximum registers per batch read
pub const DEFAULT_MAX_BATCH_SIZE: u16 = 64;

/// Default maximum gap between registers to allow merging
pub const DEFAULT_MAX_GAP: u16 = 10;

/// Default reconnect cooldown in milliseconds (60 seconds)
pub const DEFAULT_RECONNECT_COOLDOWN_MS: u64 = 60_000;

/// Default maximum reconnect attempts (0 = unlimited)
pub const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 0;

/// Default consecutive zero-data cycles before triggering reconnect
pub const DEFAULT_ZERO_DATA_THRESHOLD: u32 = 5;

// ============================================================================
// ReconnectConfig
// ============================================================================

/// Reconnect configuration for automatic connection recovery.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Cooldown period after disconnect before reconnect attempts (in ms)
    pub cooldown_ms: u64,
    /// Maximum reconnect attempts (0 = unlimited)
    pub max_attempts: u32,
    /// Consecutive zero-data polling cycles before triggering reconnect
    pub zero_data_threshold: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            cooldown_ms: DEFAULT_RECONNECT_COOLDOWN_MS,
            max_attempts: DEFAULT_MAX_RECONNECT_ATTEMPTS,
            zero_data_threshold: DEFAULT_ZERO_DATA_THRESHOLD,
        }
    }
}

impl ReconnectConfig {
    /// Create a new reconnect configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set cooldown period.
    pub fn with_cooldown_ms(mut self, ms: u64) -> Self {
        self.cooldown_ms = ms;
        self
    }

    /// Set maximum reconnect attempts.
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Set zero-data threshold.
    pub fn with_zero_data_threshold(mut self, threshold: u32) -> Self {
        self.zero_data_threshold = threshold;
        self
    }
}

// ============================================================================
// ConnectionMode
// ============================================================================

/// Connection mode for Modbus channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionMode {
    /// TCP/IP connection (default)
    #[default]
    Tcp,
    /// RTU serial port connection
    #[cfg(feature = "modbus")]
    Rtu,
}

// ============================================================================
// ModbusMappingConfig (JSON deserialization)
// ============================================================================

/// Modbus point mapping configuration (deserialized from protocol_mappings JSON).
///
/// # Required Fields
/// - `register_address`: The Modbus register address (0-based).
///
/// # Optional Fields
/// - `slave_id`: Unit/slave ID (default: 1)
/// - `function_code`: Modbus function code (default: 3 = holding registers)
/// - `data_type`: Data format (default: uint16)
/// - `byte_order`: Byte order for multi-byte values (default: ABCD)
/// - `bit_position`: Bit position for boolean extraction from register (0-15)
#[derive(Debug, Clone, Deserialize)]
pub struct ModbusMappingConfig {
    #[serde(default = "default_slave_id")]
    pub slave_id: u8,

    #[serde(default = "default_function_code")]
    pub function_code: u8,

    /// Register address (0-based). **Required field**.
    pub register_address: u16,

    #[serde(default)]
    pub data_type: DataFormat,

    #[serde(default)]
    pub byte_order: ByteOrder,

    #[serde(default)]
    pub bit_position: Option<u8>,
}

fn default_slave_id() -> u8 {
    1
}

fn default_function_code() -> u8 {
    3
}

// ============================================================================
// ModbusChannelParamsConfig (JSON deserialization)
// ============================================================================

/// Modbus channel parameters configuration (deserialized from parameters_json).
#[derive(Debug, Clone, Deserialize)]
pub struct ModbusChannelParamsConfig {
    #[serde(default)]
    pub host: Option<String>,

    #[serde(default = "default_modbus_port")]
    pub port: u16,

    #[serde(default)]
    pub device: Option<String>,

    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,

    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    #[serde(default = "default_io_timeout_ms")]
    pub io_timeout_ms: u64,

    #[serde(default = "default_max_batch_size_config")]
    pub max_batch_size: u16,

    #[serde(default = "default_max_gap_config")]
    pub max_gap: u16,
}

fn default_modbus_port() -> u16 {
    502
}

fn default_baud_rate() -> u32 {
    9600
}

fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_MS
}

fn default_io_timeout_ms() -> u64 {
    DEFAULT_IO_TIMEOUT_MS
}

fn default_max_batch_size_config() -> u16 {
    64
}

fn default_max_gap_config() -> u16 {
    10
}

impl ModbusChannelParamsConfig {
    pub fn is_tcp(&self) -> bool {
        self.host.is_some()
    }

    pub fn tcp_address(&self) -> Option<String> {
        self.host.as_ref().map(|h| format!("{}:{}", h, self.port))
    }

    /// Convert to ModbusChannelConfig.
    ///
    /// Note: Points must be set separately via `with_points()`.
    pub fn to_channel_config(&self) -> ModbusChannelConfig {
        if self.is_tcp() {
            ModbusChannelConfig::tcp(self.tcp_address().unwrap_or_default())
                .with_connect_timeout(Duration::from_millis(self.connect_timeout_ms))
                .with_io_timeout(Duration::from_millis(self.io_timeout_ms))
                .with_max_batch_size(self.max_batch_size)
                .with_max_gap(self.max_gap)
        } else if let Some(device) = &self.device {
            ModbusChannelConfig::rtu(device, self.baud_rate)
                .with_io_timeout(Duration::from_millis(self.io_timeout_ms))
                .with_max_batch_size(self.max_batch_size)
                .with_max_gap(self.max_gap)
        } else {
            ModbusChannelConfig::tcp("")
        }
    }
}

// ============================================================================
// ModbusChannelConfig (builder pattern)
// ============================================================================

/// Modbus channel configuration.
#[derive(Debug, Clone)]
pub struct ModbusChannelConfig {
    /// Connection mode (TCP or RTU)
    pub connection_mode: ConnectionMode,
    /// Target address for TCP (e.g., "192.168.1.100:502")
    pub address: String,
    /// Connection timeout (TCP only)
    pub connect_timeout: Duration,
    /// I/O operation timeout
    pub io_timeout: Duration,
    /// RTU serial device path (e.g., "/dev/ttyUSB0")
    #[cfg(feature = "modbus")]
    pub rtu_device: String,
    /// RTU baud rate (e.g., 9600, 19200, 115200)
    #[cfg(feature = "modbus")]
    pub baud_rate: u32,
    /// Point configurations
    pub points: Vec<PointConfig>,
    /// Maximum registers per batch read (default: 125)
    pub max_batch_size: u16,
    /// Maximum gap between registers to allow merging (default: 10)
    pub max_gap: u16,
    /// Reconnect configuration
    pub reconnect: ReconnectConfig,
}

impl ModbusChannelConfig {
    /// Create a TCP configuration.
    pub fn tcp(address: impl Into<String>) -> Self {
        Self {
            connection_mode: ConnectionMode::Tcp,
            address: address.into(),
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            io_timeout: Duration::from_millis(DEFAULT_IO_TIMEOUT_MS),
            #[cfg(feature = "modbus")]
            rtu_device: String::new(),
            #[cfg(feature = "modbus")]
            baud_rate: 9600,
            points: Vec::new(),
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_gap: DEFAULT_MAX_GAP,
            reconnect: ReconnectConfig::default(),
        }
    }

    /// Create an RTU (serial) configuration.
    #[cfg(feature = "modbus")]
    pub fn rtu(device: impl Into<String>, baud_rate: u32) -> Self {
        Self {
            connection_mode: ConnectionMode::Rtu,
            address: String::new(),
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            io_timeout: Duration::from_millis(DEFAULT_IO_TIMEOUT_MS),
            rtu_device: device.into(),
            baud_rate,
            points: Vec::new(),
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_gap: DEFAULT_MAX_GAP,
            reconnect: ReconnectConfig::default(),
        }
    }

    /// Set connection timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set I/O timeout.
    pub fn with_io_timeout(mut self, timeout: Duration) -> Self {
        self.io_timeout = timeout;
        self
    }

    /// Add point configurations.
    pub fn with_points(mut self, points: Vec<PointConfig>) -> Self {
        self.points = points;
        self
    }

    /// Set maximum batch size for register reads.
    pub fn with_max_batch_size(mut self, size: u16) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Set maximum gap for merging consecutive registers.
    pub fn with_max_gap(mut self, gap: u16) -> Self {
        self.max_gap = gap;
        self
    }

    /// Set reconnect configuration.
    pub fn with_reconnect(mut self, config: ReconnectConfig) -> Self {
        self.reconnect = config;
        self
    }
}
