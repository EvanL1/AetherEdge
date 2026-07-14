//! Io service configuration structures

use common::serde_helpers::{deserialize_bool_flexible, deserialize_u8_default_zero};
use common::validation::CsvFields;
use common::{
    ApiConfig, BaseServiceConfig, ConfigValidator, LoggingConfig, ValidationLevel, ValidationResult,
};

use aether_schema_macro::Schema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "openapi")]
use utoipa::ToSchema;

/// Default API configuration for io (port 6001)
fn default_io_api() -> ApiConfig {
    ApiConfig {
        host: common::DEFAULT_API_HOST.to_string(),
        port: 6001,
    }
}

/// Io service configuration (internal config, not exposed via API)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoConfig {
    /// Base service configuration
    #[serde(flatten, default)]
    pub service: BaseServiceConfig,

    /// API configuration (has default value)
    #[serde(default = "default_io_api")]
    pub api: ApiConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Channel configurations (wrapped in Arc for cheap cloning during startup)
    #[serde(default)]
    pub channels: Vec<Arc<ChannelConfig>>,
}

/// Service configuration table SQL (from common)
pub use common::SERVICE_CONFIG_TABLE;

/// Sync metadata table SQL (from common)
pub use common::SYNC_METADATA_TABLE;

/// Default port for io service
pub const DEFAULT_PORT: u16 = 6001;

/// Largest accepted channel polling or I/O timeout interval (24 hours).
///
/// Longer work belongs in scheduled automation rather than a protocol polling
/// loop. Keeping this bounded also protects runtime duration calculations.
pub const MAX_CHANNEL_TIMING_MS: u64 = 86_400_000;

/// Channel core fields (shared between Config and API responses)
/// These fields represent the essential channel identity and state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ChannelCore {
    /// Channel ID
    pub id: u32,

    /// Channel name
    pub name: String,

    /// Channel description
    pub description: Option<String>,

    /// Protocol type (modbus, virtual, grpc, etc.)
    pub protocol: String,

    /// Whether the channel is enabled
    #[serde(default)]
    pub enabled: bool,
}

/// Channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ChannelConfig {
    /// Core channel fields
    #[serde(flatten)]
    pub core: ChannelCore,

    /// Protocol-specific parameters
    #[serde(default)]
    pub parameters: HashMap<String, serde_json::Value>,

    /// Channel logging configuration
    #[serde(default)]
    pub logging: ChannelLoggingConfig,
}

fn validate_required_string_parameter(
    channel: &ChannelConfig,
    result: &mut ValidationResult,
    parameter: &str,
) {
    match channel.parameters.get(parameter) {
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {},
        _ => result.add_error(format!(
            "Channel {}: '{parameter}' must be a non-empty string",
            channel.core.name
        )),
    }
}

fn validate_required_integer_parameter(
    channel: &ChannelConfig,
    result: &mut ValidationResult,
    parameter: &str,
    maximum: u64,
) {
    let valid = channel
        .parameters
        .get(parameter)
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|value| (1..=maximum).contains(&value));
    if !valid {
        result.add_error(format!(
            "Channel {}: '{parameter}' must be an integer between 1 and {maximum}",
            channel.core.name
        ));
    }
}

fn validate_optional_timing_parameter(
    channel: &ChannelConfig,
    result: &mut ValidationResult,
    parameter: &str,
) {
    let Some(value) = channel.parameters.get(parameter) else {
        return;
    };
    let valid = value
        .as_u64()
        .is_some_and(|value| (1..=MAX_CHANNEL_TIMING_MS).contains(&value));
    if !valid {
        result.add_error(format!(
            "Channel {}: '{parameter}' must be an integer between 1 and {MAX_CHANNEL_TIMING_MS}",
            channel.core.name
        ));
    }
}

impl ChannelConfig {
    /// Convenient accessor for channel ID
    pub fn id(&self) -> u32 {
        self.core.id
    }

    /// Convenient accessor for channel name
    pub fn name(&self) -> &str {
        &self.core.name
    }

    /// Convenient accessor for protocol
    pub fn protocol(&self) -> &str {
        &self.core.protocol
    }

    /// Convenient accessor for enabled status
    pub fn is_enabled(&self) -> bool {
        self.core.enabled
    }
}

