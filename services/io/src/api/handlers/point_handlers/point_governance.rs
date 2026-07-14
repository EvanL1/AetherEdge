//! HTTP adapter for governed point-topology application commands.

use std::sync::Arc;

use aether_application::{ApplicationError, CompletionAuditStatus};
use aether_auth_jwt::AccessTokenAuthenticator;
use aether_ports::{ChannelRevision, PortErrorKind};
use axum::http::{HeaderMap, StatusCode, header};

use crate::dto::{AppError, ChannelCompletionAudit, ChannelCompletionAuditState, ErrorInfo};
use crate::point_topology::{
    PointTopologyAcceptance, PointTopologyApplication, PointTopologyAuthorization,
    PointTopologyMutation,
};

const CONFIRMATION_HEADER: &str = "x-aether-confirmed";
const REQUEST_ID_HEADER: &str = "x-request-id";
const EXPECTED_REVISION_HEADER: &str = "x-aether-expected-revision";

/// HTTP-owned authentication and application references for point commands.
#[derive(Clone)]
pub struct PointTopologyHttpBoundary {
    inner: Option<GovernedPointTopology>,
}

#[derive(Clone)]
struct GovernedPointTopology {
    application: Arc<PointTopologyApplication>,
    access_authenticator: Arc<AccessTokenAuthenticator>,
}

/// One-shot invocation authorized before a handler performs external I/O.
///
/// Private fields prevent handlers from forging the captured application
/// authorization or substituting request metadata after device discovery.
pub(crate) struct PreauthorizedPointTopologyInvocation {
    application: Arc<PointTopologyApplication>,
    authorization: PointTopologyAuthorization,
}

impl PreauthorizedPointTopologyInvocation {
    /// Consumes the lease and invokes the one audited/CAS-fenced command.
    pub async fn mutate(
        self,
        mutation: PointTopologyMutation,
    ) -> Result<PointTopologyAcceptance, AppError> {
        self.application
            .mutate_authorized(self.authorization, mutation)
            .await
            .map_err(application_error)
    }
}

impl PointTopologyHttpBoundary {
    /// Creates the production HTTP boundary.
    #[must_use]
    pub fn governed(
        application: Arc<PointTopologyApplication>,
        access_authenticator: Arc<AccessTokenAuthenticator>,
    ) -> Self {
        Self {
            inner: Some(GovernedPointTopology {
                application,
                access_authenticator,
            }),
        }
    }

    /// Creates a boundary which rejects all mutations before SQL.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self { inner: None }
    }

    /// Authenticates request metadata and invokes one application command.
    pub async fn mutate(
        &self,
        headers: &HeaderMap,
        mutation: PointTopologyMutation,
    ) -> Result<PointTopologyAcceptance, AppError> {
        let governed = self.inner.as_ref().ok_or_else(|| {
            AppError::service_unavailable("Point-topology application boundary is unavailable")
        })?;
        let context = request_context(governed, headers);
        let expected_revision = expected_revision(headers)?;
        governed
            .application
            .mutate(&context, expected_revision, mutation)
            .await
            .map_err(application_error)
    }

    /// Authenticates and authorizes one future mutation before external I/O.
    ///
    /// The same captured request context and revision are consumed by the
    /// eventual application command; no credential or confirmation header is
    /// parsed again after discovery.
    pub(crate) fn preauthorize(
        &self,
        headers: &HeaderMap,
    ) -> Result<PreauthorizedPointTopologyInvocation, AppError> {
        let governed = self.inner.as_ref().ok_or_else(|| {
            AppError::service_unavailable("Point-topology application boundary is unavailable")
        })?;
        let context = request_context(governed, headers);
        let expected_revision = expected_revision(headers)?;
        let authorization = governed
            .application
            .preauthorize(&context, expected_revision)
            .map_err(application_error)?;
        Ok(PreauthorizedPointTopologyInvocation {
            application: Arc::clone(&governed.application),
            authorization,
        })
    }
}

fn request_context(
    governed: &GovernedPointTopology,
    headers: &HeaderMap,
) -> aether_application::RequestContext {
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
    governed
        .access_authenticator
        .invocation(
            authorization,
            request_id,
            confirmed,
            aether_domain::TimestampMs::new(timestamp),
        )
        .context()
        .clone()
}

/// Converts terminal application audit state into the stable HTTP DTO.
pub fn completion_audit(status: &CompletionAuditStatus) -> ChannelCompletionAudit {
    match status {
        CompletionAuditStatus::Recorded => ChannelCompletionAudit {
            status: ChannelCompletionAuditState::Recorded,
            retryable: false,
            message: None,
        },
        CompletionAuditStatus::Incomplete { .. } => ChannelCompletionAudit {
            status: ChannelCompletionAuditState::Incomplete,
            retryable: false,
            message: Some(
                "operation was accepted but its terminal audit is incomplete; reconcile by request_id and do not retry"
                    .to_string(),
            ),
        },
    }
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

fn application_error(error: ApplicationError) -> AppError {
    match error {
        ApplicationError::PermissionDenied { .. } => http_error(
            StatusCode::FORBIDDEN,
            "Point-topology command is not authorized",
        ),
        ApplicationError::ConfirmationRequired { .. } => http_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Point-topology command requires explicit confirmation",
        ),
        ApplicationError::InvalidChannelMutation(message) => AppError::bad_request(message),
        ApplicationError::AuditUnavailable(_) => {
            AppError::service_unavailable("Mandatory point-topology mutation audit is unavailable")
        },
        ApplicationError::Port(error) => match error.kind() {
            PortErrorKind::NotFound => AppError::not_found(error.message()),
            PortErrorKind::Conflict => AppError::conflict(error.message()),
            PortErrorKind::InvalidData => AppError::bad_request(error.message()),
            PortErrorKind::Rejected => http_error(
                StatusCode::FORBIDDEN,
                "Point-topology mutation was rejected",
            ),
            PortErrorKind::Unavailable => {
                AppError::service_unavailable("Point-topology persistence is unavailable")
            },
            PortErrorKind::Timeout => http_error(
                StatusCode::GATEWAY_TIMEOUT,
                "Point-topology persistence timed out",
            ),
            PortErrorKind::Permanent => AppError::internal_error("Point-topology mutation failed"),
        },
        _ => AppError::internal_error("Point-topology application command failed"),
    }
}

fn http_error(status: StatusCode, message: &str) -> AppError {
    AppError::new(status, ErrorInfo::new(message).with_code(status.as_u16()))
}
