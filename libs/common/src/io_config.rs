//! Io service configuration structures

use crate::serde_helpers::deserialize_bool_flexible;
use crate::{ApiConfig, BaseServiceConfig, ConfigValidator, ValidationLevel, ValidationResult};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "openapi")]
use utoipa::ToSchema;

/// Default API configuration for io (port 6001)
fn default_io_api() -> ApiConfig {
    ApiConfig {
        host: crate::DEFAULT_API_HOST.to_string(),
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

    /// Channel configurations (wrapped in Arc for cheap cloning during startup)
    #[serde(default)]
    pub channels: Vec<Arc<ChannelConfig>>,
}

/// Service configuration table SQL (from common)
pub use crate::SERVICE_CONFIG_TABLE;

/// Sync metadata table SQL (from common)
pub use crate::SYNC_METADATA_TABLE;

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

    /// Protocol type (for example modbus_tcp, modbus_rtu, or grpc)
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

pub use crate::site_schema::CHANNELS_TABLE;

/// Compatibility triggers required after creating [`CHANNELS_TABLE`].
///
/// The generated table DDL cannot carry sibling trigger statements, so schema
/// setup paths must install these immediately after creating the table.
pub use crate::site_schema::{
    CHANNEL_REVISION_BUMP_TRIGGER, CHANNEL_REVISION_EXHAUSTED_TRIGGER,
    install_channel_revision_triggers,
};

/// Channel-specific logging configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ChannelLoggingConfig {
    /// Whether logging is enabled for this channel
    #[serde(default)]
    pub enabled: bool,

    /// Log level for this channel
    pub level: Option<String>,
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

use crate::serde_helpers::{deserialize_offset, deserialize_scale, scale_one, step_one};

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

pub use crate::site_schema::{
    ADJUSTMENT_POINTS_TABLE, CONTROL_POINTS_TABLE, SIGNAL_POINTS_TABLE, TELEMETRY_POINTS_TABLE,
};

// Default value functions for serde
fn default_data_type() -> String {
    "uint32".to_string()
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
            host: crate::DEFAULT_API_HOST.to_string(),
            port: 6001, // io default port
        };

        Self {
            service,
            api,
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

        let supported_protocols = [
            "modbus_tcp",
            "modbus_rtu",
            "mqtt",
            "http",
            "can",
            "gpio",
            "iec61850",
            "aether_485",
        ];
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
            "modbus_tcp" => {
                validate_required_string_parameter(self, result, "host");
                validate_required_integer_parameter(self, result, "port", u64::from(u16::MAX));
                validate_optional_timing_parameter(self, result, "read_timeout_ms");
            },
            "modbus_rtu" => {
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
    fn canonical_channel_schema_carries_the_governed_revision_invariant() {
        assert!(
            CHANNELS_TABLE.contains("enabled INTEGER NOT NULL DEFAULT 0"),
            "channel schema must be inert by default: {CHANNELS_TABLE}"
        );
        assert!(
            CHANNELS_TABLE.contains("revision INTEGER NOT NULL DEFAULT 1"),
            "channel schema must initialize the CAS revision: {CHANNELS_TABLE}"
        );
        assert!(
            CHANNELS_TABLE.contains("CHECK (TYPEOF(revision) = 'integer' AND revision >= 1)"),
            "channel schema must reject invalid revisions: {CHANNELS_TABLE}"
        );
    }

    #[test]
    fn omitted_channel_enabled_state_is_fail_safe() {
        let config: IoConfig = serde_yml::from_str(
            r#"
channels:
  - id: 1001
    name: inert-channel
    protocol: modbus_tcp
    parameters:
      host: 127.0.0.1
      port: 502
"#,
        )
        .expect("channel config without enabled");

        assert!(!config.channels[0].core.enabled);
    }

    #[test]
    fn retired_logging_file_is_rejected() {
        let error = serde_json::from_value::<ChannelLoggingConfig>(serde_json::json!({
            "enabled": true,
            "level": "debug",
            "file": "/tmp/retired.log"
        }))
        .expect_err("caller-selected diagnostic paths must be rejected");

        assert!(error.to_string().contains("unknown field `file`"));
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
                "modbus_rtu",
                serde_json::json!({"device": false, "baud_rate": 9_600}),
            ),
            (
                "modbus_rtu",
                serde_json::json!({"device": "/dev/ttyUSB0", "baud_rate": -1}),
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
                "modbus_rtu",
                serde_json::json!({"device": "/dev/ttyUSB0", "baud_rate": 1}),
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
            let result = validate_channel(
                "modbus_tcp",
                serde_json::json!({
                    "host": "127.0.0.1",
                    "port": 502,
                    "poll_interval_ms": value
                }),
            );
            assert!(
                !result.is_valid,
                "invalid poll interval must fail schema validation"
            );
        }
        assert!(
            validate_channel(
                "modbus_tcp",
                serde_json::json!({
                    "host": "127.0.0.1",
                    "port": 502,
                    "poll_interval_ms": 86_400_000
                })
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
