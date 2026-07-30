//! Data Transfer Objects for Model Service API
//!
//! This module contains all request and response structures used by the API endpoints.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use common::FourRemote;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

// === Query Parameters ===

/// Supported live-data planes for an instance query.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InstanceDataType {
    Measurement,
    Action,
}

impl From<InstanceDataType> for crate::instance_query::InstanceDataPlane {
    fn from(value: InstanceDataType) -> Self {
        match value {
            InstanceDataType::Measurement => Self::Measurement,
            InstanceDataType::Action => Self::Action,
        }
    }
}

/// Query parameter for filtering by live-data plane.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DataTypeQuery {
    #[serde(rename = "type")]
    pub data_type: Option<InstanceDataType>,
}

// === Routing Management ===

/// Governed measurement-route upsert with a mandatory shared CAS revision.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct MeasurementRoutingUpsertRequest {
    #[schema(example = 1)]
    pub channel_id: i32,
    #[schema(value_type = String, example = "T")]
    pub four_remote: FourRemote,
    #[schema(example = 101)]
    pub channel_point_id: u32,
    #[serde(default = "default_enabled")]
    #[schema(example = true)]
    pub enabled: bool,
    /// Current `logical_routing` revision returned by the previous command/query.
    #[schema(example = 7)]
    pub expected_revision: u64,
    /// Explicit confirmation for the high-risk logical topology change.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Governed measurement-route enable/disable command.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct MeasurementRoutingToggleRequest {
    #[schema(example = true)]
    pub enabled: bool,
    #[schema(example = 7)]
    pub expected_revision: u64,
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Governed measurement-route deletion command.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct MeasurementRoutingDeleteRequest {
    #[schema(example = 7)]
    pub expected_revision: u64,
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Physical channel address nested in an instance-scoped routing response.
#[derive(Debug, Serialize, ToSchema)]
pub struct RoutingChannelDto {
    pub id: u32,
    pub four_remote: String,
    pub point_id: u32,
}

/// One instance-scoped logical route.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceRoutingEntryDto {
    pub channel: RoutingChannelDto,
    pub point_id: u32,
    pub enabled: bool,
}

/// Complete instance routing view and its shared CAS head.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceRoutingResponseDto {
    pub instance_id: u32,
    pub measurement: Vec<InstanceRoutingEntryDto>,
    pub action: Vec<InstanceRoutingEntryDto>,
    pub logical_routing_revision: u64,
}

/// Flat routing row used by global and channel-scoped query responses.
#[derive(Debug, Serialize, ToSchema)]
pub struct RoutingListEntryDto {
    pub routing_id: i64,
    pub instance_id: u32,
    pub instance_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<u32>,
    pub channel_type: String,
    pub channel_point_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_point_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_point_id: Option<u32>,
    pub enabled: bool,
}

impl RoutingListEntryDto {
    pub(crate) fn measurement(
        route: crate::routing_loader::RoutingRoute,
        include_channel: bool,
    ) -> Self {
        Self::new(route, include_channel, true)
    }

    pub(crate) fn action(
        route: crate::routing_loader::RoutingRoute,
        include_channel: bool,
    ) -> Self {
        Self::new(route, include_channel, false)
    }

    fn new(
        route: crate::routing_loader::RoutingRoute,
        include_channel: bool,
        measurement: bool,
    ) -> Self {
        Self {
            routing_id: route.routing_id,
            instance_id: route.instance_id,
            instance_name: route.instance_name,
            channel_id: include_channel.then_some(route.channel_id),
            channel_type: route.channel_type,
            channel_point_id: route.channel_point_id,
            measurement_point_id: measurement.then_some(route.point_id),
            action_point_id: (!measurement).then_some(route.point_id),
            enabled: route.enabled,
        }
    }
}

