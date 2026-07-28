//! Common configuration structures shared across all services
//!
//! This module provides shared types for service configuration including:
//! - Base service and API configuration structs
//! - Validation framework (ConfigValidator, ValidationResult)
//! - Storage/configuration enums used by offline tooling and local services

use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::str::FromStr;

// Re-export the protocol/storage representation for configuration compatibility.
pub use crate::point_type::PointType;

use anyhow::Result;

// ============================================================================
// Default configuration constants
// ============================================================================

/// Default API bind host (loopback only)
/// Internal service APIs are host-local by default. The authenticated API
/// gateway opts into a public bind independently.
pub const DEFAULT_API_HOST: &str = "127.0.0.1";

// ============================================================================
// Service URL constants
// ============================================================================

const DEFAULT_IO_URL: &str = "http://localhost:6001";
const DEFAULT_AUTOMATION_URL: &str = "http://localhost:6002";
const ENV_IO_URL: &str = "AETHER_IO_URL";
const ENV_AUTOMATION_URL: &str = "AETHER_AUTOMATION_URL";

/// Resolve the aether-io base URL, preferring `AETHER_IO_URL`.
pub fn io_url() -> String {
    env::var(ENV_IO_URL).unwrap_or_else(|_| DEFAULT_IO_URL.to_string())
}

/// Resolve the aether-automation base URL, preferring `AETHER_AUTOMATION_URL`.
pub fn automation_url() -> String {
    env::var(ENV_AUTOMATION_URL).unwrap_or_else(|_| DEFAULT_AUTOMATION_URL.to_string())
}

// ============================================================================
// Base service configuration
// ============================================================================

/// Base service configuration shared by all services
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct ApiConfig {
    /// Listen host address
    #[serde(default = "default_api_host")]
    pub host: String,

    /// Listen port (no default - set by service-specific config)
    pub port: u16,
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

// Note: bool_true() is defined in serde_helpers module

// ============================================================================
// Default implementations
// ============================================================================

pub use crate::site_schema::{SERVICE_CONFIG_TABLE, SYNC_METADATA_TABLE};

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
    fn validate_syntax(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Syntax);
        result.add_warning("Syntax validation not implemented for this config type".to_string());
        Ok(result)
    }

    /// Validate schema (required fields, types)
    fn validate_schema(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Schema);
        result.add_warning("Schema validation not implemented for this config type".to_string());
        Ok(result)
    }

    /// Validate business rules
    fn validate_business(&self) -> Result<ValidationResult>;

    /// Validate runtime environment
    fn validate_runtime(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Runtime);
        result.add_warning(
            "Runtime validation not applicable for configuration management".to_string(),
        );
        Ok(result)
    }

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

mod helpers {
    use anyhow::Result;
    use std::net::TcpListener;

    pub(super) fn check_port_available(port: u16) -> Result<()> {
        TcpListener::bind(("127.0.0.1", port))
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("Port {port} is not available: {error}"))
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
        if let Err(e) = helpers::check_port_available(self.port) {
            result.add_error(format!("Port {} not available: {}", self.port, e));
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
}
