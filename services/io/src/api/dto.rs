#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[allow(unused_imports)] // Used in #[schema] macro expansion
use serde_json::json;
use std::collections::HashMap;
use utoipa::ToSchema;

pub use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};
pub use common::{
    AppError, ComponentHealth, ErrorInfo, ErrorResponse, HealthStatus, PaginatedResponse,
    ServiceStatus as SharedServiceStatus, SuccessResponse,
};

/// Control command (remote control)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ControlRequest {
    #[schema(example = 101)]
    pub point_id: u32,
    #[schema(example = 1)]
    pub value: u8, // 0 or 1
}

/// Adjustment command (setpoint)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdjustmentRequest {
    #[schema(example = 201)]
    pub point_id: u32,
    #[schema(example = 5000.0)]
    pub value: f64,
}

/// Batch control commands
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchControlRequest {
    #[schema(example = json!([{"point_id": 101, "value": 1}, {"point_id": 102, "value": 0}]))]
    pub commands: Vec<ControlRequest>,
}

/// Batch adjustment commands
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchAdjustmentRequest {
    #[schema(example = json!([{"point_id": 201, "value": 5000.0}, {"point_id": 202, "value": 380.0}]))]
    pub commands: Vec<AdjustmentRequest>,
}

/// Batch command execution result
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchCommandResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<BatchCommandError>,
}

/// Unified write response - supports both single and batch operations
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum WriteResponse {
    /// Single point write response
    Single(WritePointResponse),
    /// Batch write response
    Batch(BatchCommandResult),
}

/// Individual command error in batch
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchCommandError {
    #[schema(example = 101)]
    pub point_id: u32,
    #[schema(example = "Invalid control value")]
    pub error: String,
}

/// Control value request for RESTful endpoints
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ControlValueRequest {
    #[schema(example = 1)]
    pub value: u8, // 0 or 1
}

/// Adjustment value request for RESTful endpoints
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdjustmentValueRequest {
    #[schema(example = 50.0)]
    pub value: f64,
}

/// Simulation write request for acquisition-owned T/S points, single or batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WritePointRequest {
    /// Simulation point type: T/Telemetry or S/Signal.
    #[serde(alias = "point_type", alias = "t")]
    #[schema(example = "A")]
    pub r#type: String,

    /// Single point or batch points (automatically detected)
    #[serde(flatten)]
    pub data: WritePointData,
}

/// Write point data - supports single or batch writes
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum WritePointData {
    /// Single point write: {"id": "1", "value": 50.0}
    Single {
        #[serde(alias = "point_id")]
        #[schema(example = "1")]
        id: String,
        #[schema(example = 50.0)]
        value: f64,
    },
    /// Batch write: {"points": [{"id": "1", "value": 50.0}, ...]}
    Batch { points: Vec<PointValue> },
}

/// Point value for batch operations
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointValue {
    #[serde(alias = "point_id")]
    #[schema(example = "1")]
    pub id: String,
    #[schema(example = 50.0)]
    pub value: f64,
}

/// Write point response with operation details
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WritePointResponse {
    pub channel_id: u32,
    pub point_type: String,
    pub point_id: u32,
    pub value: f64,
    pub timestamp_ms: i64,
}

/// service status response
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceStatus {
    pub name: String,
    pub version: String,
    pub uptime: u64,
    #[schema(value_type = String, format = "date-time")]
    pub start_time: DateTime<Utc>,
    pub channels: u32,
    pub active_channels: u32,
}

/// channel status response for list endpoint
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelStatusResponse {
    pub id: u32,
    pub name: String,
    pub description: Option<String>, // Channel description
    pub protocol: String,
    pub enabled: bool, // Enabled state
    pub connected: bool,
    #[schema(value_type = String, format = "date-time")]
    pub last_update: DateTime<Utc>,
}

/// channel status response - Enhanced version combining API and `ComBase` requirements
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelStatusDto {
    pub id: u32,
    pub name: String,
    pub protocol: String,
    pub connected: bool,
    pub running: bool,
    #[schema(value_type = String, format = "date-time")]
    pub last_update: DateTime<Utc>,
    pub statistics: HashMap<String, serde_json::Value>,
}