/// Channels table record
/// Stores channel configurations - Maps to ChannelConfig structure
#[allow(dead_code)]
#[derive(Schema)]
#[table(
    name = "channels",
    suffix = "CHECK (TYPEOF(revision) = 'integer' AND revision >= 1)"
)]
struct ChannelRecord {
    #[column(primary_key)]
    channel_id: u32,

    #[column(not_null, unique)]
    name: String,

    protocol: Option<String>,

    #[column(default = "false")]
    enabled: bool,

    config: Option<String>, // JSON TEXT

    /// Monotonic desired-state revision. Runtime writers use this for CAS;
    /// legacy writers are covered by the schema's compatibility trigger.
    #[column(default = "1")]
    revision: i64,

    #[column(default = "CURRENT_TIMESTAMP")]
    created_at: String, // TIMESTAMP type

    #[column(default = "CURRENT_TIMESTAMP")]
    updated_at: String, // TIMESTAMP type
}

/// Channels table SQL (generated by Schema macro)
pub const CHANNELS_TABLE: &str = ChannelRecord::CREATE_TABLE_SQL;

/// Compatibility triggers required after creating [`CHANNELS_TABLE`].
///
/// The generated table DDL cannot carry sibling trigger statements, so schema
/// setup paths must install these immediately after creating the table.
pub use common::test_utils::schema::{
    CHANNEL_REVISION_BUMP_TRIGGER, CHANNEL_REVISION_EXHAUSTED_TRIGGER,
    install_channel_revision_triggers,
};

/// Channel-specific logging configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ChannelLoggingConfig {
    /// Whether logging is enabled for this channel
    #[serde(default)]
    pub enabled: bool,

    /// Log level for this channel
    pub level: Option<String>,

    /// Log file for this channel
    pub file: Option<String>,
}

/// Base point configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct Point {
    /// Point ID
    pub point_id: u32,

    /// Signal name
    pub signal_name: String,

    /// Point description
    pub description: Option<String>,

    /// Unit of measurement
    pub unit: Option<String>,

    /// Protocol-specific mapping data as JSON string
    /// Contains protocol-dependent fields like slave_id, register_address for Modbus
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_mappings: Option<String>,
}

use common::serde_helpers::{deserialize_offset, deserialize_scale, scale_one, step_one};

/// Telemetry point (T)
/// For analog measurements like voltage, current, temperature
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TelemetryPoint {
    /// Base point information
    #[serde(flatten)]
    pub base: Point,

    /// Scale factor for value conversion
    #[serde(default = "scale_one", deserialize_with = "deserialize_scale")]
    pub scale: f64,

    /// Offset for value conversion
    #[serde(default, deserialize_with = "deserialize_offset")]
    pub offset: f64,

    /// Data type (float32, float64, int16, int32, etc.)
    #[serde(default = "default_data_type")]
    pub data_type: String,

    /// Whether to reverse signal logic (not used for telemetry values)
    /// Note: Byte order/endian for multi-byte values is controlled via protocol mappings
    /// using the `byte_order` field, not this flag.
    /// Supports: 1/0, true/false, yes/no in CSV files
    #[serde(default, deserialize_with = "deserialize_bool_flexible")]
    pub reverse: bool,
}

/// Signal point (S)
/// For digital/binary status like on/off, open/close
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct SignalPoint {
    /// Base point information
    #[serde(flatten)]
    pub base: Point,

    /// Whether to reverse the signal logic
    /// Supports: 1/0, true/false, yes/no in CSV files
    #[serde(default, deserialize_with = "deserialize_bool_flexible")]
    pub reverse: bool,
}

/// Control point (C)
/// For remote control commands
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ControlPoint {
    /// Base point information
    #[serde(flatten)]
    pub base: Point,

    /// Whether to reverse the control logic (like SignalPoint)
    /// Supports: 1/0, true/false, yes/no in CSV files
    #[serde(default, deserialize_with = "deserialize_bool_flexible")]
    pub reverse: bool,

    /// Control type (momentary, latching, etc.)
    #[serde(default = "default_control_type")]
    pub control_type: String,

    /// Control value for ON/OPEN command
    #[serde(default = "default_on_value")]
    pub on_value: u16,

    /// Control value for OFF/CLOSE command
    #[serde(default = "default_off_value")]
    pub off_value: u16,

    /// Pulse duration in milliseconds (for momentary controls)
    pub pulse_duration_ms: Option<u32>,
}