impl From<crate::routing_loader::RoutingRoute> for InstanceRoutingEntryDto {
    fn from(route: crate::routing_loader::RoutingRoute) -> Self {
        Self {
            channel: RoutingChannelDto {
                id: route.channel_id,
                four_remote: route.channel_type,
                point_id: route.channel_point_id,
            },
            point_id: route.point_id,
            enabled: route.enabled,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RoutingTotalsDto {
    pub measurement: usize,
    pub action: usize,
}

/// Complete desired-routing query response.
#[derive(Debug, Serialize, ToSchema)]
pub struct AllRoutingResponseDto {
    pub measurement_routing: Vec<RoutingListEntryDto>,
    pub action_routing: Vec<RoutingListEntryDto>,
    pub total: RoutingTotalsDto,
    pub logical_routing_revision: u64,
}

/// Desired routes that touch one physical channel.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChannelRoutingResponseDto {
    pub channel_id: u32,
    pub uplink: Vec<RoutingListEntryDto>,
    pub downlink: Vec<RoutingListEntryDto>,
    pub logical_routing_revision: u64,
}

/// Channel point kinds that are valid destinations for an action route.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
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
/// Measurement routing supports T/S destinations, while this command accepts
/// only C/A destinations and always requires explicit confirmation.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ActionRoutingUpsertBody {
    #[schema(example = 1)]
    pub channel_id: Option<i32>,
    pub four_remote: Option<ActionRoutingFourRemote>,
    #[schema(example = 201)]
    pub channel_point_id: Option<u32>,
    #[schema(default = true, example = true)]
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Current `logical_routing` revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,
    /// Required explicit confirmation for the physical command-topology change.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Swagger request body for a governed action-route enable/disable command.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ActionRoutingToggleBody {
    #[schema(example = true)]
    pub enabled: bool,
    /// Current `logical_routing` revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,
    /// Required explicit confirmation for the physical command-topology change.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Swagger request body for a governed action-route deletion command.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ActionRoutingConfirmationBody {
    /// Current `logical_routing` revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,
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

/// Runtime command-routing projection after the durable mutation commits.
#[derive(Debug, Clone, ToSchema)]
pub struct ActionRoutingRuntimeState {
    /// `published`, or `commands_revoked` when fail-closed reconciliation is required.
    #[schema(example = "published")]
    pub status: String,
    /// True when the runtime must be rebuilt from the committed SQLite view.
    #[schema(example = false)]
    pub reconciliation_required: bool,
    /// Present when commands were revoked after publication failed.
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
    /// Authoritative shared logical-routing revision after commit.
    #[schema(example = 8)]
    pub resulting_revision: u64,
    pub audit: ActionRoutingAuditState,
    pub runtime: ActionRoutingRuntimeState,
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
    /// Current `instances` aggregate revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,
    /// Explicit confirmation for the desired-state mutation.
    #[schema(example = true)]
    pub confirmed: bool,
}

/// Query/body fence for destructive instance-configuration mutations.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
pub struct InstanceMutationConfirmation {
    /// Current `instances` aggregate revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,
    /// Explicit confirmation for the desired-state mutation.
    #[schema(example = true)]
    pub confirmed: bool,
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
    /// Current `instances` aggregate revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,
    /// Explicit confirmation for commissioning this instance.
    #[schema(example = true)]
    pub confirmed: bool,
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

    /// Current `instances` aggregate revision required for compare-and-set.
    #[schema(example = 7)]
    pub expected_revision: u64,

    /// Explicit confirmation for changing desired state.
    #[schema(example = true)]
    pub confirmed: bool,
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

// === Runtime Point Structures (Product Point + Instance Routing) ===

/// Point routing configuration (instance-specific attribute)
///
/// This structure represents the routing configuration for an instance point.
/// It defines how the point is mapped to a channel point.
/// `channel_id`, `channel_type`, and `channel_point_id` form a unit - all null means unbound.
#[derive(Debug, Serialize, ToSchema)]
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
#[derive(Debug, Serialize, ToSchema)]
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
#[derive(Debug, Serialize, ToSchema)]
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
#[derive(Debug, Serialize, ToSchema)]
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
#[derive(Debug, Serialize, ToSchema)]
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

    /// Shared CAS head for every measurement/action route in this response.
    pub logical_routing_revision: u64,
}

impl From<crate::instance_query::PointRoutingView> for PointRouting {
    fn from(view: crate::instance_query::PointRoutingView) -> Self {
        Self {
            channel_id: view.channel_id,
            channel_type: view.channel_type,
            channel_point_id: view.channel_point_id,
            enabled: view.enabled,
            channel_name: view.channel_name,
            channel_point_name: view.channel_point_name,
        }
    }
}

impl From<crate::instance_query::InstanceMeasurementPointView> for InstanceMeasurementPoint {
    fn from(view: crate::instance_query::InstanceMeasurementPointView) -> Self {
        Self {
            measurement_id: view.measurement_id,
            name: view.name,
            unit: view.unit,
            description: view.description,
            routing: view.routing.map(PointRouting::from),
        }
    }
}

impl From<crate::instance_query::InstanceActionPointView> for InstanceActionPoint {
    fn from(view: crate::instance_query::InstanceActionPointView) -> Self {
        Self {
            action_id: view.action_id,
            name: view.name,
            unit: view.unit,
            description: view.description,
            routing: view.routing.map(PointRouting::from),
        }
    }
}

impl From<crate::instance_query::InstancePropertyView> for InstancePropertyPoint {
    fn from(view: crate::instance_query::InstancePropertyView) -> Self {
        Self {
            property_id: view.property_id,
            name: view.name,
            unit: view.unit,
            description: view.description,
            value: view.value,
        }
    }
}

impl From<crate::instance_query::InstancePointsView> for InstancePointsResponse {
    fn from(view: crate::instance_query::InstancePointsView) -> Self {
        Self {
            instance_name: view.instance_name,
            measurements: view
                .measurements
                .into_iter()
                .map(InstanceMeasurementPoint::from)
                .collect(),
            actions: view
                .actions
                .into_iter()
                .map(InstanceActionPoint::from)
                .collect(),
            properties: view
                .properties
                .into_iter()
                .map(InstancePropertyPoint::from)
                .collect(),
            logical_routing_revision: view.logical_routing_revision,
        }
    }
}

/// Lightweight commissioned-instance summary.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceSummaryDto {
    pub instance_id: u32,
    pub instance_name: String,
    pub product_name: String,
    pub parent_id: Option<u32>,
    pub properties: HashMap<String, serde_json::Value>,
}

impl From<crate::config::Instance> for InstanceSummaryDto {
    fn from(instance: crate::config::Instance) -> Self {
        Self {
            instance_id: instance.core.instance_id,
            instance_name: instance.core.instance_name,
            product_name: instance.core.product_name,
            parent_id: instance.core.parent_id,
            properties: instance.core.properties,
        }
    }
}

/// Bounded result for `GET /api/instances/search`.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceSearchResponseDto {
    pub count: usize,
    pub limit: u32,
    pub list: Vec<InstanceSummaryDto>,
}