impl From<crate::core::channels::ChannelStatus> for ChannelStatusDto {
    /// Convert from `ComBase` `ChannelStatus` to API `ChannelStatus`
    fn from(status: crate::core::channels::ChannelStatus) -> Self {
        Self {
            id: 0,                           // Will be filled by handler
            name: "Unknown".to_string(),     // Will be filled by handler
            protocol: "Unknown".to_string(), // Will be filled by handler
            connected: status.is_connected,
            running: status.is_connected, // Use is_connected as running status
            last_update: DateTime::<Utc>::from_timestamp(status.last_update, 0)
                .unwrap_or_else(Utc::now),
            statistics: HashMap::new(), // Will be filled by handler
        }
    }
}

/// Create a health status with memory and CPU checks
pub fn create_health_status(
    status: &str,
    uptime: u64,
    memory_usage: u64,
    cpu_usage: f64,
) -> HealthStatus {
    let service_status = match status {
        "healthy" | "ok" | "OK" => SharedServiceStatus::Healthy,
        "degraded" => SharedServiceStatus::Degraded,
        _ => SharedServiceStatus::Unhealthy,
    };

    let health_check = |ok: bool, msg: String| ComponentHealth {
        status: if ok {
            SharedServiceStatus::Healthy
        } else {
            SharedServiceStatus::Degraded
        },
        message: Some(msg),
        duration_ms: None,
    };

    let checks = HashMap::from([
        (
            "memory".into(),
            health_check(
                memory_usage < 1_073_741_824,
                format!("Memory usage: {} bytes", memory_usage),
            ),
        ),
        (
            "cpu".into(),
            health_check(cpu_usage < 80.0, format!("CPU usage: {:.2}%", cpu_usage)),
        ),
    ]);

    HealthStatus {
        status: service_status,
        service: "aether-io".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: uptime,
        timestamp: chrono::Utc::now(),
        checks,
        system: Some(serde_json::json!({
            "process_cpu_percent": cpu_usage,
            "process_memory_mb": memory_usage / 1024 / 1024
        })),
    }
}

/// channel operation request
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelOperation {
    pub operation: String, // "start", "stop", "restart"
}

/// Channel creation request
///
/// - `channel_id` is optional (auto-assigned if not provided)
/// - `name` must be unique across all channels
/// - `parameters` are protocol-specific (see OpenAPI schema examples)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelCreateRequest {
    /// Channel ID (optional, auto-assigned via MAX+1 if null)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(example = json!(null), nullable = true)]
    pub channel_id: Option<u32>,

    /// Channel name (must be unique)
    #[schema(example = "PV Inverter Channel")]
    pub name: String,

    #[schema(example = "Primary PV inverter communication channel")]
    pub description: Option<String>,

    /// Protocol type: modbus_tcp, modbus_rtu, can, virtual
    #[schema(example = "modbus_tcp", value_type = String)]
    pub protocol: String,

    /// Enable channel immediately after creation (default: true)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(example = true, nullable = true)]
    pub enabled: Option<bool>,

    /// Protocol-specific parameters (see OpenAPI schema for per-protocol fields)
    #[schema(value_type = Object, example = json!({"host": "192.168.1.100", "port": 502, "connect_timeout_ms": 5000, "max_batch_size": 64}))]
    pub parameters: HashMap<String, serde_json::Value>,

    /// Logging configuration (optional, defaults to disabled)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<ChannelLoggingConfig>,
}

/// Channel configuration update request (PATCH semantics, null fields unchanged)
/// If `channel_id` differs from path ID, triggers ID migration.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelConfigUpdateRequest {
    /// New channel ID (triggers migration if different from path ID)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(example = 6, nullable = true)]
    pub channel_id: Option<u32>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub protocol: Option<String>,
    pub parameters: Option<HashMap<String, serde_json::Value>>,
    /// Logging configuration for this channel
    pub logging: Option<ChannelLoggingConfig>,
}