/// Adjustment point (A)
/// For remote setpoint adjustments
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct AdjustmentPoint {
    /// Base point information
    #[serde(flatten)]
    pub base: Point,

    /// Minimum allowed value
    pub min_value: Option<f64>,

    /// Maximum allowed value
    pub max_value: Option<f64>,

    /// Step size for adjustments
    #[serde(default = "step_one")]
    pub step: f64,

    /// Data type (float32, float64, int16, int32, etc.)
    #[serde(default = "default_data_type")]
    pub data_type: String,

    /// Scale factor for value conversion
    #[serde(default = "scale_one", deserialize_with = "deserialize_scale")]
    pub scale: f64,

    /// Offset for value conversion
    #[serde(default, deserialize_with = "deserialize_offset")]
    pub offset: f64,
}

/// Telemetry points table record
/// Stores analog measurement points with embedded protocol mappings as JSON
#[allow(dead_code)]
#[derive(Schema)]
#[table(
    name = "telemetry_points",
    suffix = "PRIMARY KEY (channel_id, point_id)"
)]
struct TelemetryPointRecord {
    #[column(not_null)]
    point_id: u32,

    #[column(not_null, references = "channels(channel_id)", on_delete = "CASCADE")]
    channel_id: u32,

    #[column(not_null)]
    signal_name: String,

    #[column(default = "1.0")]
    scale: f64,

    #[column(default = "0.0")]
    offset: f64,

    unit: Option<String>,

    #[column(default = "false")]
    reverse: bool,

    data_type: Option<String>,

    description: Option<String>,

    protocol_mappings: Option<String>, // JSON TEXT
}

/// Signal points table record
/// Stores digital/binary status points with embedded protocol mappings as JSON
#[allow(dead_code)]
#[derive(Schema)]
#[table(name = "signal_points", suffix = "PRIMARY KEY (channel_id, point_id)")]
struct SignalPointRecord {
    #[column(not_null)]
    point_id: u32,

    #[column(not_null, references = "channels(channel_id)", on_delete = "CASCADE")]
    channel_id: u32,

    #[column(not_null)]
    signal_name: String,

    #[column(default = "1.0")]
    scale: f64,

    #[column(default = "0.0")]
    offset: f64,

    unit: Option<String>,

    #[column(default = "false")]
    reverse: bool,

    #[column(default = "0")]
    normal_state: i32,

    data_type: Option<String>,

    description: Option<String>,

    protocol_mappings: Option<String>, // JSON TEXT
}

/// Control points table record
/// Stores remote control command points with embedded protocol mappings as JSON
#[allow(dead_code)]
#[derive(Schema)]
#[table(name = "control_points", suffix = "PRIMARY KEY (channel_id, point_id)")]
struct ControlPointRecord {
    #[column(not_null)]
    point_id: u32,

    #[column(not_null, references = "channels(channel_id)", on_delete = "CASCADE")]
    channel_id: u32,

    #[column(not_null)]
    signal_name: String,

    #[column(default = "1.0")]
    scale: f64,

    #[column(default = "0.0")]
    offset: f64,

    unit: Option<String>,

    #[column(default = "false")]
    reverse: bool,

    data_type: Option<String>,

    description: Option<String>,

    protocol_mappings: Option<String>, // JSON TEXT
}

/// Adjustment points table record
/// Stores remote setpoint adjustment points with embedded protocol mappings as JSON
#[allow(dead_code)]
#[derive(Schema)]
#[table(
    name = "adjustment_points",
    suffix = "PRIMARY KEY (channel_id, point_id)"
)]
struct AdjustmentPointRecord {
    #[column(not_null)]
    point_id: u32,

    #[column(not_null, references = "channels(channel_id)", on_delete = "CASCADE")]
    channel_id: u32,

    #[column(not_null)]
    signal_name: String,

    #[column(default = "1.0")]
    scale: f64,

    #[column(default = "0.0")]
    offset: f64,

    unit: Option<String>,

    #[column(default = "false")]
    reverse: bool,

    data_type: Option<String>,

    description: Option<String>,

    protocol_mappings: Option<String>, // JSON TEXT

    min_value: Option<f64>,

    max_value: Option<f64>,

    #[column(default = "1.0")]
    step: f64,
}

/// Telemetry points table SQL (generated by Schema macro)
pub const TELEMETRY_POINTS_TABLE: &str = TelemetryPointRecord::CREATE_TABLE_SQL;

