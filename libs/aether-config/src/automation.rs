//! Automation service configuration structures
//!
//! This module contains all automation-specific configuration types.

use anyhow::Result;
use common::{ApiConfig, BaseServiceConfig, ValidationLevel, ValidationResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use utoipa::ToSchema;

// ============================================================================
// Default Functions
// ============================================================================

/// Default API configuration for automation (port 6002)
fn default_automation_api() -> ApiConfig {
    ApiConfig {
        host: common::DEFAULT_API_HOST.to_string(),
        port: 6002,
    }
}

use common::serde_helpers::bool_true;

// ============================================================================
// Core Configuration
// ============================================================================

/// Default port for automation service
pub const DEFAULT_PORT: u16 = 6002;

/// Automation service configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationConfig {
    /// Base service configuration
    #[serde(flatten)]
    pub service: BaseServiceConfig,

    /// API configuration (has default value)
    #[serde(default = "default_automation_api")]
    pub api: ApiConfig,

    /// Path to products directory
    #[serde(default)]
    pub products_path: Option<String>,

    /// Path to instances configuration
    #[serde(default)]
    pub instances_path: Option<String>,

    /// Whether to auto-load instances at startup
    #[serde(default = "bool_true")]
    pub auto_load_instances: bool,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        let service = BaseServiceConfig {
            name: "aether-automation".to_string(),
            ..Default::default()
        };

        let api = ApiConfig {
            host: common::DEFAULT_API_HOST.to_string(),
            port: 6002, // automation default port
        };

        Self {
            service,
            api,
            products_path: None,
            instances_path: Some("config/automation/instances.yaml".to_string()),
            auto_load_instances: true,
        }
    }
}

// ============================================================================
// Database Schema Definitions
// ============================================================================

/// Re-export service config table SQL from common
pub use common::SERVICE_CONFIG_TABLE;

/// Re-export sync metadata table SQL from common
pub use common::SYNC_METADATA_TABLE;

/// Re-export DDL from common (single source of truth — production code in
/// aether and automation reads these constants; the Schema-macro variants that
/// previously lived here drifted from the canonical SQL.)
pub use common::test_utils::schema::{
    ACTION_ROUTING_TABLE, CONFIGURATION_REVISIONS_TABLE, INSTANCE_PROPERTIES_TABLE,
    INSTANCES_TABLE, MEASUREMENT_ROUTING_TABLE,
};

// ============================================================================
// Product & Point Types
// ============================================================================

/// Complete product definition with nested structure
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Product {
    /// Product name (unique identifier)
    pub product_name: String,

    /// Parent product name for hierarchy
    pub parent_name: Option<String>,

    /// Measurement points (includes physical and virtual)
    #[serde(default)]
    pub measurements: Vec<MeasurementPoint>,

    /// Action points
    #[serde(default)]
    pub actions: Vec<ActionPoint>,

    /// Property templates
    #[serde(default)]
    pub properties: Vec<PropertyTemplate>,
}

/// Measurement point definition (M type)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPoint {
    /// Measurement point ID (unique within product)
    #[serde(alias = "id", alias = "index")]
    pub measurement_id: u32,

    /// Point name
    pub name: String,

    /// Unit of measurement
    pub unit: Option<String>,

    /// Point description
    pub description: Option<String>,
}

/// Action point definition (A type)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActionPoint {
    /// Action point ID (unique within product)
    #[serde(alias = "id", alias = "index")]
    pub action_id: u32,

    /// Action name
    pub name: String,

    /// Unit for adjustment actions
    pub unit: Option<String>,

    /// Point description
    pub description: Option<String>,
}

/// Property template for instance configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PropertyTemplate {
    /// Property ID (unique within product)
    #[serde(alias = "id", alias = "index")]
    pub property_id: i32,

    /// Property name
    pub name: String,

    /// Unit of the property
    pub unit: Option<String>,

    /// Property description
    pub description: Option<String>,
}

// ============================================================================
// Instance Types
// ============================================================================

/// Instance core fields (shared between Config and API responses)
/// These fields represent the essential instance identity and properties
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceCore {
    /// Unique instance identifier (numeric)
    pub instance_id: u32,

    /// Human-readable instance name used in API paths and operator output
    pub instance_name: String,

    /// Associated product name
    pub product_name: String,

    /// Parent instance ID for topology hierarchy (None for root instances)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,

    /// Instance properties (key-value pairs supporting multiple types)
    #[serde(default)]
    pub properties: HashMap<String, serde_json::Value>,
}

/// Instance definition for runtime devices
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Core instance fields
    #[serde(flatten)]
    pub core: InstanceCore,

    /// Creation timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Instance {
    /// Convenient accessor for instance ID
    pub fn instance_id(&self) -> u32 {
        self.core.instance_id
    }

    /// Convenient accessor for instance name
    pub fn instance_name(&self) -> &str {
        &self.core.instance_name
    }

    /// Convenient accessor for product name
    pub fn product_name(&self) -> &str {
        &self.core.product_name
    }
}

/// Request to create a new instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInstanceRequest {
    /// Instance ID — `Some(id)` to use a specific ID, `None` to let DB auto-assign
    pub instance_id: Option<u32>,

    /// Human-readable instance name
    pub instance_name: String,

    /// Product name
    pub product_name: String,

    /// Parent instance ID for topology hierarchy (None for root instances like Station)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,

    /// Instance properties (supports multiple types: numbers, strings, etc.)
    #[serde(default)]
    pub properties: HashMap<String, serde_json::Value>,
}

/// Product hierarchy using tuples (following CLAUDE.md)
pub type ProductHierarchy = Vec<(String, Option<String>)>;

/// Topology tree node for hierarchical instance display
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TopologyNode {
    pub instance_id: u32,
    pub instance_name: String,
    pub product_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,
}