/// Channel enabled state update request
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelEnabledRequest {
    pub enabled: bool,
}

/// Channel CRUD operation result
/// Uses ChannelCore to eliminate field duplication
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelCrudResult {
    /// Core channel fields (id, name, description, protocol, enabled)
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub core: ChannelCore,

    /// Runtime status
    #[schema(example = "running")]
    pub runtime_status: String, // "running", "stopped", "error", "not_started"

    /// Operation message
    #[schema(example = "Channel configuration updated successfully")]
    pub message: Option<String>,
}

impl From<(&ChannelConfig, String, Option<String>)> for ChannelCrudResult {
    fn from((config, runtime_status, message): (&ChannelConfig, String, Option<String>)) -> Self {
        Self {
            core: config.core.clone(),
            runtime_status,
            message,
        }
    }
}

/// Reload configuration result
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReloadConfigResult {
    #[schema(example = 3)]
    pub total_channels: usize,
    #[schema(example = json!([1, 2]))]
    pub channels_added: Vec<u32>,
    #[schema(example = json!([3]))]
    pub channels_updated: Vec<u32>,
    #[schema(example = json!([]))]
    pub channels_removed: Vec<u32>,
    #[schema(example = json!([]))]
    pub errors: Vec<String>,
}

/// Routing cache reload result
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RoutingReloadResult {
    /// Number of C2M routing mappings loaded
    #[schema(example = 150)]
    pub c2m_count: usize,
    /// Number of M2C routing mappings loaded
    #[schema(example = 80)]
    pub m2c_count: usize,
    /// Number of C2C routing mappings loaded
    #[schema(example = 20)]
    pub c2c_count: usize,
    /// Error messages (if any)
    #[schema(example = json!([]))]
    pub errors: Vec<String>,
    /// Reload duration in milliseconds
    #[schema(example = 25)]
    pub duration_ms: u64,
}

/// Complete channel details (configuration + runtime status + statistics)
/// Uses ChannelConfig to eliminate field duplication
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelDetail {
    /// Complete channel configuration (includes core fields + parameters + logging)
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub config: ChannelConfig,

    /// Runtime status information
    pub runtime_status: ChannelRuntimeStatus,

    /// Point counts by type
    pub point_counts: PointCounts,
}

/// Channel runtime status information
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelRuntimeStatus {
    pub connected: bool,
    pub running: bool,
    #[schema(value_type = String, format = "date-time")]
    pub last_update: DateTime<Utc>,
    pub statistics: HashMap<String, serde_json::Value>,
}

/// Point counts by type
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointCounts {
    pub telemetry: usize,
    pub signal: usize,
    pub control: usize,
    pub adjustment: usize,
}

/// Channel list query parameters (pagination and filtering)
#[derive(Debug, Deserialize, ToSchema)]
pub struct ChannelListQuery {
    /// Page number (starting from 1)
    #[serde(default = "default_page")]
    pub page: usize,

    /// Items per page
    #[serde(default = "default_page_size")]
    pub page_size: usize,

    /// Filter by protocol type
    pub protocol: Option<String>,

    /// Filter by enabled status
    pub enabled: Option<bool>,

    /// Filter by connection status
    pub connected: Option<bool>,
}

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    20
}

/// Auto-reload query parameter (default true = changes take effect immediately)
#[derive(Debug, Deserialize, ToSchema)]
pub struct AutoReloadQuery {
    #[serde(default = "default_auto_reload")]
    pub auto_reload: bool,
}

fn default_auto_reload() -> bool {
    true
}

/// Parameter change classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
pub enum ParameterChangeType {
    /// Only metadata changed (name, description) - no reload needed
    MetadataOnly,
    /// Non-critical parameters changed (timeout, retry) - may need reload
    NonCritical,
    /// Critical parameters changed (host, port, slave_id) - must reload
    Critical,
}

