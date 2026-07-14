//! HTTP adaptation for governed, revision-fenced measurement routing.

use aether_application::MeasurementRoutingMutationAcceptance;
use aether_domain::{ChannelId, ChannelPointAddress, InstanceId, PointId, PointKind, TimestampMs};
use aether_ports::{
    LogicalRoutingRevision, MeasurementRoute, MeasurementRouteKey, MeasurementRoutingMutation,
};
use axum::http::HeaderMap;
use common::FourRemote;
use serde_json::{Value, json};

use crate::app_state::AppState;
use crate::dto::{MeasurementRoutingUpsertRequest, RoutingRequest};
use crate::error::AutomationError;

/// Converts a single-point payload into a typed acquisition-owned route.
pub fn upsert_mutation(
    instance_id: u32,
    measurement_id: u32,
    request: &MeasurementRoutingUpsertRequest,
) -> Result<MeasurementRoutingMutation, AutomationError> {
    let channel_id = u32::try_from(request.channel_id).map_err(|_| {
        AutomationError::InvalidRouting("channel_id must be non-negative".to_string())
    })?;
    let kind = match request.four_remote {
        FourRemote::Telemetry => PointKind::Telemetry,
        FourRemote::Signal => PointKind::Status,
        FourRemote::Control | FourRemote::Adjustment => {
            return Err(AutomationError::InvalidRouting(
                "measurement routes may target only T or S channel points".to_string(),
            ));
        },
    };
    let key = MeasurementRouteKey::new(InstanceId::new(instance_id), PointId::new(measurement_id));
    let destination = ChannelPointAddress::new(
        ChannelId::new(channel_id),
        kind,
        PointId::new(request.channel_point_id),
    )
    .map_err(|error| AutomationError::InvalidRouting(error.to_string()))?;
    Ok(MeasurementRoutingMutation::upsert(
        MeasurementRoute::new(key, destination, request.enabled),
        revision(request.expected_revision)?,
    ))
}

/// Converts the legacy generic measurement payload without bypassing governance.
pub fn generic_mutation(
    instance_id: u32,
    request: &RoutingRequest,
) -> Result<MeasurementRoutingMutation, AutomationError> {
    let key =
        MeasurementRouteKey::new(InstanceId::new(instance_id), PointId::new(request.point_id));
    match (
        request.channel_id,
        request.four_remote,
        request.channel_point_id,
    ) {
        (None, None, None) => Ok(MeasurementRoutingMutation::delete(
            key,
            revision(request.expected_revision)?,
        )),
        (Some(channel_id), Some(four_remote), Some(channel_point_id)) => upsert_mutation(
            instance_id,
            request.point_id,
            &MeasurementRoutingUpsertRequest {
                channel_id,
                four_remote,
                channel_point_id,
                enabled: true,
                expected_revision: request.expected_revision,
                confirmed: request.confirmed,
            },
        ),
        _ => Err(AutomationError::InvalidRouting(
            "channel_id, four_remote, and channel_point_id must be supplied together or all omitted"
                .to_string(),
        )),
    }
}

/// Converts a wire revision to the mandatory positive CAS head.
pub fn revision(value: u64) -> Result<LogicalRoutingRevision, AutomationError> {
    if value == 0 || value >= i64::MAX as u64 {
        return Err(AutomationError::InvalidRouting(
            "expected_revision must be in 1..i64::MAX".to_string(),
        ));
    }
    Ok(LogicalRoutingRevision::new(value))
}

/// Applies one authenticated measurement-routing command.
pub async fn apply(
    state: &AppState,
    headers: &HeaderMap,
    confirmed: bool,
    mutation: MeasurementRoutingMutation,
) -> Result<MeasurementRoutingMutationAcceptance, AutomationError> {
    let timestamp = TimestampMs::new(chrono::Utc::now().timestamp_millis().max(0) as u64);
    let invocation = crate::infra::application_control::command_invocation_from_headers(
        &state.control_authenticator,
        headers,
        confirmed,
        timestamp,
    );
    let acceptance = state
        .measurement_routing_application
        .mutate(invocation.context(), mutation)
        .await?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        tracing::error!(
            request_id = acceptance.request_id(),
            error = %failure,
            "measurement-routing mutation completed but terminal audit is incomplete; do not retry"
        );
    }
    if let Some(failure) = acceptance.runtime_status().failure() {
        tracing::error!(
            request_id = acceptance.request_id(),
            error = %failure,
            "measurement-routing mutation committed but runtime publication failed"
        );
    }
    Ok(acceptance)
}

/// Stable HTTP representation of an accepted measurement-routing command.
pub fn response_data(
    acceptance: &MeasurementRoutingMutationAcceptance,
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
                "measurement routing is disabled until topology reconciliation succeeds"
            })
        },
        "retryable": acceptance.is_retryable()
    })
}
