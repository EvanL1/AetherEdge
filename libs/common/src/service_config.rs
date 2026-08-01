//! Common configuration structures shared across all services
//!
//! This module provides shared types for service configuration including:
//! - Base configuration structs (ApiConfig, LoggingConfig)
//! - Validation framework (ConfigValidator, ValidationResult)
//! - Shared enums (PointRole, InstanceStatus, ResponseStatus, ComparisonOperator)

use aether_schema_macro::Schema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

// Re-export the firmware/protocol representation type for wire compatibility.
pub use aether_core::PointType;

#[cfg(feature = "schema")]
use schemars::JsonSchema;

// Required for GenericValidator
use anyhow::{Context, Result};

// ============================================================================
// Default configuration constants
// ============================================================================

/// Default API bind host (listen on all interfaces)
/// Internal service APIs are host-local by default. The authenticated API
/// gateway opts into a public bind independently.
pub const DEFAULT_API_HOST: &str = "127.0.0.1";

/// Localhost address for testing
pub const LOCALHOST_HOST: &str = "127.0.0.1";

// ============================================================================
// Service URL constants
// ============================================================================

/// Default io service URL (localhost)
pub const DEFAULT_IO_URL: &str = "http://localhost:6001";

/// Default automation service URL (localhost)
pub const DEFAULT_AUTOMATION_URL: &str = "http://localhost:6002";

/// Default rules service URL (localhost, merged into automation)
pub const DEFAULT_RULES_URL: &str = "http://localhost:6002";

/// Environment variable name for io URL
pub const ENV_IO_URL: &str = "AETHER_IO_URL";

/// Environment variable name for automation URL
pub const ENV_AUTOMATION_URL: &str = "AETHER_AUTOMATION_URL";

/// Environment variable name for rules URL
pub const ENV_RULES_URL: &str = "RULES_URL";

/// Resolve the aether-io base URL, preferring `AETHER_IO_URL`.
pub fn io_url() -> String {
    env::var(ENV_IO_URL).unwrap_or_else(|_| DEFAULT_IO_URL.to_string())
}

/// Resolve the aether-automation base URL, preferring `AETHER_AUTOMATION_URL`.
pub fn automation_url() -> String {
    env::var(ENV_AUTOMATION_URL).unwrap_or_else(|_| DEFAULT_AUTOMATION_URL.to_string())
}

/// Build the socket address a service listens on.
///
/// A bare IPv6 literal is bracketed first. `format!("{host}:{port}")` cannot
/// express an IPv6 address — `::1:6007` is not valid syntax — so a service
/// configured with `API_HOST=::1` would fail to parse before it ever reached
/// the listener.
pub fn bind_address(host: &str, port: u16) -> anyhow::Result<std::net::SocketAddr> {
    let literal = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    literal
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid bind address {literal}: {error}"))
}

/// Read `name` from the environment, falling back to `default` when the
/// variable is unset or does not parse.
pub fn env_or<T: FromStr>(name: &str, default: T) -> T {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

// ============================================================================
// Base service configuration
// ============================================================================

/// Base service configuration shared by all services
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct BaseServiceConfig {
    /// Service name
    #[serde(default = "default_service_name")]
    pub name: String,

    /// Service version
    pub version: Option<String>,

    /// Service description
    pub description: Option<String>,
}

impl Default for BaseServiceConfig {
    fn default() -> Self {
        Self {
            name: default_service_name(),
            version: None,
            description: None,
        }
    }
}

// ============================================================================
// API configuration
// ============================================================================

/// API server configuration
///
/// Note: port field has no default value - each service must set its own default port
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct ApiConfig {
    /// Listen host address
    #[serde(default = "default_api_host")]
    pub host: String,

    /// Listen port (no default - set by service-specific config)
    pub port: u16,
}

// ============================================================================
// Logging configuration
// ============================================================================

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Log directory
    #[serde(default = "default_log_dir")]
    pub dir: String,

    /// Log file prefix
    pub file_prefix: Option<String>,

    /// Log rotation configuration
    #[serde(default)]
    pub rotation: Option<LogRotationConfig>,
}

/// Log rotation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct LogRotationConfig {
    /// Rotation strategy (daily, size, never)
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,

    /// Maximum file size in MB (for size-based rotation)
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,

    /// Number of log files to retain
    #[serde(default = "default_max_files")]
    pub max_files: u32,
}

// ============================================================================
// Default value functions
// ============================================================================