/// Point definition (from Points table)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointDefinition {
    pub point_id: u32,
    pub signal_name: String,
    pub scale: f64,
    pub offset: f64,
    pub unit: String,
    pub data_type: String,
    pub reverse: bool,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub protocol_mapping: Option<serde_json::Value>,
}

/// Grouped points response for channel points API
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GroupedPoints {
    pub telemetry: Vec<PointDefinition>,
    pub signal: Vec<PointDefinition>,
    pub control: Vec<PointDefinition>,
    pub adjustment: Vec<PointDefinition>,
}

/// Point list response
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointListResponse {
    #[schema(example = 1)]
    pub channel_id: u32,
    #[schema(example = "T")]
    pub point_type: String, // "T", "S", "C", "A"
    pub total_points: usize,
    pub mapped_points: usize,   // Points with mapping
    pub unmapped_points: usize, // Reserve points without mapping
    pub points: Vec<PointDefinition>,
}

/// Single point mapping detail (for GET response)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointMappingDetail {
    #[schema(example = 101)]
    pub point_id: u32,
    #[schema(example = "DC_Voltage")]
    pub signal_name: String,
    /// Protocol-specific mapping data (JSON)
    #[schema(value_type = Object)]
    pub protocol_data: serde_json::Value,
}

/// Grouped mappings response for channel mappings API
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GroupedMappings {
    pub telemetry: Vec<PointMappingDetail>,
    pub signal: Vec<PointMappingDetail>,
    pub control: Vec<PointMappingDetail>,
    pub adjustment: Vec<PointMappingDetail>,
}

/// Grouped mappings update request
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GroupedMappingsUpdateRequest {
    /// Mappings grouped by point type
    #[serde(flatten)]
    pub mappings: GroupedMappings,
    /// Validate only without writing to database
    #[serde(default)]
    #[schema(example = false)]
    pub validate_only: bool,
}

/// Single point mapping item (for PUT request)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointMappingItem {
    #[schema(example = 101)]
    pub point_id: u32,

    /// Four-remote type: T/S/C/A
    #[schema(value_type = String, example = "T")]
    pub four_remote: String,

    /// Protocol-specific mapping data (JSON)
    #[schema(value_type = Object, example = json!({"slave_id": 1, "register_address": 100}))]
    pub protocol_data: serde_json::Value,
}

/// Request to batch update protocol mappings
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MappingBatchUpdateRequest {
    pub mappings: Vec<PointMappingItem>,
    #[serde(default)]
    pub reload_channel: bool,
    #[serde(default)]
    pub validate_only: bool,
    /// Update mode: replace (overwrite) or merge (shallow merge)
    #[serde(default)]
    pub mode: MappingUpdateMode,
}

/// Result of batch mapping update operation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MappingBatchUpdateResult {
    pub updated_count: usize,
    pub channel_reloaded: bool,
    pub validation_errors: Vec<String>,
    pub message: String,
}

/// Mapping update mode
#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MappingUpdateMode {
    Replace,
    #[default]
    Merge,
}

/// Mapping list response (for batch read)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MappingListResponse {
    #[schema(example = 1)]
    pub channel_id: u32,
    #[schema(example = "modbus_tcp")]
    pub protocol: String,
    #[schema(example = "T")]
    pub point_type: String, // "T", "S", "C", "A"
    pub total_mappings: usize,
    pub mappings: Vec<PointMappingDetail>,
}

// ============================================================================
// Channel Template DTOs
// ============================================================================

/// Template list item (metadata only, no snapshots)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TemplateListItem {
    pub template_id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub protocol: String,
    pub point_counts: PointCounts,
    pub created_at: String,
}

/// Template detail (includes full snapshots)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TemplateDetail {
    pub template_id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_channel_id: Option<i64>,
    #[schema(value_type = Object)]
    pub points_snapshot: serde_json::Value,
    #[schema(value_type = Object)]
    pub mappings_snapshot: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create a template from an existing channel
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTemplateFromChannelReq {
    #[schema(example = "PCS Modbus Template")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to create a template manually (direct JSON)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTemplateReq {
    #[schema(example = "Battery BMS Template")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[schema(example = "modbus_tcp")]
    pub protocol: String,
    #[schema(value_type = Object)]
    pub points_snapshot: serde_json::Value,
    #[schema(value_type = Object)]
    pub mappings_snapshot: serde_json::Value,
}