/// Signal points table SQL (generated by Schema macro)
pub const SIGNAL_POINTS_TABLE: &str = SignalPointRecord::CREATE_TABLE_SQL;

/// Control points table SQL (generated by Schema macro)
pub const CONTROL_POINTS_TABLE: &str = ControlPointRecord::CREATE_TABLE_SQL;

/// Adjustment points table SQL (generated by Schema macro)
pub const ADJUSTMENT_POINTS_TABLE: &str = AdjustmentPointRecord::CREATE_TABLE_SQL;

// ────────────────────── Channel Routing Table ──────────────────────

/// Channel routing table record (C2C routing)
/// Stores direct channel-to-channel data forwarding rules
#[allow(dead_code)]
#[derive(Schema)]
#[table(
    name = "channel_routing",
    suffix = "PRIMARY KEY (source_channel_id, source_type, source_point_id)"
)]
struct ChannelRoutingRecord {
    #[column(not_null, references = "channels(channel_id)")]
    source_channel_id: u32,

    #[column(not_null)]
    source_type: String, // T/S/C/A

    #[column(not_null)]
    source_point_id: u32,

    #[column(not_null, references = "channels(channel_id)")]
    target_channel_id: u32,

    #[column(not_null)]
    target_type: String, // T/S/C/A

    #[column(not_null)]
    target_point_id: u32,

    #[column(default = "true")]
    enabled: bool,

    #[column(default = "1.0")]
    scale: f64,

    #[column(default = "0.0")]
    offset: f64,

    description: Option<String>,

    #[column(default = "CURRENT_TIMESTAMP")]
    created_at: String, // TIMESTAMP type

    #[column(default = "CURRENT_TIMESTAMP")]
    updated_at: String, // TIMESTAMP type
}

// Schema SQL constant
pub const CHANNEL_ROUTING_TABLE: &str = ChannelRoutingRecord::CREATE_TABLE_SQL;

/// Modbus protocol mapping (corresponds to modbus_mappings table)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ModbusMapping {
    #[serde(default)] // channel_id from directory context
    pub channel_id: u32,
    pub point_id: u32,
    #[serde(default)] // telemetry_type from filename context
    pub telemetry_type: String,
    pub slave_id: u8,
    pub function_code: u8,
    pub register_address: u16,
    pub data_type: String,
    pub byte_order: String,
    #[serde(default, deserialize_with = "deserialize_u8_default_zero")]
    pub bit_position: u8,
}

/// GPIO protocol mapping for DI/DO (corresponds to gpio_mappings table)
/// Direction is implicit: Signal=input, Control=output
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct GpioMapping {
    #[serde(default)] // channel_id from directory context
    pub channel_id: u32,
    pub point_id: u32,
    #[serde(default)] // telemetry_type from filename context
    pub telemetry_type: String,
    pub gpio_number: u32,
}

/// Virtual protocol mapping (corresponds to virtual_mappings table)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct VirtualMapping {
    #[serde(default)] // channel_id from directory context
    pub channel_id: u32,
    pub point_id: u32,
    #[serde(default)] // telemetry_type from filename context
    pub telemetry_type: String,
    pub expression: Option<String>,
    #[serde(default = "default_update_interval")]
    pub update_interval: Option<u32>,
    #[serde(default)]
    pub initial_value: Option<f64>,
    #[serde(default)]
    pub noise_range: Option<f64>,
}

fn default_update_interval() -> Option<u32> {
    Some(1000)
}

/// IEC 60870-5-104 protocol mapping (corresponds to iec_mappings table)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct IecMapping {
    #[serde(default)] // channel_id from directory context
    pub channel_id: u32,
    pub point_id: u32,
    #[serde(default)] // telemetry_type from filename context
    pub telemetry_type: String,
    pub asdu_address: i32,
    pub object_address: i32,
    pub type_id: i32,
    #[serde(default = "default_cot")]
    pub cot: i32,
    #[serde(default)]
    pub qualifier: i32,
}

fn default_cot() -> i32 {
    20 // Default Cause of Transmission
}

/// gRPC protocol mapping (corresponds to grpc_mappings table)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct GrpcMapping {
    #[serde(default)] // channel_id from directory context
    pub channel_id: u32,
    pub point_id: u32,
    #[serde(default)] // telemetry_type from filename context
    pub telemetry_type: String,
    pub service_name: String,
    pub method_name: String,
    pub field_path: Option<String>,
}