/// Paginated commissioned-instance list.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceListResponseDto {
    pub total: u32,
    pub page: u32,
    pub page_size: u32,
    pub list: Vec<InstanceSummaryDto>,
}

/// Minimal identity for instance pickers.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstancePickerItemDto {
    pub id: u32,
    pub name: String,
}

/// Minimal unpaginated instance-picker list.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstancePickerResponseDto {
    pub list: Vec<InstancePickerItemDto>,
}

/// Detail response for one commissioned instance.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceDetailResponseDto {
    pub instance: InstanceSummaryDto,
}

/// One live value read from the authoritative SHM generation.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceLiveSampleDto {
    pub value: f64,
    pub timestamp_ms: u64,
}

/// Complete live-data view when no plane filter is supplied.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceLiveDataDto {
    pub measurements: std::collections::BTreeMap<String, InstanceLiveSampleDto>,
    pub actions: std::collections::BTreeMap<String, InstanceLiveSampleDto>,
}

/// Live-data response: one filtered value map or the complete two-plane view.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum InstanceDataResponseDto {
    Values(std::collections::BTreeMap<String, InstanceLiveSampleDto>),
    Complete(InstanceLiveDataDto),
}

fn live_values(
    values: std::collections::BTreeMap<u32, crate::instance_query::InstanceLiveSample>,
) -> std::collections::BTreeMap<String, InstanceLiveSampleDto> {
    values
        .into_iter()
        .map(|(point_id, sample)| {
            (
                point_id.to_string(),
                InstanceLiveSampleDto {
                    value: sample.value,
                    timestamp_ms: sample.timestamp_ms,
                },
            )
        })
        .collect()
}

impl From<crate::instance_query::InstanceLiveDataView> for InstanceDataResponseDto {
    fn from(view: crate::instance_query::InstanceLiveDataView) -> Self {
        match view {
            crate::instance_query::InstanceLiveDataView::Values(values) => {
                Self::Values(live_values(values))
            },
            crate::instance_query::InstanceLiveDataView::Complete {
                measurements,
                actions,
            } => Self::Complete(InstanceLiveDataDto {
                measurements: live_values(measurements),
                actions: live_values(actions),
            }),
        }
    }
}