/// Request to update template metadata
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateTemplateReq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to apply a template to a channel
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyTemplateReq {
    /// Override slave_id in all protocol mappings (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = json!(null), nullable = true)]
    pub slave_id_override: Option<u8>,
    /// Clear existing points before applying (default: true)
    #[serde(default = "default_clear_existing")]
    #[schema(example = true)]
    pub clear_existing: bool,
}

fn default_clear_existing() -> bool {
    true
}

/// Template list query parameters
#[derive(Debug, Deserialize, ToSchema)]
pub struct TemplateListQuery {
    /// Filter by protocol type
    pub protocol: Option<String>,
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn test_service_status_serialization() {
        let start_time = Utc::now();
        let status = ServiceStatus {
            name: "TestService".to_string(),
            version: "1.0.0".to_string(),
            uptime: 3600,
            start_time,
            channels: 5,
            active_channels: 3,
        };

        let serialized = serde_json::to_string(&status).unwrap();
        assert!(serialized.contains("TestService"));
        assert!(serialized.contains("1.0.0"));
        assert!(serialized.contains("3600"));
    }

    #[test]
    fn test_channel_status_serialization() {
        let now = Utc::now();
        let mut parameters = HashMap::new();
        parameters.insert("timeout".to_string(), json!(5000));
        parameters.insert("slave_id".to_string(), json!(1));

        let status = ChannelStatusDto {
            id: 1,
            name: "Test Channel".to_string(),
            protocol: "modbus_tcp".to_string(),
            connected: true,
            running: true,
            last_update: now,
            statistics: parameters,
        };

        let serialized = serde_json::to_string(&status).unwrap();
        assert!(serialized.contains('1'));
        assert!(serialized.contains("modbus_tcp"));
        assert!(serialized.contains("true"));
    }

    #[test]
    fn test_health_status_serialization() {
        let health = create_health_status("healthy", 7200, 1_024_000, 15.5);

        // Verify health status fields (without comparing enums)
        assert_eq!(health.service, "aether-io");
        assert_eq!(health.uptime_seconds, 7200);
        assert!(health.checks.contains_key("memory"));
        assert!(health.checks.contains_key("cpu"));

        // Verify serialization contains expected values
        let serialized = serde_json::to_string(&health).unwrap();
        assert!(serialized.contains("healthy"));
        assert!(serialized.contains("7200"));
        assert!(serialized.contains("aether-io"));
    }

    #[test]
    fn test_channel_operation_deserialization() {
        let json_data = r#"{"operation": "start"}"#;
        let operation: ChannelOperation = serde_json::from_str(json_data).unwrap();
        assert_eq!(operation.operation, "start");

        let json_data = r#"{"operation": "stop"}"#;
        let operation: ChannelOperation = serde_json::from_str(json_data).unwrap();
        assert_eq!(operation.operation, "stop");

        let json_data = r#"{"operation": "restart"}"#;
        let operation: ChannelOperation = serde_json::from_str(json_data).unwrap();
        assert_eq!(operation.operation, "restart");
    }

    #[test]
    fn test_error_response_serialization() {
        let error_info = ErrorInfo::new("Not found").with_code(404);
        let error = ErrorResponse {
            success: false,
            error: error_info,
        };

        let serialized = serde_json::to_string(&error).unwrap();
        assert!(serialized.contains("404"));
        assert!(serialized.contains("Not found"));
        assert!(serialized.contains("\"success\":false"));
    }

    #[test]
    fn test_success_response() {
        let data = "test data".to_string();
        let response = SuccessResponse::new(data);

        assert_eq!(response.data, "test data");
        assert!(response.metadata.is_empty());

        let serialized = serde_json::to_string(&response).unwrap();
        assert!(serialized.contains("test data"));
        assert!(serialized.contains("data"));
    }