/// CAN protocol mapping (corresponds to can_mappings table)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct CanMapping {
    #[serde(default)] // channel_id comes from directory context
    pub channel_id: u32,
    pub point_id: u32,
    #[serde(default)] // telemetry_type comes from filename context
    pub telemetry_type: String,
    pub can_id: u32,
    #[serde(default)]
    pub msg_name: Option<String>,
    #[serde(default)]
    pub signal_name: Option<String>,
    pub start_bit: u32,  // Changed to u32 to match database
    pub bit_length: u32, // Changed to u32 to match database
    #[serde(default = "default_byte_order")]
    pub byte_order: String,
    #[serde(default = "default_data_type")]
    pub data_type: String,
    #[serde(default)]
    pub signed: bool,
    #[serde(default = "scale_one")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub min_value: Option<f64>,
    #[serde(default)]
    pub max_value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
}

// Default value functions for serde
fn default_byte_order() -> String {
    "ABCD".to_string()
}

fn default_data_type() -> String {
    "uint32".to_string()
}

use sqlx::{Executor, Sqlite, sqlite::SqliteQueryResult};

/// Trait for inserting point definitions into database
#[allow(async_fn_in_trait)]
pub trait SqlInsertablePoint {
    /// Execute insertion with automatic parameter binding for points
    async fn insert_with<'e, E>(
        &self,
        executor: E,
        channel_id: u32,
    ) -> Result<SqliteQueryResult, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>;
}

/// Complete runtime channel configuration
/// Contains base configuration and points with embedded protocol mappings
#[derive(Debug, Clone)]
pub struct RuntimeChannelConfig {
    /// Base channel configuration (Arc-wrapped for zero-copy sharing)
    pub base: Arc<ChannelConfig>,

    /// Telemetry points (with embedded protocol_mappings JSON)
    pub telemetry_points: Vec<TelemetryPoint>,

    /// Signal points (with embedded protocol_mappings JSON)
    pub signal_points: Vec<SignalPoint>,

    /// Control points (with embedded protocol_mappings JSON)
    pub control_points: Vec<ControlPoint>,

    /// Adjustment points (with embedded protocol_mappings JSON)
    pub adjustment_points: Vec<AdjustmentPoint>,
    // Protocol mappings are now embedded in each point's protocol_mappings field
}

impl RuntimeChannelConfig {
    /// Create from base configuration (wraps in Arc for zero-copy sharing)
    pub fn from_base(base: ChannelConfig) -> Self {
        Self::from_base_arc(Arc::new(base))
    }

    /// Create from Arc-wrapped base configuration (zero-copy)
    pub fn from_base_arc(base: Arc<ChannelConfig>) -> Self {
        Self {
            base,
            telemetry_points: Vec::new(),
            signal_points: Vec::new(),
            control_points: Vec::new(),
            adjustment_points: Vec::new(),
        }
    }

    /// Get channel ID
    pub fn id(&self) -> u32 {
        self.base.core.id
    }

    /// Get channel name
    pub fn name(&self) -> &str {
        &self.base.core.name
    }

    /// Get protocol
    pub fn protocol(&self) -> &str {
        &self.base.core.protocol
    }

    /// Check if enabled
    pub fn is_enabled(&self) -> bool {
        self.base.core.enabled
    }

    // ========================================================================
    // Point Query Methods (Type-Safe)
    // ========================================================================
    //
    // DESIGN PRINCIPLE: point_id is only unique within a point type.
    // The composite key is (channel_id, point_type, point_id).
    //
    // When querying points, you MUST either:
    // 1. Iterate over a specific type collection (e.g., `for pt in &signal_points`)
    // 2. Use typed query methods (e.g., `get_control_point(id)`)
    //
    // NEVER search across all point types with just a point_id - this was the
    // root cause of the GPIO mapping bug where signal and control had the same
    // point_id but different GPIO numbers.
    // ========================================================================

    /// Get a telemetry point by ID
    pub fn get_telemetry_point(&self, point_id: u32) -> Option<&TelemetryPoint> {
        self.telemetry_points
            .iter()
            .find(|p| p.base.point_id == point_id)
    }

    /// Get a signal point by ID
    pub fn get_signal_point(&self, point_id: u32) -> Option<&SignalPoint> {
        self.signal_points
            .iter()
            .find(|p| p.base.point_id == point_id)
    }

