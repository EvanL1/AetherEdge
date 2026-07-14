//! HTTP adaptation for the governed action-routing application command.

use aether_application::ActionRoutingMutationAcceptance;
use aether_domain::{
    ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind, TimestampMs,
};
use aether_ports::{
    ActionRoute, ActionRouteKey, LogicalRoutingRevision, RevisionedActionRoutingMutation,
};
use axum::http::HeaderMap;
use common::FourRemote;
use serde_json::{Value, json};

use crate::app_state::AppState;
use crate::dto::{ActionRoutingFourRemote, ActionRoutingUpsertBody, RoutingRequest};
use crate::error::AutomationError;

/// Converts a single-point HTTP payload into a typed action-routing mutation.
pub fn single_point_mutation(
    instance_id: u32,
    action_id: u32,
    request: &ActionRoutingUpsertBody,
) -> Result<RevisionedActionRoutingMutation, AutomationError> {
    let expected_revision = revision(request.expected_revision)?;
    let destination_kind = request.four_remote.map(|four_remote| match four_remote {
        ActionRoutingFourRemote::Control => PointKind::Command,
        ActionRoutingFourRemote::Adjustment => PointKind::Action,
    });
    mutation_from_parts(
        instance_id,
        action_id,
        request.channel_id,
        destination_kind,
        request.channel_point_id,
        request.enabled,
        expected_revision,
    )
}

/// Converts the legacy generic routing payload without permitting it to bypass
/// the application command.
pub fn generic_action_mutation(
    instance_id: u32,
    request: &RoutingRequest,
) -> Result<RevisionedActionRoutingMutation, AutomationError> {
    let expected_revision = revision(request.expected_revision)?;
    let destination_kind = match request.four_remote {
        Some(FourRemote::Control) => Some(PointKind::Command),
        Some(FourRemote::Adjustment) => Some(PointKind::Action),
        Some(FourRemote::Telemetry | FourRemote::Signal) => {
            return Err(AutomationError::InvalidRouting(
                "action routes may target only C or A channel points".to_string(),
            ));
        },
        None => None,
    };
    mutation_from_parts(
        instance_id,
        request.point_id,
        request.channel_id,
        destination_kind,
        request.channel_point_id,
        true,
        expected_revision,
    )
}

fn mutation_from_parts(
    instance_id: u32,
    action_id: u32,
    channel_id: Option<i32>,
    destination_kind: Option<PointKind>,
    channel_point_id: Option<u32>,
    enabled: bool,
    expected_revision: LogicalRoutingRevision,
) -> Result<RevisionedActionRoutingMutation, AutomationError> {
    let key = ActionRouteKey::new(InstanceId::new(instance_id), PointId::new(action_id));
    match (channel_id, destination_kind, channel_point_id) {
        (None, None, None) => Ok(RevisionedActionRoutingMutation::delete(
            key,
            expected_revision,
        )),
        (Some(channel_id), Some(point_kind), Some(channel_point_id)) => {
            let channel_id = u32::try_from(channel_id).map_err(|_| {
                AutomationError::InvalidRouting("channel_id must be non-negative".to_string())
            })?;
            let destination = ChannelCommandAddress::new(
                ChannelId::new(channel_id),
                point_kind,
                PointId::new(channel_point_id),
            )
            .map_err(|error| AutomationError::InvalidRouting(error.to_string()))?;
            Ok(RevisionedActionRoutingMutation::upsert(
                ActionRoute::new(key, destination, enabled),
                expected_revision,
            ))
        }
        _ => Err(AutomationError::InvalidRouting(
            "channel_id, four_remote, and channel_point_id must be supplied together or all omitted"
                .to_string(),
        )),
    }
}

fn revision(value: u64) -> Result<LogicalRoutingRevision, AutomationError> {
    crate::api::measurement_routing_boundary::revision(value)
}

/// Applies one authenticated action-routing command.
pub async fn apply(
    state: &AppState,
    headers: &HeaderMap,
    confirmed: bool,
    mutation: RevisionedActionRoutingMutation,
) -> Result<ActionRoutingMutationAcceptance, AutomationError> {
    let timestamp = TimestampMs::new(chrono::Utc::now().timestamp_millis().max(0) as u64);
    let invocation = crate::infra::application_control::command_invocation_from_headers(
        &state.control_authenticator,
        headers,
        confirmed,
        timestamp,
    );
    let acceptance = state
        .action_routing_application
        .mutate_revisioned(invocation.context(), mutation)
        .await
        .map_err(|error| match &error {
            aether_application::ApplicationError::Port(port_error)
                if port_error.kind() == aether_ports::PortErrorKind::Conflict =>
            {
                AutomationError::RoutingConflict(port_error.to_string())
            },
            _ => AutomationError::from(error),
        })?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        tracing::error!(
            request_id = acceptance.request_id(),
            operation = acceptance.kind().as_str(),
            error = %failure,
            "action-routing mutation completed but terminal audit is incomplete; do not retry"
        );
    }
    if let Some(failure) = acceptance.runtime_status().failure() {
        tracing::error!(
            request_id = acceptance.request_id(),
            operation = acceptance.kind().as_str(),
            error = %failure,
            "action-routing mutation committed but runtime publication failed; commands remain revoked and the request must not be retried"
        );
    }
    Ok(acceptance)
}

/// Stable HTTP representation of an accepted action-routing command.
pub fn response_data(
    acceptance: &ActionRoutingMutationAcceptance,
    message: impl Into<String>,
) -> Value {
    let runtime = acceptance.runtime_status();
    json!({
        "message": message.into(),
        "request_id": acceptance.request_id(),
        "operation": acceptance.kind().as_str(),
        "affected_routes": acceptance.affected_routes(),
        "resulting_revision": acceptance.resulting_revision().get(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "runtime": {
            "status": runtime.as_str(),
            "reconciliation_required": runtime.reconciliation_required(),
            "message": runtime.failure().map(|_| {
                "command routing is disabled until topology reconciliation succeeds"
            })
        },
        "retryable": acceptance.is_retryable()
    })
}
