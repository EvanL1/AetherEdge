//! Configuration validation module
//!
//! This module provides validation functionality for service configurations
//! using the shared validation framework.

use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// Import validation types from common
use common::{ConfigValidator as _, GenericValidator, ValidationLevel, ValidationResult};

// Cross-platform config schema (shared with io/automation via aether-config).
use aether_config::automation::{AutomationConfig, RulesConfig};
use aether_config::io::IoConfig;

// Type aliases for validators
type IoValidator = GenericValidator<IoConfig>;
type AutomationValidator = GenericValidator<AutomationConfig>;
type RulesValidator = GenericValidator<RulesConfig>;

/// Create a failed validation result with error message
fn validation_error(error: impl Into<String>) -> ValidationResult {
    let mut result = ValidationResult::new(ValidationLevel::Schema);
    result.add_error(error.into());
    result
}

/// Create a successful validation result
fn validation_ok() -> ValidationResult {
    ValidationResult::new(ValidationLevel::Schema)
}

/// Configuration validator using shared validation framework
pub struct ConfigValidator {
    config_path: PathBuf,
    validation_level: ValidationLevel,
}

impl ConfigValidator {
    /// Create a new validator
    pub fn new(config_path: impl AsRef<Path>) -> Self {
        Self {
            config_path: config_path.as_ref().to_path_buf(),
            // For Aether, validate up to Business level (not Runtime)
            validation_level: ValidationLevel::Business,
        }
    }

    /// Validate configuration for a specific service
    pub async fn validate_service(&self, service: &str) -> Result<ValidationResult> {
        info!("Validate: {}", service);

        // Special handling for global configuration (no subdirectory)
        if service == "global" {
            return self.validate_global().await;
        }

        // Public service names are canonical; repository config directories use
        // concise role names.
        let config_directory = match service {
            "aether-io" => "io",
            "aether-automation" => "automation",
            other => other,
        };
        let service_config_path = self.config_path.join(config_directory);
        if !service_config_path.exists() {
            return Ok(validation_error(format!(
                "Service configuration directory not found: {:?}",
                service_config_path
            )));
        }

        // Use shared validation framework
        let result = match service {
            "aether-io" => self.validate_io().await?,
            "aether-automation" => self.validate_automation().await?,
            "rules" => self.validate_rules().await?,
            _ => return Ok(validation_error(format!("Unknown service: {}", service))),
        };

        if result.is_valid {
            debug!("{}: valid", service);
        } else {
            warn!("{}: invalid", service);
            for error in &result.errors {
                warn!("  {}", error);
            }
        }

        Ok(result)
    }

    /// Validate io configuration
    async fn validate_io(&self) -> Result<ValidationResult> {
        let yaml_path = self.config_path.join("io").join("io.yaml");

        // Check if file exists
        if !yaml_path.exists() {
            return Ok(validation_error(format!(
                "Missing required file: {:?}",
                yaml_path
            )));
        }

        // Load and validate using shared framework
        // Note: Errors from from_file already include file path + line number + reason
        let validator = IoValidator::from_file(&yaml_path)?;
        validator.validate(self.validation_level)
    }

    /// Validate automation configuration
    async fn validate_automation(&self) -> Result<ValidationResult> {
        let yaml_path = self.config_path.join("automation").join("automation.yaml");

        // Check if file exists
        if !yaml_path.exists() {
            return Ok(validation_error(format!(
                "Missing required file: {:?}",
                yaml_path
            )));
        }

        // Load and validate using shared framework
        // Note: Errors from from_file already include file path + line number + reason
        let validator = AutomationValidator::from_file(&yaml_path)?;
        validator.validate(self.validation_level)
    }

    /// Validate rules configuration
    async fn validate_rules(&self) -> Result<ValidationResult> {
        let yaml_path = self.config_path.join("rules").join("rules.yaml");

        // Check if file exists
        if !yaml_path.exists() {
            return Ok(validation_error(format!(
                "Missing required file: {:?}",
                yaml_path
            )));
        }

        // Load and validate using shared framework
        // Note: Errors from from_file already include file path + line number + reason
        let validator = RulesValidator::from_file(&yaml_path)?;
        validator.validate(self.validation_level)
    }

    /// Validate global configuration
    async fn validate_global(&self) -> Result<ValidationResult> {
        let yaml_path = self.config_path.join("global.yaml");

        // Check if file exists
        if !yaml_path.exists() {
            return Ok(validation_error(format!(
                "Missing global configuration file: {:?}",
                yaml_path
            )));
        }

        // Load YAML and perform basic validation
        let yaml_content = std::fs::read_to_string(&yaml_path)?;
        match serde_yml::from_str::<serde_yml::Value>(&yaml_content) {
            Ok(_) => {
                // Global config is valid YAML
                Ok(validation_ok())
            },
            Err(e) => {
                // YAML parsing failed
                Ok(validation_error(format!(
                    "Invalid YAML in {:?}: {}",
                    yaml_path, e
                )))
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_validator_with_shared_framework() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path();
        let io_dir = config_path.join("io");
        fs::create_dir_all(&io_dir).unwrap();

        // Create test config
        let config_content = r#"
service:
  name: aether-io
  description: Test Service
  port: 6000
channels: []
"#;
        fs::write(io_dir.join("io.yaml"), config_content).unwrap();

        // Test validation
        let validator = ConfigValidator::new(config_path);
        let result = validator.validate_service("aether-io").await.unwrap();

        // Zero channels is the fail-safe, not-yet-commissioned state.
        assert!(result.is_valid);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("No channels configured"))
        );
    }
}