    /// Get a control point by ID
    pub fn get_control_point(&self, point_id: u32) -> Option<&ControlPoint> {
        self.control_points
            .iter()
            .find(|p| p.base.point_id == point_id)
    }

    /// Get an adjustment point by ID
    pub fn get_adjustment_point(&self, point_id: u32) -> Option<&AdjustmentPoint> {
        self.adjustment_points
            .iter()
            .find(|p| p.base.point_id == point_id)
    }
}

// Default value functions
fn default_control_type() -> String {
    "momentary".to_string()
}

fn default_on_value() -> u16 {
    1
}

fn default_off_value() -> u16 {
    0
}

// Default implementations
impl Default for IoConfig {
    fn default() -> Self {
        let service = BaseServiceConfig {
            name: "aether-io".to_string(),
            ..Default::default()
        };

        let api = ApiConfig {
            host: common::DEFAULT_API_HOST.to_string(),
            port: 6001, // io default port
        };

        Self {
            service,
            api,
            logging: LoggingConfig::default(),
            channels: Vec::new(),
        }
    }
}

use anyhow::Result;

impl ConfigValidator for IoConfig {
    fn validate_syntax(&self) -> Result<ValidationResult> {
        // Syntax validation is mainly done during deserialization
        // If we get here, the YAML/JSON was parseable
        Ok(ValidationResult::new(ValidationLevel::Syntax))
    }

    fn validate_schema(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Schema);

        // Validate common components
        self.service.validate(&mut result);
        self.api.validate(&mut result);
        self.logging.validate(&mut result);

        // Validate channels
        for (idx, channel) in self.channels.iter().enumerate() {
            channel.validate(&mut result, idx);
        }

        Ok(result)
    }

    fn validate_business(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Business);

        if self.channels.is_empty() {
            result.add_warning("No channels configured".to_string());
        }

        let supported_protocols = ["modbus_tcp", "modbus_rtu", "virtual", "grpc"];
        let mut channel_ids = std::collections::HashSet::new();
        let mut channel_names = std::collections::HashSet::new();
        for channel in &self.channels {
            if !channel_ids.insert(channel.core.id) {
                result.add_error(format!("Duplicate channel ID: {}", channel.core.id));
            }
            if !channel_names.insert(&channel.core.name) {
                result.add_error(format!("Duplicate channel name: {}", channel.core.name));
            }
            if !supported_protocols.contains(&channel.core.protocol.as_str()) {
                result.add_warning(format!(
                    "Channel {} uses unknown protocol: {}",
                    channel.core.name, channel.core.protocol
                ));
            }
        }

        Ok(result)
    }

    fn validate_runtime(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Runtime);

        // Port availability check
        self.api.validate_runtime(&mut result);

        Ok(result)
    }
}

impl ChannelConfig {
    /// Validate channel configuration
    pub fn validate(&self, result: &mut ValidationResult, idx: usize) {
        if self.core.name.is_empty() {
            result.add_error(format!("Channel {} name cannot be empty", idx));
        }

        if self.core.protocol.is_empty() {
            result.add_error(format!(
                "Channel {} protocol cannot be empty",
                self.core.name
            ));
        }

        // Zero reaches `tokio::time::interval` as a panic, so this is a
        // protocol-independent schema invariant rather than an adapter default.
        validate_optional_timing_parameter(self, result, "poll_interval_ms");

        // Protocol-specific parameter validation
        match self.core.protocol.as_str() {
            "modbus_tcp" | "sunspec_tcp" => {
                validate_required_string_parameter(self, result, "host");
                validate_required_integer_parameter(self, result, "port", u64::from(u16::MAX));
                validate_optional_timing_parameter(self, result, "read_timeout_ms");
            },
            "modbus_rtu" | "sunspec_rtu" => {
                validate_required_string_parameter(self, result, "device");
                validate_required_integer_parameter(self, result, "baud_rate", u64::from(u32::MAX));
                validate_optional_timing_parameter(self, result, "read_timeout_ms");
            },
            _ => {
                // Other protocols may have different requirements
            },
        }
    }
}

/// Type alias for backward compatibility - use GenericValidator directly for new code
pub type IoValidator = common::GenericValidator<IoConfig>;

