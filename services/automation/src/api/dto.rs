//! Data Transfer Objects for Model Service API
//!
//! This module contains all request and response structures used by the API endpoints.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use common::FourRemote;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

// Import Core types for zero-duplication architecture
use crate::config::{Instance, InstanceCore};

// Import shared serde helpers
use common::serde_helpers::{deserialize_optional_i32, deserialize_optional_u32};

// === Custom Deserializer for FourRemote ===

/// Deserialize `Option<FourRemote>` from null, empty string, or valid enum value
fn deserialize_optional_four_remote<'de, D>(deserializer: D) -> Result<Option<FourRemote>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => {
            // Parse directly using FromStr - avoids clone and intermediate Value
            s.parse::<FourRemote>()
                .map(Some)
                .map_err(serde::de::Error::custom)
        },
    }
}

// === Query Parameters ===

/// Query parameter for filtering by data type
#[derive(Deserialize, ToSchema)]
pub struct DataTypeQuery {
    #[serde(rename = "type")]
    pub data_type: Option<String>, // 'measurement', 'action', or null for both
}

// === Parameter Management ===

/// Request to update instance parameter routing
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct RoutingUpdate {
    pub routings: HashMap<String, String>,
    #[serde(default)]
    pub routing_type: RoutingType,
}

/// Type of routing being updated
#[derive(Debug, Clone, Deserialize, Serialize, Default, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RoutingType {
    #[default]
    Measurement,
    Action,
}

/// API-layer subset of `aether_model::PointType` (M/A only).
///
/// Used in RoutingRequest to explicitly specify whether the point_id
/// refers to a measurement point or an action point.
/// Unlike the full `aether_model::PointType` which includes T/S/C/A,
/// automation only routes Measurement and Action points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum PointType {
    /// Measurement point routing
    #[serde(rename = "M")]
    Measurement,
    /// Action point routing
    #[serde(rename = "A")]
    Action,
}

// === Routing Management ===

/// Request to create or update a channel-to-instance point routing
///
/// `channel_id`, `four_remote`, and `channel_point_id` form a unit to identify a channel point.
/// All three must be present for a valid routing, or all null/empty to unbind the routing.
///
/// Supports null, empty string "", or omitted fields to indicate "unbind routing".
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct RoutingRequest {
    /// Point type: "M" for measurement, "A" for action
    #[schema(example = "M")]
    pub point_type: PointType,
    /// Point ID (measurement_id or action_id based on point_type)
    #[schema(example = 101)]
    pub point_id: u32,
    #[schema(example = 1)]
    #[serde(default, deserialize_with = "deserialize_optional_i32")]
    pub channel_id: Option<i32>,
    #[schema(value_type = Option<String>, example = "T")]
    #[serde(default, deserialize_with = "deserialize_optional_four_remote")]
    pub four_remote: Option<FourRemote>,
    #[schema(example = 101)]
    #[serde(default, deserialize_with = "deserialize_optional_u32")]
    pub channel_point_id: Option<u32>,
    /// Explicit confirmation required when this request changes an action route.
    #[serde(default)]
    #[schema(default = false, example = true)]
    pub confirmed: bool,
}

/// Request to create or update routing for a single point
///
/// `channel_id`, `four_remote`, and `channel_point_id` can all be null to unbind the routing
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct SinglePointRoutingRequest {
    #[schema(example = 1)]
    pub channel_id: Option<i32>,
    #[schema(value_type = Option<String>, example = "T")]
    pub four_remote: Option<FourRemote>,
    #[schema(example = 101)]
    pub channel_point_id: Option<u32>,
    #[serde(default = "default_enabled")]
    #[schema(example = true)]
    pub enabled: bool,
    /// Explicitly confirms a high-risk physical command-topology change.
    #[serde(default)]
    #[schema(default = false, example = true)]
    pub confirmed: bool,
}

/// Request to toggle routing enabled state for a single point
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ToggleRoutingRequest {
    #[schema(example = true)]
    pub enabled: bool,
    /// Explicitly confirms a high-risk physical command-topology change.
    #[serde(default)]
    #[schema(default = false, example = true)]
    pub confirmed: bool,
}

/// Confirmation body for destructive action-routing commands.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, ToSchema)]
pub struct RoutingMutationConfirmation {
    /// Explicitly confirms a high-risk physical command-topology change.
    #[serde(default)]
    #[schema(default = false, example = true)]
    pub confirmed: bool,
}

/// Channel point kinds that are valid destinations for an action route.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub enum ActionRoutingFourRemote {
    /// Channel command point (`C`).
    #[serde(rename = "C")]
    Control,
    /// Channel adjustment point (`A`).
    #[serde(rename = "A")]
    Adjustment,
}