fn default_service_name() -> String {
    "unnamed_service".to_string()
}

fn default_api_host() -> String {
    DEFAULT_API_HOST.to_string()
}

fn default_log_level() -> String {
    env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string())
}

fn default_log_dir() -> String {
    "logs".to_string()
}

fn default_rotation_strategy() -> String {
    "daily".to_string()
}

fn default_max_size_mb() -> u64 {
    100
}

fn default_max_files() -> u32 {
    7
}

// Note: bool_true() is defined in serde_helpers module

// ============================================================================
// Default implementations
// ============================================================================

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            dir: default_log_dir(),
            file_prefix: None,
            rotation: None,
        }
    }
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            strategy: default_rotation_strategy(),
            max_size_mb: default_max_size_mb(),
            max_files: default_max_files(),
        }
    }
}

// ============================================================================
// Database Schema Definitions (Shared across services)
// ============================================================================

/// Service configuration table record
/// Supports both global and service-specific configuration with composite primary key
#[allow(dead_code)]
#[derive(Schema)]
#[table(name = "service_config")]
pub struct ServiceConfigRecord {
    #[column(not_null, primary_key)]
    pub service_name: String,

    #[column(not_null, primary_key)]
    pub key: String,

    #[column(not_null)]
    pub value: String,

    #[column(default = "string")]
    pub r#type: String,

    pub description: Option<String>,

    #[column(default = "CURRENT_TIMESTAMP")]
    pub updated_at: String, // TIMESTAMP type
}

/// Sync metadata table record
/// Tracks configuration synchronization status
#[allow(dead_code)]
#[derive(Schema)]
#[table(name = "sync_metadata")]
pub struct SyncMetadataRecord {
    #[column(primary_key)]
    pub service: String,

    #[column(not_null)]
    pub last_sync: String, // TIMESTAMP type

    pub version: Option<String>,
}

/// Service configuration table SQL (generated by Schema macro)
pub const SERVICE_CONFIG_TABLE: &str = ServiceConfigRecord::CREATE_TABLE_SQL;

/// Sync metadata table SQL (generated by Schema macro)
pub const SYNC_METADATA_TABLE: &str = SyncMetadataRecord::CREATE_TABLE_SQL;

// ============================================================================
// Core Validation Framework
// ============================================================================

/// Validation result with detailed information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub is_valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub level: ValidationLevel,
}

impl ValidationResult {
    pub fn new(level: ValidationLevel) -> Self {
        Self {
            is_valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
            level,
        }
    }

    pub fn add_error(&mut self, error: String) {
        self.errors.push(error);
        self.is_valid = false;
    }

    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    pub fn merge(&mut self, other: ValidationResult) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
        if !other.is_valid {
            self.is_valid = false;
        }
    }
}

/// Validation levels for different stages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationLevel {
    /// YAML/CSV syntax validation (Aether only)
    Syntax,
    /// Schema and required fields validation (Aether only)
    Schema,
    /// Business rules validation (Aether and services)
    Business,
    /// Runtime environment validation (Services only)
    Runtime,
}

/// Core trait for configuration validation
pub trait ConfigValidator: Send + Sync {
    /// Validate syntax (YAML/CSV format)
    fn validate_syntax(&self) -> Result<ValidationResult>;

    /// Validate schema (required fields, types)
    fn validate_schema(&self) -> Result<ValidationResult>;

    /// Validate business rules
    fn validate_business(&self) -> Result<ValidationResult>;

    /// Validate runtime environment
    fn validate_runtime(&self) -> Result<ValidationResult>;

    /// Perform full validation up to specified level
    fn validate(&self, up_to_level: ValidationLevel) -> Result<ValidationResult> {
        let mut combined = ValidationResult::new(up_to_level);

        if up_to_level as u8 >= ValidationLevel::Syntax as u8 {
            combined.merge(self.validate_syntax()?);
        }

        if up_to_level as u8 >= ValidationLevel::Schema as u8 {
            combined.merge(self.validate_schema()?);
        }

        if up_to_level as u8 >= ValidationLevel::Business as u8 {
            combined.merge(self.validate_business()?);
        }

        if up_to_level as u8 >= ValidationLevel::Runtime as u8 {
            combined.merge(self.validate_runtime()?);
        }

        Ok(combined)
    }
}

// ============================================================================
// Generic Validator
// ============================================================================

