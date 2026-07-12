//! Governed I/O channel-management HTTP boundary.
//!
//! Channel mutation and runtime-reconciliation routes translate HTTP into the
//! shared application facades. Durable SQLite and runtime projection work
//! belongs to port adapters, never to these handlers.

mod reload;

pub use reload::*;

use std::{collections::BTreeMap, sync::Arc};

use aether_application::{
    ApplicationError, ChannelManagementApplication, ChannelMutationAcceptance,
    ChannelReconciliationAcceptance, ChannelReconciliationApplication, CompletionAuditStatus,
};
use aether_auth_jwt::AccessTokenAuthenticator;
use aether_domain::{ChannelId, TimestampMs};
use aether_ports::{
    ChannelDefinition, ChannelLoggingPolicy, ChannelMutation, ChannelMutationKind,
    ChannelParameterValue, ChannelParameters, ChannelPatch, ChannelReconciliationScope,
    ChannelRevision, ChannelRuntimeProjection, PortErrorKind,
};
use axum::{
    Extension,
    extract::{Path, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    response::Json,
};
use serde_json::Value;

use crate::dto::{
    AppError, ChannelCompletionAudit, ChannelCompletionAuditState, ChannelConfigUpdateRequest,
    ChannelCreateRequest, ChannelEnabledRequest, ChannelMutationOperation, ChannelMutationResponse,
    ChannelMutationResult, ChannelRuntimeProjectionResult, ErrorInfo,
};

const CONFIRMATION_HEADER: &str = "x-aether-confirmed";
const REQUEST_ID_HEADER: &str = "x-request-id";
const EXPECTED_REVISION_HEADER: &str = "x-aether-expected-revision";

/// HTTP-owned references needed to invoke the channel application command.
///
/// The unavailable form is used by the legacy route factory and fails closed.
/// Production composition must construct the governed form explicitly.
#[derive(Clone)]
pub struct ChannelManagementHttpBoundary {
    inner: Option<GovernedChannelManagement>,
}

#[derive(Clone)]
struct GovernedChannelManagement {
    application: Arc<ChannelManagementApplication>,
    reconciliation: Option<Arc<ChannelReconciliationApplication>>,
    access_authenticator: Arc<AccessTokenAuthenticator>,
}

impl ChannelManagementHttpBoundary {
    /// Creates the production HTTP boundary over the shared application API.
    #[must_use]
    pub fn governed(
        application: Arc<ChannelManagementApplication>,
        access_authenticator: Arc<AccessTokenAuthenticator>,
    ) -> Self {
        Self {
            inner: Some(GovernedChannelManagement {
                application,
                reconciliation: None,
                access_authenticator,
            }),
        }
    }

    /// Creates a production boundary over both desired-state mutation and
    /// runtime reconciliation application commands.
    #[must_use]
    pub fn governed_with_reconciliation(
        application: Arc<ChannelManagementApplication>,
        reconciliation: Arc<ChannelReconciliationApplication>,
        access_authenticator: Arc<AccessTokenAuthenticator>,
    ) -> Self {
        Self {
            inner: Some(GovernedChannelManagement {
                application,
                reconciliation: Some(reconciliation),
                access_authenticator,
            }),
        }
    }

    /// Creates a fail-closed compatibility boundary.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self { inner: None }
    }

    pub(super) async fn mutate(
        &self,
        headers: &HeaderMap,
        mutation: ChannelMutation,
    ) -> Result<ChannelMutationAcceptance, AppError> {
        let governed = self.inner.as_ref().ok_or_else(|| {
            http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Channel-management application boundary is unavailable",
            )
        })?;
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        let request_id = headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok());
        let confirmed = headers
            .get(CONFIRMATION_HEADER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
        let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let invocation = governed.access_authenticator.invocation(
            authorization,
            request_id,
            confirmed,
            TimestampMs::new(timestamp),
        );

        governed
            .application
            .mutate(invocation.context(), mutation)
            .await
            .map_err(application_error)
    }

    pub(super) async fn reconcile(
        &self,
        headers: &HeaderMap,
        scope: ChannelReconciliationScope,
    ) -> Result<ChannelReconciliationAcceptance, AppError> {
        let governed = self.inner.as_ref().ok_or_else(|| {
            http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Channel-reconciliation application boundary is unavailable",
            )
        })?;
        let application = governed.reconciliation.as_ref().ok_or_else(|| {
            http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Channel-reconciliation application boundary is unavailable",
            )
        })?;
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        let request_id = required_request_id(headers)?;
        let confirmed = headers
            .get(CONFIRMATION_HEADER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
        let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let invocation = governed.access_authenticator.invocation(
            authorization,
            Some(request_id),
            confirmed,
            TimestampMs::new(timestamp),
        );

        application
            .reconcile(invocation.context(), scope)
            .await
            .map_err(reconciliation_application_error)
    }
}