/// Swagger request body for a governed action-route upsert or unbind command.
///
/// This is intentionally separate from [`SinglePointRoutingRequest`]:
/// measurement routing supports T/S destinations without high-risk
/// confirmation, while action routing supports only C/A and requires it.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingUpsertBody {
    #[schema(example = 1)]
    pub channel_id: Option<i32>,
    pub four_remote: Option<ActionRoutingFourRemote>,
    #[schema(example = 201)]
    pub channel_point_id: Option<u32>,
    #[schema(default = true, example = true)]
    pub enabled: bool,
    /// Required explicit confirmation for the physical command-topology change.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Swagger request body for a governed action-route enable/disable command.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingToggleBody {
    #[schema(example = true)]
    pub enabled: bool,
    /// Required explicit confirmation for the physical command-topology change.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Swagger request body for a governed action-route deletion command.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingConfirmationBody {
    /// Required explicit confirmation for the physical command-topology change.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Terminal audit state returned after an accepted action-routing mutation.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingAuditState {
    /// `recorded`, or `incomplete` when terminal audit persistence degraded.
    #[schema(example = "recorded")]
    pub status: String,
    /// Accepted mutations are never safe for automatic retry.
    #[schema(example = false)]
    pub retryable: bool,
    /// Present only when terminal audit persistence is incomplete.
    pub message: Option<String>,
}

/// Stable application-command result nested below the success envelope.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingMutationData {
    #[schema(example = "Routing updated for action point 201")]
    pub message: String,
    /// Caller-supplied or generated audit correlation identifier.
    #[schema(example = "018f0000-0000-7000-8000-000000000007")]
    pub request_id: String,
    /// Stable mutation kind: `upsert`, `delete`, `enable`, or `disable`.
    #[schema(example = "upsert")]
    pub operation: String,
    #[schema(example = 1)]
    pub affected_routes: u64,
    pub audit: ActionRoutingAuditState,
    /// Always false after the application command has been accepted.
    #[schema(example = false)]
    pub retryable: bool,
}

/// Success envelope for a governed action-routing mutation.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingMutationResponse {
    #[schema(example = true)]
    pub success: bool,
    pub data: ActionRoutingMutationData,
}

/// Request to upsert a single instance property value.
///
/// The `value` is an arbitrary JSON value (number, string, bool, object).
/// `property_id` is passed in the URL path; the handler rejects ids that
/// don't appear in the instance's product PropertyTemplate.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UpsertPropertyRequest {
    #[schema(value_type = Object, example = json!(5000.0))]
    pub value: serde_json::Value,
}

/// Default value for enabled field (true)
fn default_enabled() -> bool {
    true
}

// === Instance Management ===

/// Request to create a new instance from a product template
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct CreateInstanceDto {
    #[schema(example = 1)]
    pub instance_id: Option<u32>, // Optional - auto-generated if not provided
    #[schema(example = "pump_01")]
    pub instance_name: String,
    #[schema(example = "pump")]
    pub product_name: String,
    /// Parent instance ID for topology hierarchy (required for non-root products)
    #[schema(example = 1)]
    #[serde(default)]
    pub parent_id: Option<u32>,
    #[schema(value_type = Object, example = json!({"max_flow_lpm": 500.0, "manufacturer": "Example Corp"}))]
    pub properties: Option<HashMap<String, serde_json::Value>>,
}

/// Request to update an existing instance
///
/// Supports updating instance_name and/or properties.
/// At least one field must be provided.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UpdateInstanceDto {
    /// New instance name (optional, must be unique if provided)
    #[schema(example = "pump_renamed")]
    pub instance_name: Option<String>,

    /// Updated properties (optional)
    #[schema(value_type = Option<Object>, example = json!({"max_flow_lpm": 500.0, "manufacturer": "Example Corp", "model": "P-500"}))]
    pub properties: Option<HashMap<String, serde_json::Value>>,
}

/// Request to execute an action on an instance
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ActionRequest {
    /// Numeric action point ID encoded as a string (for example, "1").
    /// Also accepts "id" and "action_id" for backward compatibility
    #[serde(alias = "id", alias = "action_id")]
    #[schema(example = "1")]
    pub point_id: String,
    #[schema(example = 4500.0)]
    pub value: f64,
    /// Explicit confirmation for this high-risk device command.
    #[schema(example = true)]
    pub confirmed: bool,
}

// === Instance Result Responses ===

