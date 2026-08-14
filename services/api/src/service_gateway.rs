use std::sync::Arc;

use aether_auth_jwt::{ROLE_COMMAND_PERMISSIONS, scope_allows};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header},
    response::IntoResponse,
    routing::any,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::Claims;
use crate::config::GatewayConfig;
use crate::state::AppState;

const MAX_GATEWAY_BODY_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceName {
    Io,
    Automation,
    History,
    Uplink,
    Alarm,
}

impl ServiceName {
    #[cfg(test)]
    pub(crate) fn from_route(route: &str) -> Option<Self> {
        match route {
            "io" => Some(Self::Io),
            "automation" => Some(Self::Automation),
            "history" => Some(Self::History),
            "uplink" => Some(Self::Uplink),
            "alarm" => Some(Self::Alarm),
            _ => None,
        }
    }

    pub(crate) fn base_url(self, config: &GatewayConfig) -> &str {
        match self {
            Self::Io => &config.io_service_url,
            Self::Automation => &config.automation_service_url,
            Self::History => &config.history_service_url,
            Self::Uplink => &config.uplink_service_url,
            Self::Alarm => &config.alarm_service_url,
        }
    }

    #[cfg(feature = "swagger-ui")]
    pub(crate) fn from_openapi_name(name: &str) -> Option<Self> {
        match name {
            "io" => Some(Self::Io),
            "automation" => Some(Self::Automation),
            "history" => Some(Self::History),
            "uplink" => Some(Self::Uplink),
            "alarm" => Some(Self::Alarm),
            _ => None,
        }
    }

    fn downstream_path(self, path: &str) -> String {
        match self {
            Self::Io | Self::Automation => path.to_owned(),
            Self::History => format!("hisApi/{path}"),
            Self::Uplink => path.to_owned(),
            Self::Alarm => format!("alarmApi/{path}"),
        }
    }
}

pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/io/{*path}", any(proxy_io))
        .route("/automation/{*path}", any(proxy_automation))
        .route("/history/{*path}", any(proxy_history))
        .route("/uplink/{*path}", any(proxy_uplink))
        .route("/alarm/{*path}", any(proxy_alarm))
        .layer(DefaultBodyLimit::max(MAX_GATEWAY_BODY_BYTES))
}