/// Generic configuration validator that works with any config type
///
/// This eliminates the need for separate validator implementations for each service.
/// Instead of defining IoValidator, AutomationValidator, and RulesValidator separately,
/// use type aliases:
///
/// ```ignore
/// pub type IoValidator = GenericValidator<IoConfig>;
/// pub type AutomationValidator = GenericValidator<AutomationConfig>;
/// pub type RulesValidator = GenericValidator<RulesConfig>;
/// ```
pub struct GenericValidator<T> {
    config: Option<T>,
    raw_yaml: Option<serde_yml::Value>,
}

impl<T: DeserializeOwned + ConfigValidator> GenericValidator<T> {
    /// Create validator from YAML value
    pub fn from_yaml(yaml: serde_yml::Value) -> Self {
        let config = serde_yml::from_value(yaml.clone()).ok();
        Self {
            config,
            raw_yaml: Some(yaml),
        }
    }

    /// Create validator from already-parsed config
    pub fn from_config(config: T) -> Self {
        Self {
            config: Some(config),
            raw_yaml: None,
        }
    }

    /// Create validator from file path
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        // Deserialize directly from string to capture line/column information
        let config = serde_yml::from_str::<T>(&content).map_err(|e| {
            if let Some(location) = e.location() {
                anyhow::anyhow!(
                    "Configuration error in {}:{}:{}\n  {}",
                    path.display(),
                    location.line(),
                    location.column(),
                    e
                )
            } else {
                anyhow::anyhow!("Configuration error in {}\n  {}", path.display(), e)
            }
        })?;

        // Also parse as YAML Value for raw_yaml field
        let yaml: serde_yml::Value = serde_yml::from_str(&content)?;

        Ok(Self {
            config: Some(config),
            raw_yaml: Some(yaml),
        })
    }

    /// Get reference to the parsed config
    pub fn config(&self) -> Option<&T> {
        self.config.as_ref()
    }

    /// Take ownership of the parsed config
    pub fn into_config(self) -> Option<T> {
        self.config
    }
}

impl<T: DeserializeOwned + ConfigValidator> GenericValidator<T> {
    /// Delegate validation to inner config, or return error if config is unavailable
    fn delegate_or_error(
        &self,
        level: ValidationLevel,
        f: impl FnOnce(&T) -> Result<ValidationResult>,
    ) -> Result<ValidationResult> {
        match &self.config {
            Some(config) => f(config),
            None => {
                let mut result = ValidationResult::new(level);
                result.add_error("Configuration not available".to_string());
                Ok(result)
            },
        }
    }
}

impl<T: DeserializeOwned + ConfigValidator> ConfigValidator for GenericValidator<T> {
    fn validate_syntax(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Syntax);

        if self.config.is_none() {
            if let Some(yaml) = &self.raw_yaml {
                match serde_yml::from_value::<T>(yaml.clone()) {
                    Ok(_) => {
                        result.add_warning("Configuration parsed but not stored".to_string());
                    },
                    Err(e) => {
                        result.add_error(format!("Invalid YAML syntax: {}", e));
                    },
                }
            } else {
                result.add_error("No configuration data available".to_string());
            }
        }

        Ok(result)
    }

    fn validate_schema(&self) -> Result<ValidationResult> {
        self.delegate_or_error(ValidationLevel::Schema, |c| c.validate_schema())
    }

    fn validate_business(&self) -> Result<ValidationResult> {
        self.delegate_or_error(ValidationLevel::Business, |c| c.validate_business())
    }

    fn validate_runtime(&self) -> Result<ValidationResult> {
        self.delegate_or_error(ValidationLevel::Runtime, |c| c.validate_runtime())
    }
}

// ============================================================================
// Validation implementations for common configs
// ============================================================================

impl BaseServiceConfig {
    /// Validate base service configuration
    pub fn validate(&self, result: &mut ValidationResult) {
        if self.name.is_empty() {
            result.add_error("Service name cannot be empty".to_string());
        }
    }
}

impl ApiConfig {
    /// Validate API configuration
    pub fn validate(&self, result: &mut ValidationResult) {
        // Port validation
        if self.port == 0 {
            result.add_error("API port cannot be 0".to_string());
        } else if self.port < 1024 {
            result.add_warning(format!(
                "API port {} is in system range (< 1024)",
                self.port
            ));
        }

        // Host validation
        if self.host.is_empty() {
            result.add_error("API host cannot be empty".to_string());
        }
    }

    /// Validate port availability (runtime check)
    pub fn validate_runtime(&self, result: &mut ValidationResult) {
        if let Err(e) = check_port_available(self.port) {
            result.add_error(format!("Port {} not available: {}", self.port, e));
        }
    }
}