impl Point {
    /// Insert a point row into the points table with the given type-specific values
    async fn insert_point<'e, E>(
        &self,
        executor: E,
        channel_id: u32,
        point_type: &str,
        scale: f64,
        offset: f64,
        reverse: bool,
        data_type: &str,
    ) -> Result<SqliteQueryResult, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query(
            "INSERT INTO points (channel_id, point_id, telemetry_type, signal_name,
                               scale, offset, unit, reverse, data_type, description)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(channel_id)
        .bind(self.point_id)
        .bind(point_type)
        .bind(&self.signal_name)
        .bind(scale)
        .bind(offset)
        .bind(&self.unit)
        .bind(reverse)
        .bind(data_type)
        .bind(&self.description)
        .execute(executor)
        .await
    }
}

impl SqlInsertablePoint for TelemetryPoint {
    async fn insert_with<'e, E>(
        &self,
        executor: E,
        channel_id: u32,
    ) -> Result<SqliteQueryResult, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        self.base
            .insert_point(
                executor,
                channel_id,
                "T",
                self.scale,
                self.offset,
                self.reverse,
                &self.data_type,
            )
            .await
    }
}

impl SqlInsertablePoint for SignalPoint {
    async fn insert_with<'e, E>(
        &self,
        executor: E,
        channel_id: u32,
    ) -> Result<SqliteQueryResult, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        self.base
            .insert_point(executor, channel_id, "S", 1.0, 0.0, self.reverse, "uint16")
            .await
    }
}

impl SqlInsertablePoint for ControlPoint {
    async fn insert_with<'e, E>(
        &self,
        executor: E,
        channel_id: u32,
    ) -> Result<SqliteQueryResult, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        self.base
            .insert_point(executor, channel_id, "C", 1.0, 0.0, self.reverse, "uint16")
            .await
    }
}

impl SqlInsertablePoint for AdjustmentPoint {
    async fn insert_with<'e, E>(
        &self,
        executor: E,
        channel_id: u32,
    ) -> Result<SqliteQueryResult, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        self.base
            .insert_point(
                executor,
                channel_id,
                "A",
                self.scale,
                self.offset,
                false,
                &self.data_type,
            )
            .await
    }
}