async fn proxy_io(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response<Body> {
    proxy_service(state, ServiceName::Io, path, request).await
}

async fn proxy_automation(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response<Body> {
    proxy_service(state, ServiceName::Automation, path, request).await
}

async fn proxy_history(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response<Body> {
    proxy_service(state, ServiceName::History, path, request).await
}

async fn proxy_uplink(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response<Body> {
    proxy_service(state, ServiceName::Uplink, path, request).await
}

async fn proxy_alarm(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response<Body> {
    proxy_service(state, ServiceName::Alarm, path, request).await
}

async fn proxy_service(
    state: Arc<AppState>,
    service: ServiceName,
    path: String,
    mut request: Request<Body>,
) -> Response<Body> {
    if validate_relative_path(&path).is_err()
        || is_service_local_only_path(&path)
        || !is_supported_gateway_path(service, &path, request.method())
    {
        return gateway_error(
            StatusCode::BAD_REQUEST,
            "INVALID_SERVICE_PATH",
            "the internal application path is invalid",
        );
    }
    let Some(claims) = request.extensions().get::<Claims>() else {
        return gateway_error(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_REQUIRED",
            "an authenticated application identity is required",
        );
    };
    if let Err(error) =
        authorize_service_request(claims, service, &path, request.method(), request.headers())
    {
        return error.into_response();
    }
    let request_id = if is_governed_mutation(service, &path, request.method()) {
        let request_id = match canonical_request_id(request.headers()) {
            Ok(Some(request_id)) => request_id,
            Ok(None) if requires_caller_request_id(service, &path, request.method()) => {
                return gateway_error(
                    StatusCode::BAD_REQUEST,
                    "MISSING_REQUEST_ID",
                    "this device command requires a caller-supplied canonical x-request-id",
                );
            },
            Ok(None) => match HeaderValue::from_str(&Uuid::new_v4().to_string()) {
                Ok(request_id) => request_id,
                Err(_) => {
                    return gateway_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "REQUEST_ID_GENERATION_FAILED",
                        "the application gateway could not create a request ID",
                    );
                },
            },
            Err(()) => {
                return gateway_error(
                    StatusCode::BAD_REQUEST,
                    "INVALID_REQUEST_ID",
                    "x-request-id must contain exactly one canonical UUID",
                );
            },
        };
        request
            .headers_mut()
            .insert(REQUEST_ID_HEADER, request_id.clone());
        Some(request_id)
    } else {
        None
    };
    forward_to_upstream(
        &state.service_client,
        service.base_url(&state.config),
        &service.downstream_path(&path),
        request,
        request_id,
    )
    .await
}

fn canonical_request_id(headers: &HeaderMap) -> Result<Option<HeaderValue>, ()> {
    let mut values = headers.get_all(REQUEST_ID_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let text = value.to_str().map_err(|_| ())?;
    let parsed = Uuid::parse_str(text).map_err(|_| ())?;
    if parsed.to_string() != text {
        return Err(());
    }
    Ok(Some(value.clone()))
}

fn is_service_local_only_path(path: &str) -> bool {
    path == "api/admin"
        || path.starts_with("api/admin/")
        || path == "api/internal"
        || path.starts_with("api/internal/")
}

fn is_supported_gateway_path(service: ServiceName, path: &str, method: &Method) -> bool {
    service != ServiceName::Uplink
        || (path == "health" && matches!(*method, Method::GET | Method::HEAD))
}

fn is_governed_mutation(service: ServiceName, path: &str, method: &Method) -> bool {
    if matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }
    // History batch-query is a read expressed as POST because its filter can be large.
    !(service == ServiceName::History && path == "data/batch-query" && *method == Method::POST)
}

/// Device-point writes use the request UUID as the durable IO `CommandId`.
/// The gateway must not invent that identity: if the complete HTTP response is
/// lost, only a caller-owned ID lets the caller safely query or retry the same
/// physical command instead of accidentally issuing a new one.
fn requires_caller_request_id(service: ServiceName, path: &str, method: &Method) -> bool {
    if service != ServiceName::Automation || *method != Method::POST {
        return false;
    }
    let segments = path.split('/').collect::<Vec<_>>();
    matches!(segments.as_slice(), ["api", "instances", instance_id, "action"] if !instance_id.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayAuthorizationError {
    MutationForbidden,
    ConfirmationRequired,
}

impl IntoResponse for GatewayAuthorizationError {
    fn into_response(self) -> Response<Body> {
        match self {
            Self::MutationForbidden => gateway_error(
                StatusCode::FORBIDDEN,
                "APPLICATION_MUTATION_FORBIDDEN",
                "the authenticated role cannot mutate application state",
            ),
            Self::ConfirmationRequired => gateway_error(
                StatusCode::PRECONDITION_REQUIRED,
                "EXPLICIT_CONFIRMATION_REQUIRED",
                "application mutations require x-aether-confirmed: true",
            ),
        }
    }
}

fn authorize_service_request(
    claims: &Claims,
    service: ServiceName,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
) -> Result<(), GatewayAuthorizationError> {
    if !is_governed_mutation(service, path, method) {
        return Ok(());
    }
    if !matches!(claims.role.as_deref(), Some("Engineer" | "Admin")) {
        return Err(GatewayAuthorizationError::MutationForbidden);
    }
    // A scoped token keeps command authority only if its scope still lists at
    // least one command permission. The downstream service remains the
    // authority on which one applies; this gate exists so a token narrowed to
    // reads is refused here instead of being forwarded to be rejected there.
    if !ROLE_COMMAND_PERMISSIONS
        .iter()
        .any(|permission| scope_allows(&claims.scope, permission))
    {
        return Err(GatewayAuthorizationError::MutationForbidden);
    }
    if headers
        .get("x-aether-confirmed")
        .and_then(|value| value.to_str().ok())
        != Some("true")
    {
        return Err(GatewayAuthorizationError::ConfirmationRequired);
    }
    Ok(())
}

pub(crate) fn validate_relative_path(path: &str) -> Result<(), ()> {
    let lower = path.to_ascii_lowercase();
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(());
    }
    Ok(())
}

pub(crate) async fn forward_to_upstream(
    client: &reqwest::Client,
    base_url: &str,
    relative_path: &str,
    request: Request<Body>,
    request_id: Option<HeaderValue>,
) -> Response<Body> {
    if !matches!(
        *request.method(),
        Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE | Method::HEAD
    ) {
        return gateway_error_with_request_id(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "the method is not supported by the application gateway",
            request_id.as_ref(),
        );
    }

    let mut url = match reqwest::Url::parse(&format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        relative_path
    )) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        _ => {
            return gateway_error_with_request_id(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_CONFIGURATION_INVALID",
                "the internal application service is unavailable",
                request_id.as_ref(),
            );
        },
    };
    url.set_query(request.uri().query());

    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_GATEWAY_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return gateway_error_with_request_id(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "the application request body exceeds the gateway limit",
                request_id.as_ref(),
            );
        },
    };

    let mut downstream = client.request(parts.method, url);
    for name in request_header_allowlist() {
        if let Some(value) = parts.headers.get(&name) {
            downstream = downstream.header(name, value);
        }
    }
    let upstream = match downstream.body(body).send().await {
        Ok(response) => response,
        Err(_) => {
            return gateway_error_with_request_id(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_UNAVAILABLE",
                "the internal application service is unavailable",
                request_id.as_ref(),
            );
        },
    };

    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    if let Some(expected) = request_id.as_ref() {
        let mut actual_values = response_headers.get_all(REQUEST_ID_HEADER).iter();
        if let Some(actual) = actual_values.next()
            && (actual != expected || actual_values.next().is_some())
        {
            tracing::warn!(
                expected_request_id = ?expected,
                actual_request_id = ?actual,
                "downstream application service returned an inconsistent request ID"
            );
            return gateway_error_with_request_id(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_REQUEST_ID_MISMATCH",
                "the internal application service returned an inconsistent request ID",
                request_id.as_ref(),
            );
        }
    }
    let body = Body::from_stream(upstream.bytes_stream());
    let mut response = Response::builder().status(status);
    if let Some(headers) = response.headers_mut() {
        copy_response_headers(&response_headers, headers);
    }
    let mut response = match response.body(body) {
        Ok(response) => response,
        Err(_) => gateway_error_with_request_id(
            StatusCode::INTERNAL_SERVER_ERROR,
            "GATEWAY_RESPONSE_FAILED",
            "the application gateway could not construct a response",
            request_id.as_ref(),
        ),
    };
    if let Some(request_id) = request_id {
        response.headers_mut().insert(REQUEST_ID_HEADER, request_id);
    }
    response
}

fn request_header_allowlist() -> [header::HeaderName; 9] {
    [
        header::AUTHORIZATION,
        header::ACCEPT,
        header::CONTENT_TYPE,
        header::IF_MATCH,
        header::IF_NONE_MATCH,
        header::HeaderName::from_static("x-request-id"),
        header::HeaderName::from_static("x-aether-confirmed"),
        header::HeaderName::from_static("x-aether-expected-revision"),
        header::HeaderName::from_static("idempotency-key"),
    ]
}

fn copy_response_headers(source: &HeaderMap, destination: &mut HeaderMap) {
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_DISPOSITION,
        header::CACHE_CONTROL,
        header::ETAG,
        header::LAST_MODIFIED,
        header::HeaderName::from_static("x-request-id"),
    ] {
        if let Some(value) = source.get(&name) {
            destination.insert(name, value.clone());
        }
    }
}