/// Instance operation result (create/update/delete)
/// Uses InstanceCore to eliminate field duplication
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstanceResult {
    /// Core instance fields (instance_id, instance_name, product_name, properties)
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub core: InstanceCore,

    /// Runtime status
    #[schema(example = "active")]
    pub runtime_status: String,

    /// Operation message
    #[schema(example = "Instance created successfully")]
    pub message: Option<String>,
}

impl From<(Instance, String, Option<String>)> for InstanceResult {
    fn from((instance, runtime_status, message): (Instance, String, Option<String>)) -> Self {
        Self {
            core: instance.core, // Move instead of clone
            runtime_status,
            message,
        }
    }
}

/// Instance detail response (complete information)
/// Uses Instance to eliminate field duplication
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstanceDetail {
    /// Complete instance configuration
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub instance: Instance,

    /// Runtime status
    #[schema(example = "active")]
    pub runtime_status: String,

    /// Point statistics (measurement_count, action_count, etc.)
    pub point_statistics: HashMap<String, usize>,
}

// === Runtime Point Structures (Product Point + Instance Routing) ===

/// Point routing configuration (instance-specific attribute)
///
/// This structure represents the routing configuration for an instance point.
/// It defines how the point is mapped to a channel point.
/// `channel_id`, `channel_type`, and `channel_point_id` form a unit - all null means unbound.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PointRouting {
    /// Channel ID (null if routing is unbound)
    #[schema(example = 1)]
    pub channel_id: Option<i32>,

    /// Channel type (four-remote type, null if routing is unbound)
    #[schema(example = "T")]
    pub channel_type: Option<String>,

    /// Channel point ID (null if routing is unbound)
    #[schema(example = 101)]
    pub channel_point_id: Option<u32>,

    /// Whether routing is enabled
    #[schema(example = true)]
    pub enabled: bool,

    /// Channel name (for display purposes)
    #[schema(example = "Packaging PLC #1")]
    pub channel_name: Option<String>,

    /// Channel point name (signal_name from the point table)
    #[schema(example = "Outlet_Pressure")]
    pub channel_point_name: Option<String>,
}

/// Runtime measurement point (Product template + Instance routing)
///
/// This is the runtime view of a measurement point, combining:
/// - Product template definition (measurement_id, name, unit, description)
/// - Instance-specific routing configuration (if configured)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstanceMeasurementPoint {
    /// Measurement ID
    #[schema(example = 101)]
    pub measurement_id: u32,

    /// Point name
    #[schema(example = "DC Voltage")]
    pub name: String,

    /// Unit of measurement
    #[schema(example = "V")]
    pub unit: Option<String>,

    /// Point description
    #[schema(example = "Direct current voltage")]
    pub description: Option<String>,

    /// Routing configuration (None if not configured)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<PointRouting>,
}

/// Runtime action point (Product template + Instance routing)
///
/// This is the runtime view of an action point, combining:
/// - Product template definition (action_id, name, unit, description)
/// - Instance-specific routing configuration (if configured)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstanceActionPoint {
    /// Action ID
    #[schema(example = 201)]
    pub action_id: u32,

    /// Action name
    #[schema(example = "Power Setpoint")]
    pub name: String,

    /// Unit for adjustment actions
    #[schema(example = "W")]
    pub unit: Option<String>,

    /// Point description
    #[schema(example = "Active power setpoint")]
    pub description: Option<String>,

    /// Routing configuration (None if not configured)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<PointRouting>,
}

/// Runtime property point (Product template + Instance value)
///
/// Properties are static instance metadata (rated power, manufacturer, etc.) — they do
/// **not** carry routing because they are not part of the device data flow. The template
/// (`property_id`, `name`, `unit`, `description`) comes from the product, and `value`
/// is the per-instance value stored in `instances.properties` (keyed by `name`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstancePropertyPoint {
    /// Property ID (from product template)
    #[schema(example = 1)]
    pub property_id: i32,

    /// Property name (used as key in instances.properties JSON)
    #[schema(example = "rated_power")]
    pub name: String,

    /// Unit of the property
    #[schema(example = "kW")]
    pub unit: Option<String>,

    /// Property description
    #[schema(example = "Rated active power")]
    pub description: Option<String>,

    /// Current value from instance.properties (None if not configured for this instance)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>, example = json!(5000.0))]
    pub value: Option<serde_json::Value>,
}