/// Create one commissioned channel. The desired enabled state defaults to
/// false, so no protocol runtime starts unless the caller explicitly opts in.
///
/// `x-aether-expected-revision` is rejected for creation because no prior
/// revision exists; Swagger only exposes it on existing-resource mutations.
#[utoipa::path(
    post,
    path = "/api/channels",
    params(
        ("x-request-id" = Option<String>, Header, format = "uuid", description = "Optional UUID audit correlation ID; a UUID is generated when omitted or invalid"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    request_body(
        content = ChannelCreateRequest,
        description = "Complete channel definition. enabled defaults to false.",
        examples(
            ("Disabled commissioning" = (
                summary = "Persist desired configuration without starting a protocol runtime",
                value = json!({
                    "channel_id": 12,
                    "name": "Packaging controller",
                    "description": "Primary line controller",
                    "protocol": "modbus_tcp",
                    "enabled": false,
                    "parameters": {"host": "192.0.2.10", "port": 502}
                })
            ))
        )
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent desired-state mutation. A pending or degraded runtime projection is still accepted and must not be retried automatically; retryable=false. An incomplete completion audit is reported with request_id for operator reconciliation; do not retry automatically.", body = ChannelMutationResponse),
        (status = 400, description = "Malformed DTO/header or invalid channel definition", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 409, description = "Channel ID or name already exists", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or channel adapter is unavailable; desired state was not committed", body = common::ErrorResponse),
        (status = 504, description = "Channel adapter timed out before desired state committed", body = common::ErrorResponse),
        (status = 500, description = "Permanent adapter failure before desired state committed", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn create_channel_handler(
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
    payload: Result<Json<ChannelCreateRequest>, JsonRejection>,
) -> Result<Json<ChannelMutationResponse>, AppError> {
    let request = json_body(payload)?;
    if expected_revision(&headers)?.is_some() {
        return Err(AppError::bad_request(
            "x-aether-expected-revision must be omitted when creating a channel",
        ));
    }

    let compatibility = ChannelResponseCompatibility {
        name: Some(request.name.clone()),
        description: request.description.clone(),
        protocol: Some(request.protocol.clone()),
    };
    let parameters = parameters_from_json(request.parameters)?;
    let mut definition = ChannelDefinition::new(
        request.channel_id.map(ChannelId::new),
        request.name,
        request.protocol,
        parameters,
    )
    .with_enabled(request.enabled.unwrap_or(false));
    if let Some(description) = request.description {
        definition = definition.with_description(description);
    }
    if let Some(logging) = request.logging {
        definition = definition.with_logging(logging_policy(logging));
    }

    let acceptance = boundary
        .mutate(&headers, ChannelMutation::create(definition))
        .await?;
    Ok(Json(mutation_response(&acceptance, compatibility)))
}

/// Partially update an existing channel definition using PATCH semantics on
/// the historical PUT path. Omitted or null fields remain unchanged;
/// supplied `parameters` keys merge into the authoritative map and omitted
/// keys remain unchanged. Full replacement or clearing requires a future
/// explicit mutation rather than overloading this compatibility route.
/// The desired enabled state is changed only through `/enabled`.
///
/// This retained PUT endpoint has PATCH semantics; identity migration is forbidden.
/// A body `channel_id` may echo the path ID for compatibility, but any different
/// value returns HTTP 400 before the application mutator can observe a side effect.
#[utoipa::path(
    put,
    path = "/api/channels/{id}",
    params(
        ("id" = u32, Path, description = "Stable channel identifier below 10000", maximum = 9999),
        ("x-request-id" = Option<String>, Header, format = "uuid", description = "Optional UUID audit correlation ID; a UUID is generated when omitted or invalid"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true"),
        ("x-aether-expected-revision" = Option<u64>, Header, description = "Optional desired-state revision compare-and-set guard in 1..9223372036854775807", minimum = 1, maximum = 9223372036854775807_i64)
    ),
    request_body(
        content = ChannelConfigUpdateRequest,
        description = "PATCH semantics on the retained PUT path. Supplied parameter keys merge and omitted keys remain unchanged. Identity migration is forbidden.",
        example = json!({
            "channel_id": 12,
            "name": "Packaging controller 2",
            "parameters": {"host": "192.0.2.11", "port": 502}
        })
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent desired-state mutation. A pending or degraded runtime projection is still accepted and must not be retried automatically; retryable=false. An incomplete completion audit is reported with request_id for operator reconciliation; do not retry automatically.", body = ChannelMutationResponse),
        (status = 400, description = "Malformed DTO/header, empty patch, invalid values, or forbidden channel ID migration", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 404, description = "Channel does not exist", body = common::ErrorResponse),
        (status = 409, description = "Revision is stale or the replacement name conflicts", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or channel adapter is unavailable; desired state was not committed", body = common::ErrorResponse),
        (status = 504, description = "Channel adapter timed out before desired state committed", body = common::ErrorResponse),
        (status = 500, description = "Permanent adapter failure before desired state committed", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn update_channel_handler(
    Path(id): Path<String>,
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
    payload: Result<Json<ChannelConfigUpdateRequest>, JsonRejection>,
) -> Result<Json<ChannelMutationResponse>, AppError> {
    let id = path_channel_id(&id)?;
    let request = json_body(payload)?;
    if request
        .channel_id
        .is_some_and(|requested_id| requested_id != id)
    {
        return Err(AppError::bad_request(
            "channel_id identity migration is forbidden on ordinary updates",
        ));
    }

    let compatibility = ChannelResponseCompatibility {
        name: request.name.clone(),
        description: request.description.clone(),
        protocol: request.protocol.clone(),
    };
    let revision = expected_revision(&headers)?;
    let patch = patch_from_request(request)?;
    let mutation = match revision {
        Some(revision) => {
            ChannelMutation::update_with_revision(ChannelId::new(id), revision, patch)
        },
        None => ChannelMutation::update(ChannelId::new(id), patch),
    };
    let acceptance = boundary.mutate(&headers, mutation).await?;
    Ok(Json(mutation_response(&acceptance, compatibility)))
}

/// Enable or disable one channel's desired runtime lifecycle state.
#[utoipa::path(
    put,
    path = "/api/channels/{id}/enabled",
    params(
        ("id" = u32, Path, description = "Stable channel identifier below 10000", maximum = 9999),
        ("x-request-id" = Option<String>, Header, format = "uuid", description = "Optional UUID audit correlation ID; a UUID is generated when omitted or invalid"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true"),
        ("x-aether-expected-revision" = Option<u64>, Header, description = "Optional desired-state revision compare-and-set guard in 1..9223372036854775807", minimum = 1, maximum = 9223372036854775807_i64)
    ),
    request_body(
        content = ChannelEnabledRequest,
        description = "Desired runtime lifecycle state",
        example = json!({"enabled": true})
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent desired-state mutation. A pending or degraded runtime projection is still accepted and must not be retried automatically; retryable=false. An incomplete completion audit is reported with request_id for operator reconciliation; do not retry automatically.", body = ChannelMutationResponse),
        (status = 400, description = "Malformed DTO/header or invalid channel identifier/revision", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 404, description = "Channel does not exist", body = common::ErrorResponse),
        (status = 409, description = "Revision is stale or lifecycle state is concurrently changing", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or channel adapter is unavailable; desired state was not committed", body = common::ErrorResponse),
        (status = 504, description = "Channel adapter timed out before desired state committed", body = common::ErrorResponse),
        (status = 500, description = "Permanent adapter failure before desired state committed", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn set_channel_enabled_handler(
    Path(id): Path<String>,
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
    payload: Result<Json<ChannelEnabledRequest>, JsonRejection>,
) -> Result<Json<ChannelMutationResponse>, AppError> {
    let id = path_channel_id(&id)?;
    let request = json_body(payload)?;
    let revision = expected_revision(&headers)?;
    let channel_id = ChannelId::new(id);
    let mutation = match (request.enabled, revision) {
        (true, Some(revision)) => ChannelMutation::enable_with_revision(channel_id, revision),
        (true, None) => ChannelMutation::enable(channel_id),
        (false, Some(revision)) => ChannelMutation::disable_with_revision(channel_id, revision),
        (false, None) => ChannelMutation::disable(channel_id),
    };
    let acceptance = boundary.mutate(&headers, mutation).await?;
    Ok(Json(mutation_response(
        &acceptance,
        ChannelResponseCompatibility::default(),
    )))
}

/// Delete one channel desired configuration and its rebuildable runtime.
///
/// Deletion returns HTTP 409 while an action route still references the
/// channel; the I/O boundary never cascades governed action-routing records.
#[utoipa::path(
    delete,
    path = "/api/channels/{id}",
    params(
        ("id" = u32, Path, description = "Stable channel identifier below 10000", maximum = 9999),
        ("x-request-id" = Option<String>, Header, format = "uuid", description = "Optional UUID audit correlation ID; a UUID is generated when omitted or invalid"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true"),
        ("x-aether-expected-revision" = Option<u64>, Header, description = "Optional desired-state revision compare-and-set guard in 1..9223372036854775807", minimum = 1, maximum = 9223372036854775807_i64)
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent desired-state mutation. A pending or degraded runtime projection is still accepted and must not be retried automatically; retryable=false. An incomplete completion audit is reported with request_id for operator reconciliation; do not retry automatically.", body = ChannelMutationResponse),
        (status = 400, description = "Malformed header or invalid channel identifier/revision", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 404, description = "Channel does not exist", body = common::ErrorResponse),
        (status = 409, description = "Revision is stale or an action route still references the channel", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or channel adapter is unavailable; desired state was not committed", body = common::ErrorResponse),
        (status = 504, description = "Channel adapter timed out before desired state committed", body = common::ErrorResponse),
        (status = 500, description = "Permanent adapter failure before desired state committed", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn delete_channel_handler(
    Path(id): Path<String>,
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<ChannelMutationResponse>, AppError> {
    let id = path_channel_id(&id)?;
    let revision = expected_revision(&headers)?;
    let channel_id = ChannelId::new(id);
    let mutation = revision.map_or_else(
        || ChannelMutation::delete(channel_id),
        |revision| ChannelMutation::delete_with_revision(channel_id, revision),
    );
    let acceptance = boundary.mutate(&headers, mutation).await?;
    Ok(Json(mutation_response(
        &acceptance,
        ChannelResponseCompatibility::default(),
    )))
}

fn expected_revision(headers: &HeaderMap) -> Result<Option<ChannelRevision>, AppError> {
    let Some(value) = headers.get(EXPECTED_REVISION_HEADER) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| {
        AppError::bad_request("x-aether-expected-revision must be an unsigned integer")
    })?;
    let revision = value.parse::<u64>().map_err(|_| {
        AppError::bad_request("x-aether-expected-revision must be an unsigned integer")
    })?;
    Ok(Some(ChannelRevision::new(revision)))
}

pub(super) fn path_channel_id(value: &str) -> Result<u32, AppError> {
    let channel_id = value
        .parse::<u32>()
        .map_err(|_| AppError::bad_request("Channel ID must be an unsigned integer"))?;
    if channel_id >= 10_000 {
        return Err(AppError::bad_request("Channel ID must be below 10000"));
    }
    Ok(channel_id)
}

pub(super) fn required_request_id(headers: &HeaderMap) -> Result<&str, AppError> {
    let request_id = headers
        .get(REQUEST_ID_HEADER)
        .ok_or_else(|| AppError::bad_request("x-request-id is required"))?
        .to_str()
        .map_err(|_| AppError::bad_request("x-request-id must be a UUID"))?;
    uuid::Uuid::parse_str(request_id)
        .map_err(|_| AppError::bad_request("x-request-id must be a UUID"))?;
    Ok(request_id)
}

fn json_body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    payload
        .map(|Json(value)| value)
        .map_err(|_| AppError::bad_request("Request body must be valid application/json"))
}

fn patch_from_request(request: ChannelConfigUpdateRequest) -> Result<ChannelPatch, AppError> {
    let mut patch = ChannelPatch::new();
    if let Some(name) = request.name {
        patch = patch.with_name(name);
    }
    if let Some(description) = request.description {
        patch = patch.with_description(description);
    }
    if let Some(protocol) = request.protocol {
        patch = patch.with_protocol(protocol);
    }
    if let Some(parameters) = request.parameters {
        patch = patch.with_parameters(parameters_from_json(parameters)?);
    }
    if let Some(logging) = request.logging {
        patch = patch.with_logging(logging_policy(logging));
    }
    Ok(patch)
}

fn logging_policy(logging: crate::dto::ChannelLoggingConfig) -> ChannelLoggingPolicy {
    let mut policy = ChannelLoggingPolicy::default().with_enabled(logging.enabled);
    if let Some(level) = logging.level {
        policy = policy.with_level(level);
    }
    if let Some(file) = logging.file {
        policy = policy.with_file(file);
    }
    policy
}

fn parameters_from_json(
    parameters: std::collections::HashMap<String, Value>,
) -> Result<ChannelParameters, AppError> {
    parameters
        .into_iter()
        .map(|(key, value)| parameter_from_json(value).map(|value| (key, value)))
        .collect::<Result<BTreeMap<_, _>, _>>()
}

fn parameter_from_json(value: Value) -> Result<ChannelParameterValue, AppError> {
    match value {
        Value::Null => Ok(ChannelParameterValue::Null),
        Value::Bool(value) => Ok(ChannelParameterValue::Bool(value)),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(ChannelParameterValue::Integer(value))
            } else if let Some(value) = value.as_u64() {
                i64::try_from(value)
                    .map(ChannelParameterValue::Integer)
                    .map_err(|_| AppError::bad_request("integer channel parameter exceeds i64"))
            } else {
                value.as_f64().map_or_else(
                    || Err(AppError::bad_request("invalid numeric channel parameter")),
                    |value| Ok(ChannelParameterValue::Float(value)),
                )
            }
        },
        Value::String(value) => Ok(ChannelParameterValue::String(value)),
        Value::Array(values) => values
            .into_iter()
            .map(parameter_from_json)
            .collect::<Result<Vec<_>, _>>()
            .map(ChannelParameterValue::Array),
        Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| parameter_from_json(value).map(|value| (key, value)))
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(ChannelParameterValue::Object),
    }
}

#[derive(Default)]
struct ChannelResponseCompatibility {
    name: Option<String>,
    description: Option<String>,
    protocol: Option<String>,
}

fn mutation_response(
    acceptance: &ChannelMutationAcceptance,
    compatibility: ChannelResponseCompatibility,
) -> ChannelMutationResponse {
    let operation = match acceptance.kind() {
        ChannelMutationKind::Create => ChannelMutationOperation::Create,
        ChannelMutationKind::Update => ChannelMutationOperation::Update,
        ChannelMutationKind::Delete => ChannelMutationOperation::Delete,
        ChannelMutationKind::Enable => ChannelMutationOperation::Enable,
        ChannelMutationKind::Disable => ChannelMutationOperation::Disable,
    };
    let runtime_projection = match acceptance.runtime_projection() {
        ChannelRuntimeProjection::Stopped => ChannelRuntimeProjectionResult::Stopped,
        ChannelRuntimeProjection::ActivationPending => {
            ChannelRuntimeProjectionResult::ActivationPending
        },
        ChannelRuntimeProjection::Active => ChannelRuntimeProjectionResult::Active,
        ChannelRuntimeProjection::Degraded => ChannelRuntimeProjectionResult::Degraded,
        ChannelRuntimeProjection::Removed => ChannelRuntimeProjectionResult::Removed,
    };
    let completion_audit = match acceptance.completion_audit() {
        CompletionAuditStatus::Recorded => ChannelCompletionAudit {
            status: ChannelCompletionAuditState::Recorded,
            retryable: false,
            message: None,
        },
        CompletionAuditStatus::Incomplete { failure } => {
            tracing::error!(
                request_id = acceptance.request_id(),
                channel_id = acceptance.channel_id().get(),
                error = %failure,
                "channel mutation was accepted but terminal audit is incomplete; do not retry"
            );
            ChannelCompletionAudit {
                status: ChannelCompletionAuditState::Incomplete,
                retryable: false,
                message: Some(
                    "operation was accepted but its terminal audit is incomplete; do not retry"
                        .to_string(),
                ),
            }
        },
    };
    let channel_id = acceptance.channel_id().get();
    let desired_enabled = acceptance.desired_enabled();
    let runtime_status = match runtime_projection {
        ChannelRuntimeProjectionResult::Stopped => "stopped",
        ChannelRuntimeProjectionResult::ActivationPending => "connecting",
        ChannelRuntimeProjectionResult::Active => "running",
        ChannelRuntimeProjectionResult::Degraded => "degraded",
        ChannelRuntimeProjectionResult::Removed => "removed",
    };
    let message = format!(
        "channel {} {} accepted; automatic retry is forbidden",
        channel_id,
        operation_name(operation)
    );

    ChannelMutationResponse {
        success: true,
        data: ChannelMutationResult {
            id: channel_id,
            channel_id,
            name: compatibility.name,
            description: compatibility.description,
            protocol: compatibility.protocol,
            request_id: acceptance.request_id().to_string(),
            operation,
            resulting_revision: acceptance.resulting_revision().get(),
            enabled: desired_enabled,
            desired_enabled,
            runtime_projection,
            runtime_status: runtime_status.to_string(),
            reconciliation_required: acceptance.reconciliation_required(),
            completion_audit,
            retryable: acceptance.is_retryable(),
            message,
        },
        metadata: std::collections::HashMap::new(),
    }
}

const fn operation_name(operation: ChannelMutationOperation) -> &'static str {
    match operation {
        ChannelMutationOperation::Create => "create",
        ChannelMutationOperation::Update => "update",
        ChannelMutationOperation::Delete => "delete",
        ChannelMutationOperation::Enable => "enable",
        ChannelMutationOperation::Disable => "disable",
    }
}

fn application_error(error: ApplicationError) -> AppError {
    match error {
        error @ ApplicationError::PermissionDenied { .. } => {
            http_error(StatusCode::FORBIDDEN, error.to_string())
        },
        ApplicationError::ConfirmationRequired { .. } => http_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Explicit confirmation is required (x-aether-confirmed: true)",
        ),
        error @ ApplicationError::InvalidChannelMutation(_) => {
            AppError::bad_request(error.to_string())
        },
        ApplicationError::AuditUnavailable(error) => {
            tracing::error!(error = %error, "mandatory channel mutation audit unavailable");
            AppError::service_unavailable("Mandatory channel mutation audit is unavailable")
        },
        ApplicationError::Port(error) => match error.kind() {
            PortErrorKind::InvalidData => AppError::bad_request("Invalid channel mutation data"),
            PortErrorKind::NotFound => AppError::not_found("Channel not found"),
            PortErrorKind::Rejected => AppError::conflict("Channel mutation was rejected"),
            PortErrorKind::Conflict => {
                AppError::conflict("Channel mutation conflicts with current desired state")
            },
            PortErrorKind::Unavailable => {
                tracing::error!(error = %error, "channel mutation adapter unavailable");
                AppError::service_unavailable("Channel mutation adapter is unavailable")
            },
            PortErrorKind::Timeout => {
                tracing::error!(error = %error, "channel mutation adapter timed out");
                http_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Channel mutation timed out before desired state committed",
                )
            },
            PortErrorKind::Permanent => {
                tracing::error!(error = %error, "permanent channel mutation adapter failure");
                AppError::internal_error("Failed to mutate channel desired state")
            },
        },
        other => {
            tracing::error!(error = %other, "unexpected channel mutation application failure");
            AppError::internal_error("Failed to mutate channel desired state")
        },
    }
}

fn reconciliation_application_error(error: ApplicationError) -> AppError {
    match error {
        error @ ApplicationError::PermissionDenied { .. } => {
            http_error(StatusCode::FORBIDDEN, error.to_string())
        },
        ApplicationError::ConfirmationRequired { .. } => http_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Explicit confirmation is required (x-aether-confirmed: true)",
        ),
        ApplicationError::AuditUnavailable(error) => {
            tracing::error!(error = %error, "mandatory channel reconciliation audit unavailable");
            AppError::service_unavailable("Mandatory channel reconciliation audit is unavailable")
        },
        ApplicationError::Port(error) => match error.kind() {
            PortErrorKind::InvalidData => {
                AppError::bad_request("Invalid channel reconciliation scope")
            },
            PortErrorKind::NotFound => AppError::not_found("Channel not found"),
            PortErrorKind::Rejected | PortErrorKind::Conflict => {
                AppError::conflict("Channel reconciliation conflicts with current runtime state")
            },
            PortErrorKind::Unavailable => {
                tracing::error!(error = %error, "channel reconciliation adapter unavailable");
                AppError::service_unavailable("Channel reconciliation adapter is unavailable")
            },
            PortErrorKind::Timeout => {
                tracing::error!(error = %error, "channel reconciliation adapter timed out");
                http_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Channel reconciliation timed out",
                )
            },
            PortErrorKind::Permanent => {
                tracing::error!(error = %error, "permanent channel reconciliation adapter failure");
                AppError::internal_error("Failed to reconcile channel runtime")
            },
        },
        other => {
            tracing::error!(error = %other, "unexpected channel reconciliation application failure");
            AppError::internal_error("Failed to reconcile channel runtime")
        },
    }
}

fn http_error(status: StatusCode, message: impl Into<String>) -> AppError {
    AppError::new(status, ErrorInfo::new(message).with_code(status.as_u16()))
}
