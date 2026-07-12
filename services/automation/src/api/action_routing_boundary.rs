//! HTTP adaptation for the governed action-routing application command.

use aether_application::ActionRoutingMutationAcceptance;
use aether_domain::{
    ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind, TimestampMs,
};
use aether_ports::{ActionRoute, ActionRouteKey, ActionRoutingMutation};
use axum::http::HeaderMap;
use common::FourRemote;
use serde_json::{Value, json};

use crate::app_state::AppState;
use crate::dto::{RoutingRequest, SinglePointRoutingRequest};
use crate::error::AutomationError;

/// Converts a single-point HTTP payload into a typed action-routing mutation.
pub fn single_point_mutation(
    instance_id: u32,
    action_id: u32,
    request: &SinglePointRoutingRequest,
) -> Result<ActionRoutingMutation, AutomationError> {
    mutation_from_parts(
        instance_id,
        action_id,
        request.channel_id,
        request.four_remote,
        request.channel_point_id,
        request.enabled,
    )
}

/// Converts the legacy generic routing payload without permitting it to bypass
/// the application command.
pub fn generic_action_mutation(
    instance_id: u32,
    request: &RoutingRequest,
) -> Result<ActionRoutingMutation, AutomationError> {
    mutation_from_parts(
        instance_id,
        request.point_id,
        request.channel_id,
        request.four_remote,
        request.channel_point_id,
        true,
    )
}

fn mutation_from_parts(
    instance_id: u32,
    action_id: u32,
    channel_id: Option<i32>,
    four_remote: Option<FourRemote>,
    channel_point_id: Option<u32>,
    enabled: bool,
) -> Result<ActionRoutingMutation, AutomationError> {
    let key = ActionRouteKey::new(InstanceId::new(instance_id), PointId::new(action_id));
    match (channel_id, four_remote, channel_point_id) {
        (None, None, None) => Ok(ActionRoutingMutation::delete(key)),
        (Some(channel_id), Some(four_remote), Some(channel_point_id)) => {
            let channel_id = u32::try_from(channel_id).map_err(|_| {
                AutomationError::InvalidRouting("channel_id must be non-negative".to_string())
            })?;
            let point_kind = match four_remote {
                FourRemote::Control => PointKind::Command,
                FourRemote::Adjustment => PointKind::Action,
                FourRemote::Telemetry | FourRemote::Signal => {
                    return Err(AutomationError::InvalidRouting(
                        "action routes may target only C or A channel points".to_string(),
                    ));
                }
            };
            let destination = ChannelCommandAddress::new(
                ChannelId::new(channel_id),
                point_kind,
                PointId::new(channel_point_id),
            )
            .map_err(|error| AutomationError::InvalidRouting(error.to_string()))?;
            Ok(ActionRoutingMutation::upsert(ActionRoute::new(
                key,
                destination,
                enabled,
            )))
        }
        _ => Err(AutomationError::InvalidRouting(
            "channel_id, four_remote, and channel_point_id must be supplied together or all omitted"
                .to_string(),
        )),
    }
}

/// Applies one authenticated action-routing command.
pub async fn apply(
    state: &AppState,
    headers: &HeaderMap,
    confirmed: bool,
    mutation: ActionRoutingMutation,
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
        .mutate(invocation.context(), mutation)
        .await?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        tracing::error!(
            request_id = acceptance.request_id(),
            operation = acceptance.kind().as_str(),
            error = %failure,
            "action-routing mutation completed but terminal audit is incomplete; do not retry"
        );
    }
    Ok(acceptance)
}

/// Stable HTTP representation of an accepted action-routing command.
pub fn response_data(
    acceptance: &ActionRoutingMutationAcceptance,
    message: impl Into<String>,
) -> Value {
    json!({
        "message": message.into(),
        "request_id": acceptance.request_id(),
        "operation": acceptance.kind().as_str(),
        "affected_routes": acceptance.affected_routes(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "retryable": acceptance.is_retryable()
    })
}