/// Response for GET /api/instances/{name}/points
///
/// Returns all measurement, action, and property points for an instance.
/// Measurements and actions include their routing configurations; properties carry
/// the per-instance value (no routing — they are static metadata, not data-flow points).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstancePointsResponse {
    /// Instance name
    #[schema(example = "pump_01")]
    pub instance_name: String,

    /// Measurement points with routing
    pub measurements: Vec<InstanceMeasurementPoint>,

    /// Action points with routing
    pub actions: Vec<InstanceActionPoint>,

    /// Property points with current values (no routing)
    pub properties: Vec<InstancePropertyPoint>,
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    /// Test null values are deserialized as None
    #[test]
    fn test_routing_request_with_null_values() {
        let json = r#"{"point_type": "M", "point_id": 3, "channel_id": null, "channel_point_id": null, "four_remote": null}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.point_type, PointType::Measurement);
        assert!(req.channel_id.is_none());
        assert!(req.four_remote.is_none());
        assert!(req.channel_point_id.is_none());
        assert_eq!(req.point_id, 3);
    }

    /// Test empty strings are deserialized as None
    #[test]
    fn test_routing_request_with_empty_strings() {
        let json = r#"{"point_type": "A", "point_id": 3, "channel_id": "", "channel_point_id": "", "four_remote": ""}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.point_type, PointType::Action);
        assert!(req.channel_id.is_none());
        assert!(req.four_remote.is_none());
        assert!(req.channel_point_id.is_none());
        assert_eq!(req.point_id, 3);
    }

    /// Test omitted optional fields default to None (requires #[serde(default)])
    #[test]
    fn test_routing_request_with_omitted_fields() {
        let json = r#"{"point_type": "M", "point_id": 3}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.point_type, PointType::Measurement);
        assert!(req.channel_id.is_none());
        assert!(req.four_remote.is_none());
        assert!(req.channel_point_id.is_none());
        assert_eq!(req.point_id, 3);
    }

    /// Test valid values are deserialized correctly
    #[test]
    fn test_routing_request_with_valid_values() {
        let json = r#"{"point_type": "M", "point_id": 3, "channel_id": 1, "channel_point_id": 101, "four_remote": "T"}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.point_type, PointType::Measurement);
        assert_eq!(req.channel_id, Some(1));
        assert_eq!(req.four_remote, Some(FourRemote::Telemetry));
        assert_eq!(req.channel_point_id, Some(101));
        assert_eq!(req.point_id, 3);
    }

    /// Test mixed null and empty string (original failing scenario)
    #[test]
    fn test_routing_request_mixed_null_and_empty() {
        let json = r#"{"point_type": "A", "point_id": 3, "channel_id": null, "channel_point_id": null, "four_remote": ""}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.point_type, PointType::Action);
        assert!(req.channel_id.is_none());
        assert!(req.four_remote.is_none());
        assert!(req.channel_point_id.is_none());
        assert_eq!(req.point_id, 3);
    }

    /// Test string numbers are parsed correctly ("123" → 123)
    #[test]
    fn test_routing_request_string_numbers() {
        let json = r#"{"point_type": "M", "point_id": 3, "channel_id": "1", "channel_point_id": "101", "four_remote": "T"}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.point_type, PointType::Measurement);
        assert_eq!(req.channel_id, Some(1));
        assert_eq!(req.channel_point_id, Some(101));
        assert_eq!(req.four_remote, Some(FourRemote::Telemetry));
    }

    /// Test all FourRemote variants with standard aliases
    #[test]
    fn test_routing_request_four_remote_variants() {
        // Telemetry (T, YC, telemetry, yc)
        let json = r#"{"point_type": "M", "point_id": 1, "four_remote": "T"}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.four_remote, Some(FourRemote::Telemetry));

        // Signal (S, YX, signal, yx)
        let json = r#"{"point_type": "M", "point_id": 1, "four_remote": "S"}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.four_remote, Some(FourRemote::Signal));

        // Control (C, YK, control, yk)
        let json = r#"{"point_type": "A", "point_id": 1, "four_remote": "C"}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.four_remote, Some(FourRemote::Control));

        // Adjustment (A, YT, adjustment, setpoint, yt)
        let json = r#"{"point_type": "A", "point_id": 1, "four_remote": "A"}"#;
        let req: RoutingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.four_remote, Some(FourRemote::Adjustment));
    }

    /// Test point_type serialization and deserialization
    #[test]
    fn test_point_type_serde() {
        // Test Measurement
        assert_eq!(
            serde_json::to_string(&PointType::Measurement).unwrap(),
            "\"M\""
        );
        assert_eq!(
            serde_json::from_str::<PointType>("\"M\"").unwrap(),
            PointType::Measurement
        );

        // Test Action
        assert_eq!(serde_json::to_string(&PointType::Action).unwrap(), "\"A\"");
        assert_eq!(
            serde_json::from_str::<PointType>("\"A\"").unwrap(),
            PointType::Action
        );
    }
}