/// Reports whether the port can still be bound on loopback.
fn check_port_available(port: u16) -> Result<()> {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("Port {} is not available: {}", port, e)),
    }
}

impl LoggingConfig {
    /// Validate logging configuration
    pub fn validate(&self, result: &mut ValidationResult) {
        // Validate log level
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.level.as_str()) {
            result.add_warning(format!("Unrecognized log level: {}", self.level));
        }

        // Validate log directory (will be created if doesn't exist, so just warn)
        if self.dir.is_empty() {
            result.add_error("Log directory cannot be empty".to_string());
        }

        // Validate rotation config if present
        if let Some(rotation) = &self.rotation {
            rotation.validate(result);
        }
    }
}

impl LogRotationConfig {
    /// Validate log rotation configuration
    pub fn validate(&self, result: &mut ValidationResult) {
        let valid_strategies = ["daily", "size", "never"];
        if !valid_strategies.contains(&self.strategy.as_str()) {
            result.add_error(format!("Invalid rotation strategy: {}", self.strategy));
        }

        if self.strategy == "size" && self.max_size_mb == 0 {
            result.add_error("Max size for size-based rotation cannot be 0".to_string());
        }

        if self.max_files == 0 {
            result.add_warning(
                "Max files is 0, log rotation will delete old logs immediately".to_string(),
            );
        }
    }
}

// ============================================================================
// Shared enum types
// ============================================================================

/// Logical model-point direction used by CSV, SQLite, and HTTP DTOs.
///
/// This is a serialization-boundary type. Device-side T/S/C/A representation
/// remains [`PointType`], while business command/acquisition invariants live in
/// `aether-domain`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[repr(u8)]
pub enum PointRole {
    /// Measurement point: data flows from device to instance model.
    #[serde(rename = "M")]
    #[default]
    Measurement = 0,
    /// Action point: data flows from instance model to device.
    #[serde(rename = "A")]
    Action = 1,
}

impl PointRole {
    /// Returns the stable SQLite/CSV representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measurement => "M",
            Self::Action => "A",
        }
    }
}

impl FromStr for PointRole {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_uppercase().as_str() {
            "M" | "MEASUREMENT" => Ok(Self::Measurement),
            "A" | "ACTION" => Ok(Self::Action),
            _ => Err(format!("Unknown point role: {value}")),
        }
    }
}

impl fmt::Display for PointRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Instance status enumeration
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum InstanceStatus {
    /// Instance is running normally
    Running,
    /// Instance is stopped
    #[default]
    Stopped,
    /// Instance has encountered an error
    Error,
    /// Instance is in warning state
    Warning,
    /// Instance is disconnected
    Disconnected,
}

impl InstanceStatus {
    /// Convert to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Disconnected => "disconnected",
        }
    }

    /// Check if instance is healthy (running or warning)
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Running | Self::Warning)
    }
}

impl FromStr for InstanceStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "running" | "run" | "active" => Ok(Self::Running),
            "stopped" | "stop" | "inactive" => Ok(Self::Stopped),
            "error" | "err" | "failed" => Ok(Self::Error),
            "warning" | "warn" => Ok(Self::Warning),
            "disconnected" | "offline" => Ok(Self::Disconnected),
            _ => Err(format!("Unknown instance status: {}", s)),
        }
    }
}

impl fmt::Display for InstanceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Comparison operator for rules engine
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum ComparisonOperator {
    /// Equal to (==)
    #[serde(rename = "eq")]
    #[default]
    Equal,
    /// Not equal to (!=)
    #[serde(rename = "ne")]
    NotEqual,
    /// Greater than (>)
    #[serde(rename = "gt")]
    GreaterThan,
    /// Greater than or equal to (>=)
    #[serde(rename = "gte")]
    GreaterThanOrEqual,
    /// Less than (<)
    #[serde(rename = "lt")]
    LessThan,
    /// Less than or equal to (<=)
    #[serde(rename = "lte")]
    LessThanOrEqual,
    /// Value is within range (inclusive)
    #[serde(rename = "in")]
    InRange,
    /// Value is outside range (exclusive)
    #[serde(rename = "not_in")]
    NotInRange,
    /// String contains substring
    #[serde(rename = "contains")]
    Contains,
    /// String matches regex pattern
    #[serde(rename = "matches")]
    Matches,
}