// ============================================================================
// Validation implementations
// ============================================================================

impl common::ConfigValidator for AutomationConfig {
    fn validate_syntax(&self) -> Result<ValidationResult> {
        Ok(ValidationResult::new(ValidationLevel::Syntax))
    }

    fn validate_schema(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Schema);

        // Validate common components
        self.service.validate(&mut result);
        self.api.validate(&mut result);

        // Service-specific validation
        if let Some(products_path) = &self.products_path
            && products_path.is_empty()
        {
            result.add_error("Products path cannot be empty if specified".to_string());
        }

        if let Some(instances_path) = &self.instances_path
            && instances_path.is_empty()
        {
            result.add_error("Instances path cannot be empty if specified".to_string());
        }

        Ok(result)
    }

    fn validate_business(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Business);

        // Business rule: Warn about auto-loading instances
        if !self.auto_load_instances {
            result.add_warning(
                "Instance auto-loading is disabled, instances must be created manually".to_string(),
            );
        }

        // Validate paths exist if specified
        if let Some(products_path) = &self.products_path {
            let path = std::path::Path::new(products_path);
            if !path.exists() {
                result.add_warning(format!("Products path does not exist: {}", products_path));
            }
        }

        if let Some(instances_path) = &self.instances_path {
            let path = std::path::Path::new(instances_path);
            if !path.exists() {
                result.add_warning(format!("Instances path does not exist: {}", instances_path));
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

/// Type alias for backward compatibility - use GenericValidator directly for new code
pub type AutomationValidator = common::GenericValidator<AutomationConfig>;

// ============================================================================
// Centralized SQL Queries for Automation
// ============================================================================
// ============================================================================
// Rules Configuration Types (for YAML config export/import)
// ============================================================================

/// Default API configuration for rules (port 6002, merged into automation)
fn default_rules_api() -> ApiConfig {
    ApiConfig {
        host: common::DEFAULT_API_HOST.to_string(),
        port: 6002,
    }
}

/// Rules service configuration
/// Used by aether for YAML config export/import
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesConfig {
    /// Base service configuration
    #[serde(flatten)]
    pub service: BaseServiceConfig,

    /// API configuration (has default value)
    #[serde(default = "default_rules_api")]
    pub api: ApiConfig,

    /// Execution configuration
    #[serde(default)]
    pub execution: ExecutionConfig,
}

impl Default for RulesConfig {
    fn default() -> Self {
        let service = BaseServiceConfig {
            name: "rules".to_string(),
            ..Default::default()
        };

        let api = ApiConfig {
            host: common::DEFAULT_API_HOST.to_string(),
            port: 6002, // merged into automation
        };

        Self {
            service,
            api,
            execution: ExecutionConfig::default(),
        }
    }
}

/// Rule execution configuration (reserved for future use)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionConfig {}

/// Rule core fields (shared between Config and API responses)
/// These fields represent the essential rule identity and state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleCore {
    /// Rule ID
    pub id: i64,

    /// Rule name
    pub name: String,

    /// Rule description
    pub description: Option<String>,

    /// Whether the rule is enabled
    #[serde(default = "bool_true")]
    pub enabled: bool,

    /// Priority (higher number = higher priority)
    #[serde(default)]
    pub priority: u32,
}

/// Individual rule configuration for vue-flow/node-red
/// Used by aether for YAML config export/import
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConfig {
    /// Core rule fields
    #[serde(flatten)]
    pub core: RuleCore,

    /// Complete flow graph JSON (nodes, edges, viewport, etc.)
    pub flow_json: serde_json::Value,
}

impl RuleConfig {
    /// Convenient accessor for rule ID
    pub fn id(&self) -> i64 {
        self.core.id
    }

    /// Convenient accessor for rule name
    pub fn name(&self) -> &str {
        &self.core.name
    }

    /// Convenient accessor for enabled status
    pub fn is_enabled(&self) -> bool {
        self.core.enabled
    }

    /// Convenient accessor for priority
    pub fn priority(&self) -> u32 {
        self.core.priority
    }
}

// ============================================================================
// RulesConfig Validation Implementation
// ============================================================================

impl common::ConfigValidator for RulesConfig {
    fn validate_syntax(&self) -> Result<ValidationResult> {
        Ok(ValidationResult::new(ValidationLevel::Syntax))
    }

    fn validate_schema(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Schema);

        // Validate common components
        self.service.validate(&mut result);
        self.api.validate(&mut result);

        Ok(result)
    }

    fn validate_business(&self) -> Result<ValidationResult> {
        let result = ValidationResult::new(ValidationLevel::Business);
        Ok(result)
    }

    fn validate_runtime(&self) -> Result<ValidationResult> {
        let mut result = ValidationResult::new(ValidationLevel::Runtime);

        // Port availability check
        self.api.validate_runtime(&mut result);

        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn default_automation_configs_do_not_serialize_redis() {
        for serialized in [
            serde_json::to_value(AutomationConfig::default()).unwrap(),
            serde_json::to_value(RulesConfig::default()).unwrap(),
        ] {
            assert!(
                serialized.get("redis").is_none(),
                "SHM-only automation config must not expose Redis"
            );
        }
    }

    #[test]
    fn instance_schema_has_no_external_store_mapping_fields() {
        let instance = Instance {
            core: InstanceCore {
                instance_id: 7,
                instance_name: "pump-7".to_string(),
                product_name: "pump".to_string(),
                parent_id: None,
                properties: HashMap::new(),
            },
            created_at: None,
        };

        let serialized = serde_json::to_value(instance).unwrap();
        assert!(serialized.get("measurement_mappings").is_none());
        assert!(serialized.get("action_mappings").is_none());
    }
}