    #[test]
    fn test_error_response() {
        let error = ErrorInfo::new("Something went wrong");
        let response = ErrorResponse {
            success: false,
            error,
        };

        assert_eq!(response.error.message, "Something went wrong");
        assert!(!response.success);

        let serialized = serde_json::to_string(&response).unwrap();
        assert!(serialized.contains("Something went wrong"));
        assert!(serialized.contains("error"));
        assert!(serialized.contains("\"success\":false"));
    }

    #[test]
    fn test_channel_status_with_empty_parameters() {
        let now = Utc::now();
        let status = ChannelStatusDto {
            id: 1,
            name: "Simple Channel".to_string(),
            protocol: "Virtual".to_string(),
            connected: false,
            running: false,
            last_update: now,
            statistics: HashMap::new(),
        };

        let serialized = serde_json::to_string(&status).unwrap();
        assert!(serialized.contains('1'));
        assert!(serialized.contains("false"));
    }

    #[test]
    fn test_combase_channel_status_conversion() {
        let combase_status = crate::core::channels::ChannelStatus {
            is_connected: true,
            last_update: 1_234_567_890,
        };
        let api_status = ChannelStatusDto::from(combase_status);

        assert_eq!(api_status.id, 0); // Default value
        assert_eq!(api_status.name, "Unknown");
        assert_eq!(api_status.protocol, "Unknown");
        assert!(api_status.connected);
        assert!(api_status.statistics.is_empty());
    }

    #[test]
    fn test_channel_create_request_deserialization() {
        let json_data = r#"{
            "channel_id": 1001,
            "name": "Test Channel",
            "description": "Test channel for Modbus TCP",
            "protocol": "modbus_tcp",
            "enabled": true,
            "parameters": {
                "host": "192.168.1.100",
                "port": 502,
                "slave_id": 1
            }
        }"#;

        let request: ChannelCreateRequest = serde_json::from_str(json_data).unwrap();
        assert_eq!(request.channel_id, Some(1001));
        assert_eq!(request.name, "Test Channel");
        assert_eq!(
            request.description,
            Some("Test channel for Modbus TCP".to_string())
        );
        assert_eq!(request.protocol, "modbus_tcp");
        assert_eq!(request.enabled, Some(true));
        assert_eq!(request.parameters.len(), 3);
        assert_eq!(
            request.parameters.get("host"),
            Some(&json!("192.168.1.100"))
        );
        assert_eq!(request.parameters.get("port"), Some(&json!(502)));
        assert_eq!(request.parameters.get("slave_id"), Some(&json!(1)));
    }

    #[test]
    fn test_channel_config_update_request_deserialization() {
        let json_data = r#"{
            "name": "Updated Channel Name",
            "description": "Updated description",
            "parameters": {
                "timeout": 5000
            }
        }"#;

        let request: ChannelConfigUpdateRequest = serde_json::from_str(json_data).unwrap();
        assert!(request.channel_id.is_none()); // Not provided, defaults to None
        assert_eq!(request.name, Some("Updated Channel Name".to_string()));
        assert_eq!(request.description, Some("Updated description".to_string()));
        assert!(request.protocol.is_none());
        assert!(request.parameters.is_some());

        let params = request.parameters.unwrap();
        assert_eq!(params.get("timeout"), Some(&json!(5000)));
    }

    #[test]
    fn test_channel_config_update_request_with_channel_id() {
        let json_data = r#"{
            "channel_id": 6,
            "name": "Migrated Channel"
        }"#;

        let request: ChannelConfigUpdateRequest = serde_json::from_str(json_data).unwrap();
        assert_eq!(request.channel_id, Some(6));
        assert_eq!(request.name, Some("Migrated Channel".to_string()));
        assert!(request.description.is_none());
        assert!(request.protocol.is_none());
        assert!(request.parameters.is_none());
        assert!(request.logging.is_none());
    }
}