impl ComparisonOperator {
    /// Convert to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Equal => "eq",
            Self::NotEqual => "ne",
            Self::GreaterThan => "gt",
            Self::GreaterThanOrEqual => "gte",
            Self::LessThan => "lt",
            Self::LessThanOrEqual => "lte",
            Self::InRange => "in",
            Self::NotInRange => "not_in",
            Self::Contains => "contains",
            Self::Matches => "matches",
        }
    }

    /// Get symbol representation
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::GreaterThan => ">",
            Self::GreaterThanOrEqual => ">=",
            Self::LessThan => "<",
            Self::LessThanOrEqual => "<=",
            Self::InRange => "∈",
            Self::NotInRange => "∉",
            Self::Contains => "⊃",
            Self::Matches => "~",
        }
    }

    /// Compare two f64 values
    pub fn compare_f64(&self, left: f64, right: f64) -> bool {
        match self {
            Self::Equal => (left - right).abs() < f64::EPSILON,
            Self::NotEqual => (left - right).abs() >= f64::EPSILON,
            Self::GreaterThan => left > right,
            Self::GreaterThanOrEqual => left >= right,
            Self::LessThan => left < right,
            Self::LessThanOrEqual => left <= right,
            _ => false, // InRange and NotInRange need special handling
        }
    }

    /// Compare two i64 values
    pub fn compare_i64(&self, left: i64, right: i64) -> bool {
        match self {
            Self::Equal => left == right,
            Self::NotEqual => left != right,
            Self::GreaterThan => left > right,
            Self::GreaterThanOrEqual => left >= right,
            Self::LessThan => left < right,
            Self::LessThanOrEqual => left <= right,
            _ => false, // InRange and NotInRange need special handling
        }
    }
}

impl FromStr for ComparisonOperator {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "eq" | "==" | "=" | "equal" => Ok(Self::Equal),
            "ne" | "!=" | "<>" | "not_equal" => Ok(Self::NotEqual),
            "gt" | ">" | "greater" => Ok(Self::GreaterThan),
            "gte" | ">=" | "greater_equal" => Ok(Self::GreaterThanOrEqual),
            "lt" | "<" | "less" => Ok(Self::LessThan),
            "lte" | "<=" | "less_equal" => Ok(Self::LessThanOrEqual),
            "in" | "within" | "between" => Ok(Self::InRange),
            "not_in" | "outside" | "not_between" => Ok(Self::NotInRange),
            "contains" | "has" | "includes" => Ok(Self::Contains),
            "matches" | "~" | "regex" => Ok(Self::Matches),
            _ => Err(format!("Unknown comparison operator: {}", s)),
        }
    }
}

impl fmt::Display for ComparisonOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.symbol())
    }
}

/// FourRemote is an alias for PointType for backward compatibility
///
/// Both represent the same concept: the four remote point types (T/S/C/A)
/// in industrial SCADA systems.
///
/// **Prefer using `PointType` for new code.**
pub type FourRemote = PointType;

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_point_role_serialization() {
        let role = PointRole::Measurement;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"M\"");

        let role = PointRole::Action;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"A\"");

        let role: PointRole = serde_json::from_str("\"M\"").unwrap();
        assert_eq!(role, PointRole::Measurement);
    }

    #[test]
    fn test_point_role_from_str() {
        assert_eq!(PointRole::from_str("M").unwrap(), PointRole::Measurement);
        assert_eq!(PointRole::from_str("A").unwrap(), PointRole::Action);
        assert_eq!(
            PointRole::from_str("measurement").unwrap(),
            PointRole::Measurement
        );
        assert!(PointRole::from_str("X").is_err());
    }

    #[test]
    fn test_instance_status_methods() {
        assert!(InstanceStatus::Running.is_healthy());
        assert!(InstanceStatus::Warning.is_healthy());
        assert!(!InstanceStatus::Stopped.is_healthy());
        assert!(!InstanceStatus::Error.is_healthy());
    }

    #[test]
    fn test_comparison_operator_compare_methods() {
        let op = ComparisonOperator::GreaterThan;
        assert!(op.compare_f64(5.0, 3.0));
        assert!(!op.compare_f64(3.0, 5.0));

        let op = ComparisonOperator::Equal;
        assert!(op.compare_i64(42, 42));
        assert!(!op.compare_i64(42, 43));
    }

    #[test]
    fn test_four_remote_is_point_type() {
        let fr: FourRemote = FourRemote::Telemetry;
        let pt: PointType = fr;
        assert_eq!(pt, PointType::Telemetry);
    }
}