/// Common CSV field names shared by all point types
fn point_csv_fields() -> Vec<String> {
    [
        "point_id",
        "signal_name",
        "scale",
        "offset",
        "unit",
        "reverse",
        "data_type",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// All four point types share the same CSV column layout
macro_rules! impl_csv_fields {
    ($($t:ty),+) => { $(impl CsvFields for $t { fn field_names() -> Vec<String> { point_csv_fields() } })+ };
}
impl_csv_fields!(TelemetryPoint, SignalPoint, ControlPoint, AdjustmentPoint);

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    fn validate_channel(protocol: &str, parameters: serde_json::Value) -> ValidationResult {
        let parameters = parameters
            .as_object()
            .expect("test parameters object")
            .clone()
            .into_iter()
            .collect();
        let channel = ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "validation-channel".to_owned(),
                description: None,
                protocol: protocol.to_owned(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        };
        let mut result = ValidationResult::new(ValidationLevel::Schema);
        channel.validate(&mut result, 0);
        result
    }

    #[test]
    fn generated_channel_schema_carries_the_governed_revision_invariant() {
        assert!(
            CHANNELS_TABLE.contains("enabled BOOLEAN NOT NULL DEFAULT FALSE"),
            "generated channel schema must be inert by default: {CHANNELS_TABLE}"
        );
        assert!(
            CHANNELS_TABLE.contains("revision INTEGER NOT NULL DEFAULT 1"),
            "generated channel schema must initialize the CAS revision: {CHANNELS_TABLE}"
        );
        assert!(
            CHANNELS_TABLE.contains("CHECK (TYPEOF(revision) = 'integer' AND revision >= 1)"),
            "generated channel schema must reject invalid revisions: {CHANNELS_TABLE}"
        );
    }

    #[test]
    fn omitted_channel_enabled_state_is_fail_safe() {
        let config: IoConfig = serde_yml::from_str(
            r#"
channels:
  - id: 1001
    name: inert-channel
    protocol: virtual
    parameters: {}
"#,
        )
        .expect("channel config without enabled");

        assert!(!config.channels[0].core.enabled);
    }

    #[test]
    fn generated_point_tables_cascade_when_channel_is_deleted() {
        for (table, ddl) in [
            ("telemetry_points", TELEMETRY_POINTS_TABLE),
            ("signal_points", SIGNAL_POINTS_TABLE),
            ("control_points", CONTROL_POINTS_TABLE),
            ("adjustment_points", ADJUSTMENT_POINTS_TABLE),
        ] {
            assert!(
                ddl.contains("REFERENCES channels(channel_id) ON DELETE CASCADE"),
                "generated schema for {table} must cascade its channel foreign key: {ddl}"
            );
        }
    }

    #[test]
    fn modbus_endpoint_schema_rejects_wrong_types_and_numeric_overflow() {
        for (protocol, parameters) in [
            ("modbus_tcp", serde_json::json!({"host": 123, "port": 502})),
            (
                "modbus_tcp",
                serde_json::json!({"host": "edge", "port": -1}),
            ),
            (
                "sunspec_tcp",
                serde_json::json!({"host": "edge", "port": 65_536}),
            ),
            (
                "modbus_rtu",
                serde_json::json!({"device": false, "baud_rate": 9_600}),
            ),
            (
                "modbus_rtu",
                serde_json::json!({"device": "/dev/ttyUSB0", "baud_rate": -1}),
            ),
            (
                "sunspec_rtu",
                serde_json::json!({"device": "/dev/ttyUSB0", "baud_rate": 4_294_967_296_u64}),
            ),
        ] {
            assert!(
                !validate_channel(protocol, parameters).is_valid,
                "{protocol} must reject an endpoint that could fallback or truncate"
            );
        }

        for (protocol, parameters) in [
            ("modbus_tcp", serde_json::json!({"host": "edge", "port": 1})),
            (
                "sunspec_tcp",
                serde_json::json!({"host": "edge", "port": 65_535}),
            ),
            (
                "modbus_rtu",
                serde_json::json!({"device": "/dev/ttyUSB0", "baud_rate": 1}),
            ),
            (
                "sunspec_rtu",
                serde_json::json!({"device": "/dev/ttyUSB0", "baud_rate": 4_294_967_295_u64}),
            ),
        ] {
            assert!(
                validate_channel(protocol, parameters).is_valid,
                "{protocol} boundary endpoint must remain valid"
            );
        }
    }

    #[test]
    fn every_protocol_rejects_an_invalid_poll_interval() {
        for value in [
            serde_json::json!("1000"),
            serde_json::json!(0),
            serde_json::json!(86_400_001),
        ] {
            let result =
                validate_channel("virtual", serde_json::json!({"poll_interval_ms": value}));
            assert!(
                !result.is_valid,
                "invalid poll interval must fail schema validation"
            );
        }
        assert!(
            validate_channel(
                "virtual",
                serde_json::json!({"poll_interval_ms": 86_400_000})
            )
            .is_valid
        );
    }

    #[test]
    fn test_minimal_config_with_only_channels() {
        let yaml = r#"
channels:
  - id: 1001
    name: "Test Channel"
    protocol: "modbus_tcp"
    enabled: true
    parameters:
      host: "192.168.1.100"
      port: 502
    logging:
      enabled: false
"#;

        let config: IoConfig =
            serde_yml::from_str(yaml).expect("Should load minimal config with only channels");

        // Verify default values are used
        assert_eq!(config.service.name, "unnamed_service");
        assert_eq!(config.api.host, "127.0.0.1");
        assert_eq!(config.api.port, 6001);
        let serialized = serde_json::to_value(&config).expect("IoConfig should serialize");
        assert!(
            serialized.get("redis").is_none(),
            "SHM-only io config must not expose Redis"
        );
        assert_eq!(config.channels.len(), 1);
        assert_eq!(config.channels[0].core.name, "Test Channel");
    }

    #[test]
    fn test_empty_config_uses_all_defaults() {
        let yaml = "{}";

        let config: IoConfig =
            serde_yml::from_str(yaml).expect("Should load empty config with all defaults");

        // Verify all default values
        assert_eq!(config.service.name, "unnamed_service");
        assert_eq!(config.api.host, "127.0.0.1");
        assert_eq!(config.api.port, 6001);
        let serialized = serde_json::to_value(&config).expect("IoConfig should serialize");
        assert!(
            serialized.get("redis").is_none(),
            "default io config must remain external-database-free"
        );
        assert_eq!(config.channels.len(), 0);
    }
}