fn gateway_error(status: StatusCode, code: &'static str, message: &'static str) -> Response<Body> {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

fn gateway_error_with_request_id(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    request_id: Option<&HeaderValue>,
) -> Response<Body> {
    let mut response = gateway_error(status, code, message);
    if let Some(request_id) = request_id {
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, request_id.clone());
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::{Body, Bytes, to_bytes},
        extract::OriginalUri,
        http::{HeaderMap, Method, Request, StatusCode},
        response::IntoResponse,
        routing::any,
    };
    use serde_json::json;

    use super::{
        GatewayAuthorizationError, ServiceName, authorize_service_request, canonical_request_id,
        forward_to_upstream, is_governed_mutation, is_service_local_only_path,
        is_supported_gateway_path, requires_caller_request_id, validate_relative_path,
    };
    use crate::auth::Claims;

    fn claims(role: &str) -> Claims {
        Claims {
            user_id: 7,
            username: "gateway-test".to_owned(),
            role: Some(role.to_owned()),
            token_id: None,
            scope: aether_auth_jwt::permissions_for_role(Some(role))
                .into_iter()
                .map(str::to_owned)
                .collect(),
            exp: usize::MAX,
            iat: 0,
            token_type: "access".to_owned(),
        }
    }

    #[test]
    fn service_names_and_paths_are_closed_to_known_local_targets() {
        assert_eq!(ServiceName::from_route("io"), Some(ServiceName::Io));
        assert_eq!(
            ServiceName::from_route("automation"),
            Some(ServiceName::Automation)
        );
        assert_eq!(
            ServiceName::from_route("history"),
            Some(ServiceName::History)
        );
        assert_eq!(ServiceName::from_route("uplink"), Some(ServiceName::Uplink));
        assert_eq!(ServiceName::from_route("alarm"), Some(ServiceName::Alarm));
        assert_eq!(ServiceName::from_route("http://attacker.invalid"), None);

        assert_eq!(
            ServiceName::Io.downstream_path("api/channels"),
            "api/channels"
        );
        assert_eq!(
            ServiceName::Automation.downstream_path("api/rules"),
            "api/rules"
        );
        assert_eq!(
            ServiceName::History.downstream_path("data/query"),
            "hisApi/data/query"
        );
        assert_eq!(ServiceName::Uplink.downstream_path("health"), "health");
        assert_eq!(
            ServiceName::Alarm.downstream_path("rules"),
            "alarmApi/rules"
        );

        assert!(validate_relative_path("api/channels/7").is_ok());
        assert!(validate_relative_path("api/channels/../secrets").is_err());
        assert!(validate_relative_path("//attacker.invalid/path").is_err());
        assert!(validate_relative_path("api/%2e%2e/secrets").is_err());
        assert!(is_service_local_only_path("api/admin/logs/view"));
        assert!(is_service_local_only_path("api/internal/alarm-events"));
        assert!(!is_service_local_only_path("data/query"));
        assert!(is_supported_gateway_path(
            ServiceName::Uplink,
            "health",
            &Method::GET
        ));
        assert!(!is_supported_gateway_path(
            ServiceName::Uplink,
            "mqtt/status",
            &Method::GET
        ));
    }

    #[test]
    fn application_mutations_require_an_operator_role_and_explicit_confirmation() {
        let headers = HeaderMap::new();
        assert!(!is_governed_mutation(
            ServiceName::History,
            "data/batch-query",
            &Method::POST
        ));
        assert!(is_governed_mutation(
            ServiceName::Alarm,
            "rules",
            &Method::POST
        ));

        let viewer = authorize_service_request(
            &claims("Viewer"),
            ServiceName::Alarm,
            "rules",
            &Method::POST,
            &headers,
        )
        .expect_err("Viewer mutation must fail");
        assert_eq!(viewer, GatewayAuthorizationError::MutationForbidden);

        let engineer = authorize_service_request(
            &claims("Engineer"),
            ServiceName::Alarm,
            "rules",
            &Method::POST,
            &headers,
        )
        .expect_err("unconfirmed mutation must fail");
        assert_eq!(engineer, GatewayAuthorizationError::ConfirmationRequired);

        let mut confirmed = HeaderMap::new();
        confirmed.insert("x-aether-confirmed", "true".parse().expect("valid header"));
        authorize_service_request(
            &claims("Admin"),
            ServiceName::Alarm,
            "rules",
            &Method::POST,
            &confirmed,
        )
        .expect("confirmed Admin mutation must pass");
        authorize_service_request(
            &claims("Viewer"),
            ServiceName::History,
            "data/batch-query",
            &Method::POST,
            &headers,
        )
        .expect("read-only batch query must pass");
    }

    #[test]
    fn governed_request_ids_must_be_single_canonical_uuids() {
        let canonical = "0190aee6-2139-7a87-8448-806f1b843201";
        assert!(
            canonical_request_id(&HeaderMap::new())
                .expect("missing request ID is generated by the caller")
                .is_none()
        );

        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", canonical.parse().expect("valid header"));
        assert_eq!(
            canonical_request_id(&headers)
                .expect("canonical request ID")
                .and_then(|value| value.to_str().ok().map(str::to_owned)),
            Some(canonical.to_owned())
        );

        for invalid in [
            "not-a-uuid",
            "0190AEE6-2139-7A87-8448-806F1B843201",
            "{0190aee6-2139-7a87-8448-806f1b843201}",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-request-id", invalid.parse().expect("valid header bytes"));
            assert!(canonical_request_id(&headers).is_err(), "value {invalid}");
        }

        let mut repeated = HeaderMap::new();
        repeated.append("x-request-id", canonical.parse().expect("valid header"));
        repeated.append("x-request-id", canonical.parse().expect("valid header"));
        assert!(canonical_request_id(&repeated).is_err());
    }

    #[test]
    fn physical_device_action_requires_a_caller_owned_request_id() {
        assert!(requires_caller_request_id(
            ServiceName::Automation,
            "api/instances/7/action",
            &Method::POST,
        ));
        assert!(!requires_caller_request_id(
            ServiceName::Automation,
            "api/instances/7",
            &Method::PUT,
        ));
        assert!(!requires_caller_request_id(
            ServiceName::Automation,
            "api/rules/7/execute",
            &Method::POST,
        ));
        assert!(!requires_caller_request_id(
            ServiceName::Io,
            "api/instances/7/action",
            &Method::POST,
        ));
    }

    fn scoped_claims(role: &str, scope: Vec<&str>) -> Claims {
        Claims {
            scope: scope.into_iter().map(str::to_owned).collect(),
            ..claims(role)
        }
    }

    #[test]
    fn a_scope_without_command_permissions_cannot_pass_a_governed_mutation() {
        let mut confirmed = HeaderMap::new();
        confirmed.insert("x-aether-confirmed", "true".parse().expect("valid header"));

        // Confirmation is asserted by the caller, so it is no obstacle to an AI
        // client holding an administrative token. The scope is what has to stop
        // it: narrowed away from every command permission, the mutation must be
        // refused here rather than forwarded for the service to reject.
        let forbidden = authorize_service_request(
            &scoped_claims("Admin", vec!["data_processing.read"]),
            ServiceName::Uplink,
            "mqtt/config",
            &Method::POST,
            &confirmed,
        )
        .expect_err("read-only scope must not mutate");
        assert_eq!(forbidden, GatewayAuthorizationError::MutationForbidden);

        // A scope that still carries command authority keeps working.
        authorize_service_request(
            &scoped_claims("Admin", vec!["io.channel.manage"]),
            ServiceName::Uplink,
            "mqtt/config",
            &Method::POST,
            &confirmed,
        )
        .expect("command-scoped mutation must pass");

        // Reads are never governed mutations, so a narrow scope keeps them.
        authorize_service_request(
            &scoped_claims("Viewer", vec!["data_processing.read"]),
            ServiceName::History,
            "data/batch-query",
            &Method::POST,
            &HeaderMap::new(),
        )
        .expect("read-only batch query must pass");
    }

    #[tokio::test]
    async fn proxy_preserves_application_credentials_but_drops_forged_identity_headers() {
        async fn echo_request(
            method: Method,
            OriginalUri(uri): OriginalUri,
            headers: HeaderMap,
            body: Bytes,
        ) -> impl IntoResponse {
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            (
                StatusCode::ACCEPTED,
                [("x-request-id", "downstream-request")],
                axum::Json(json!({
                    "method": method.as_str(),
                    "uri": uri.to_string(),
                    "authorization": header("authorization"),
                    "confirmed": header("x-aether-confirmed"),
                    "expected_revision": header("x-aether-expected-revision"),
                    "forged_actor": header("x-aether-actor-id"),
                    "body": String::from_utf8_lossy(&body),
                })),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated downstream server");
        let address = listener.local_addr().expect("downstream server address");
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/api/channels/{id}", any(echo_request)),
            )
            .await
            .expect("serve isolated downstream server");
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/io/api/channels/7?include=points")
            .header("authorization", "Bearer signed-user-token")
            .header("content-type", "application/json")
            .header("x-aether-confirmed", "true")
            .header("x-aether-expected-revision", "41")
            .header("x-aether-actor-id", "forged-admin")
            .body(Body::from(r#"{"enabled":true}"#))
            .expect("valid gateway request");

        let response = forward_to_upstream(
            &reqwest::Client::new(),
            &format!("http://{address}"),
            "api/channels/7",
            request,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(response.headers()["x-request-id"], "downstream-request");
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("read downstream response");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("decode downstream echo");
        assert_eq!(payload["method"], "POST");
        assert_eq!(payload["uri"], "/api/channels/7?include=points");
        assert_eq!(payload["authorization"], "Bearer signed-user-token");
        assert_eq!(payload["confirmed"], "true");
        assert_eq!(payload["expected_revision"], "41");
        assert!(payload["forged_actor"].is_null());
        assert_eq!(payload["body"], r#"{"enabled":true}"#);
    }

    #[tokio::test]
    async fn governed_response_always_returns_the_forwarded_request_id() {
        async fn echo_without_response_header(
            OriginalUri(uri): OriginalUri,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            let status = if uri.path().ends_with("/failure") {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::ACCEPTED
            };
            (
                status,
                axum::Json(json!({
                    "request_id": headers
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok()),
                })),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated downstream server");
        let address = listener.local_addr().expect("downstream server address");
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/{result}", any(echo_without_response_header)),
            )
            .await
            .expect("serve isolated downstream server");
        });

        let request_id = "0190aee6-2139-7a87-8448-806f1b843201";
        for (path, expected_status) in [
            ("success", StatusCode::ACCEPTED),
            ("failure", StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!("/api/io/{path}"))
                .header("x-request-id", request_id)
                .body(Body::empty())
                .expect("valid gateway request");
            let response = forward_to_upstream(
                &reqwest::Client::new(),
                &format!("http://{address}"),
                path,
                request,
                Some(request_id.parse().expect("valid request ID header")),
            )
            .await;

            assert_eq!(response.status(), expected_status);
            assert_eq!(response.headers()["x-request-id"], request_id);
            let body = to_bytes(response.into_body(), 16 * 1024)
                .await
                .expect("read downstream response");
            let payload: serde_json::Value =
                serde_json::from_slice(&body).expect("decode downstream response");
            assert_eq!(payload["request_id"], request_id);
        }
    }

    #[tokio::test]
    async fn mismatched_downstream_request_id_fails_closed() {
        async fn mismatched() -> impl IntoResponse {
            (
                StatusCode::ACCEPTED,
                [("x-request-id", "0190aee6-2139-7a87-8448-806f1b843202")],
                axum::Json(json!({ "accepted": true })),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated downstream server");
        let address = listener.local_addr().expect("downstream server address");
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/command", any(mismatched)))
                .await
                .expect("serve isolated downstream server");
        });

        let request_id = "0190aee6-2139-7a87-8448-806f1b843201";
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/io/command")
            .header("x-request-id", request_id)
            .body(Body::empty())
            .expect("valid gateway request");
        let response = forward_to_upstream(
            &reqwest::Client::new(),
            &format!("http://{address}"),
            "command",
            request,
            Some(request_id.parse().expect("valid request ID header")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()["x-request-id"], request_id);
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("read gateway response");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("decode gateway error");
        assert_eq!(payload["error"]["code"], "UPSTREAM_REQUEST_ID_MISMATCH");
    }

    #[tokio::test]
    async fn transport_failure_is_a_sanitized_bad_gateway_response() {
        let request_id = "0190aee6-2139-7a87-8448-806f1b843201";
        let request = Request::builder()
            .uri("/api/io/health")
            .header("x-request-id", request_id)
            .body(Body::empty())
            .expect("valid gateway request");
        let response = forward_to_upstream(
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            "health",
            request,
            Some(request_id.parse().expect("valid request ID header")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()["x-request-id"], request_id);
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("read gateway error");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("decode gateway error");
        assert_eq!(payload["error"]["code"], "UPSTREAM_UNAVAILABLE");
        assert_eq!(
            payload["error"]["message"],
            "the internal application service is unavailable"
        );
        assert!(!String::from_utf8_lossy(&body).contains("127.0.0.1"));
    }
}
