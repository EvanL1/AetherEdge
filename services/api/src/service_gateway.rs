use std::sync::Arc;

use aether_application::{ApplicationError, SafetyPolicy};
use aether_ports::AuditOutcome;
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
use crate::gateway_command_audit::{
    CommandAudit, GatewayCommand, command_context, history_command, uplink_command,
};
use crate::state::AppState;

const MAX_GATEWAY_BODY_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_ID_HEADER: &str = "x-request-id";
const AUDIT_STATE_HEADER: &str = "x-aether-audit";

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
            Self::Uplink => format!("netApi/{path}"),
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
    if validate_relative_path(&path).is_err() || is_internal_admin_path(service, &path) {
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
    if let Some(command) = gateway_audited_command(service, &path, request.method()) {
        let claims = claims.clone();
        return forward_audited(state, service, path, command, &claims, request).await;
    }
    if let Err(error) =
        authorize_service_request(claims, service, &path, request.method(), request.headers())
    {
        return error.into_response();
    }
    if is_governed_mutation(service, &path, request.method())
        && !request.headers().contains_key("x-request-id")
    {
        let request_id = Uuid::new_v4().to_string();
        if let Ok(value) = request_id.parse() {
            request.headers_mut().insert("x-request-id", value);
        }
    }
    forward_to_upstream(
        &state.service_client,
        service.base_url(&state.config),
        &service.downstream_path(&path),
        request,
    )
    .await
}

fn gateway_audited_command(
    service: ServiceName,
    path: &str,
    method: &Method,
) -> Option<GatewayCommand> {
    if !is_governed_mutation(service, path, method) {
        return None;
    }
    match service {
        ServiceName::History => Some(history_command(path, method)),
        ServiceName::Uplink => Some(uplink_command(path, method)),
        ServiceName::Io | ServiceName::Automation | ServiceName::Alarm => None,
    }
}

async fn forward_audited(
    state: Arc<AppState>,
    service: ServiceName,
    path: String,
    command: GatewayCommand,
    claims: &Claims,
    mut request: Request<Body>,
) -> Response<Body> {
    if !is_forwardable_method(request.method()) {
        return method_not_allowed();
    }
    let context = command_context(claims, request.headers());
    let Ok(request_id) = HeaderValue::from_str(context.request_id()) else {
        return gateway_error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST_ID",
            "the request identifier is invalid",
        );
    };
    let audit = CommandAudit::new(Arc::clone(&state.command_audit), context, command);

    if let Err(error) = SafetyPolicy.authorize(command.capability(), audit.context()) {
        let reason = error.to_string();
        if audit
            .record(AuditOutcome::Rejected, Some(&reason))
            .await
            .is_err()
        {
            return audit_unavailable();
        }
        let mut response = match error {
            ApplicationError::ConfirmationRequired { .. } => {
                GatewayAuthorizationError::ConfirmationRequired
            },
            _ => GatewayAuthorizationError::MutationForbidden,
        }
        .into_response();
        response.headers_mut().insert(REQUEST_ID_HEADER, request_id);
        return response;
    }
    if audit.record(AuditOutcome::Attempted, None).await.is_err() {
        return audit_unavailable();
    }
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, request_id.clone());

    // Detached so a caller that disconnects cannot skip the terminal record.
    let dispatch = tokio::spawn(async move {
        let mut response = forward_to_upstream(
            &state.service_client,
            service.base_url(&state.config),
            &service.downstream_path(&path),
            request,
        )
        .await;
        let audit_state = audit.record_terminal(response.status()).await;
        let headers = response.headers_mut();
        headers.insert(REQUEST_ID_HEADER, request_id);
        headers.insert(AUDIT_STATE_HEADER, HeaderValue::from_static(audit_state));
        response
    });
    dispatch.await.unwrap_or_else(|_| {
        gateway_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "GATEWAY_RESPONSE_FAILED",
            "the application gateway could not construct a response",
        )
    })
}

fn audit_unavailable() -> Response<Body> {
    gateway_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "COMMAND_AUDIT_UNAVAILABLE",
        "the mandatory command audit is unavailable; nothing was changed",
    )
}

fn is_internal_admin_path(service: ServiceName, path: &str) -> bool {
    matches!(service, ServiceName::Io | ServiceName::Automation)
        && (path == "api/admin" || path.starts_with("api/admin/"))
}

fn is_governed_mutation(service: ServiceName, path: &str, method: &Method) -> bool {
    if matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }
    // History batch-query is a read expressed as POST because its filter can be large.
    !(service == ServiceName::History && path == "data/batch-query" && *method == Method::POST)
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
    if !claims.may_mutate_application_state() {
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
        || path.contains(['\\', '?', '#'])
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

fn is_forwardable_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE | Method::HEAD
    )
}

fn method_not_allowed() -> Response<Body> {
    gateway_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "the method is not supported by the application gateway",
    )
}

pub(crate) async fn forward_to_upstream(
    client: &reqwest::Client,
    base_url: &str,
    relative_path: &str,
    request: Request<Body>,
) -> Response<Body> {
    if !is_forwardable_method(request.method()) {
        return method_not_allowed();
    }

    let mut url = match reqwest::Url::parse(&format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        relative_path
    )) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        _ => {
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_CONFIGURATION_INVALID",
                "the internal application service is unavailable",
            );
        },
    };
    url.set_query(request.uri().query());

    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_GATEWAY_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return gateway_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "the application request body exceeds the gateway limit",
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
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_UNAVAILABLE",
                "the internal application service is unavailable",
            );
        },
    };

    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    let body = Body::from_stream(upstream.bytes_stream());
    let mut response = Response::builder().status(status);
    if let Some(headers) = response.headers_mut() {
        copy_response_headers(&response_headers, headers);
    }
    match response.body(body) {
        Ok(response) => response,
        Err(_) => gateway_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "GATEWAY_RESPONSE_FAILED",
            "the application gateway could not construct a response",
        ),
    }
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
        GatewayAuthorizationError, ServiceName, authorize_service_request, forward_to_upstream,
        is_governed_mutation, is_internal_admin_path, validate_relative_path,
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
        assert_eq!(
            ServiceName::Uplink.downstream_path("mqtt/status"),
            "netApi/mqtt/status"
        );
        assert_eq!(
            ServiceName::Alarm.downstream_path("rules"),
            "alarmApi/rules"
        );

        assert!(validate_relative_path("api/channels/7").is_ok());
        assert!(validate_relative_path("api/channels/../secrets").is_err());
        assert!(validate_relative_path("//attacker.invalid/path").is_err());
        assert!(validate_relative_path("api/%2e%2e/secrets").is_err());
        assert!(is_internal_admin_path(
            ServiceName::Io,
            "api/admin/logs/view"
        ));
        assert!(is_internal_admin_path(
            ServiceName::Automation,
            "api/admin/logs/level"
        ));
        assert!(!is_internal_admin_path(ServiceName::History, "data/query"));
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
            ServiceName::Alarm,
            "rules",
            &Method::POST,
            &confirmed,
        )
        .expect_err("read-only scope must not mutate");
        assert_eq!(forbidden, GatewayAuthorizationError::MutationForbidden);

        // A scope that still carries command authority keeps working.
        authorize_service_request(
            &scoped_claims("Admin", vec!["io.channel.manage"]),
            ServiceName::Alarm,
            "rules",
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
            .uri("/api/v1/io/api/channels/7?include=points")
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
    async fn transport_failure_is_a_sanitized_bad_gateway_response() {
        let request = Request::builder()
            .uri("/api/v1/io/health")
            .body(Body::empty())
            .expect("valid gateway request");
        let response = forward_to_upstream(
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            "health",
            request,
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
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

    mod gateway_audited_commands {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::Duration;

        use aether_ports::{
            AuditOutcome, AuditRecord, AuditSink, PortError, PortErrorKind, PortResult,
        };
        use aether_store_local::MemoryAuditSink;
        use async_trait::async_trait;
        use axum::{
            Router,
            body::{Body, to_bytes},
            http::{HeaderMap, Method, Request, Response, StatusCode},
            routing::any,
        };
        use serde_json::json;
        use uuid::Uuid;

        use super::{claims, scoped_claims};
        use crate::auth::Claims;
        use crate::service_gateway::{ServiceName, proxy_service};
        use crate::test_support::app_state_with_audited_upstream;

        const CAPABILITY: &str = "history.config.manage";

        struct FailingAfter {
            accepted: usize,
            seen: AtomicUsize,
            inner: MemoryAuditSink,
        }

        impl FailingAfter {
            fn new(accepted: usize) -> Self {
                Self {
                    accepted,
                    seen: AtomicUsize::new(0),
                    inner: MemoryAuditSink::new(),
                }
            }
        }

        #[async_trait]
        impl AuditSink for FailingAfter {
            async fn record(&self, record: AuditRecord) -> PortResult<()> {
                if self.seen.fetch_add(1, Ordering::SeqCst) >= self.accepted {
                    return Err(PortError::new(
                        PortErrorKind::Unavailable,
                        "audit store offline",
                    ));
                }
                self.inner.record(record).await
            }
        }

        type SeenRequestIds = Arc<Mutex<Vec<Option<String>>>>;

        async fn history_upstream(status: StatusCode) -> (String, SeenRequestIds) {
            slow_history_upstream(status, Duration::ZERO).await
        }

        async fn slow_history_upstream(
            status: StatusCode,
            delay: Duration,
        ) -> (String, SeenRequestIds) {
            let seen: SeenRequestIds = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&seen);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind isolated history server");
            let address = listener.local_addr().expect("history server address");
            let handler = move |headers: HeaderMap| {
                let recorder = Arc::clone(&recorder);
                async move {
                    let request_id = headers
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    recorder
                        .lock()
                        .expect("record history request")
                        .push(request_id);
                    tokio::time::sleep(delay).await;
                    (
                        status,
                        axum::Json(json!({ "success": status.is_success() })),
                    )
                }
            };
            let app = Router::new()
                .route("/hisApi/{*path}", any(handler.clone()))
                .route("/netApi/{*path}", any(handler));
            tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve isolated history server");
            });
            (format!("http://{address}"), seen)
        }

        fn mutation(
            claims: Claims,
            method: Method,
            confirmed: bool,
            body: &'static str,
        ) -> Request<Body> {
            let mut builder = Request::builder()
                .method(method)
                .uri("/api/v1/history/storage")
                .header("content-type", "application/json");
            if confirmed {
                builder = builder.header("x-aether-confirmed", "true");
            }
            let mut request = builder
                .body(Body::from(body))
                .expect("valid gateway request");
            request.extensions_mut().insert(claims);
            request
        }

        async fn error_code(response: Response<Body>) -> String {
            let body = to_bytes(response.into_body(), 16 * 1024)
                .await
                .expect("read gateway error");
            let payload: serde_json::Value =
                serde_json::from_slice(&body).expect("decode gateway error");
            payload["error"]["code"]
                .as_str()
                .expect("gateway error code")
                .to_owned()
        }

        fn forwarded(seen: &SeenRequestIds) -> Vec<Option<String>> {
            seen.lock().expect("read history requests").clone()
        }

        async fn records_once_there_are(audit: &MemoryAuditSink, count: usize) -> Vec<AuditRecord> {
            for _ in 0..300 {
                let records = audit.records().expect("audit records");
                if records.len() >= count {
                    return records;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the audit trail never reached {count} records");
        }

        #[tokio::test]
        async fn a_confirmed_mutation_is_audited_before_and_after_the_forward() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let response = proxy_service(
                state,
                ServiceName::History,
                "storage".to_owned(),
                mutation(
                    claims("Engineer"),
                    Method::PUT,
                    true,
                    r#"{"backend":"timescaledb","password":"s3cret-dsn"}"#,
                ),
            )
            .await;

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["x-aether-audit"], "recorded");
            let request_id = response.headers()["x-request-id"]
                .to_str()
                .expect("request id header")
                .to_owned();
            assert!(Uuid::parse_str(&request_id).is_ok());
            assert_eq!(forwarded(&seen), vec![Some(request_id.clone())]);

            let records = audit.records().expect("audit records");
            assert_eq!(
                records.iter().map(AuditRecord::outcome).collect::<Vec<_>>(),
                vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
            );
            for record in &records {
                assert_eq!(record.capability(), CAPABILITY);
                assert_eq!(record.actor_id(), "user:7");
                assert_eq!(record.request_id(), request_id);
                let detail = record.detail().expect("audit detail");
                assert!(detail.contains("operation=storage.update"), "{detail}");
                assert!(!detail.contains("s3cret-dsn"), "{detail}");
            }
        }

        #[tokio::test]
        async fn a_caller_request_id_is_kept_only_when_it_is_a_uuid() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;
            let supplied = "0f0e0d0c-0b0a-4908-8706-050403020100";

            for value in [supplied, "not-a-uuid"] {
                let mut request = mutation(claims("Admin"), Method::PUT, true, "{}");
                request
                    .headers_mut()
                    .insert("x-request-id", value.parse().expect("valid header"));
                let response = proxy_service(
                    state.clone(),
                    ServiceName::History,
                    "config".to_owned(),
                    request,
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
            }

            let forwarded = forwarded(&seen);
            assert_eq!(forwarded[0].as_deref(), Some(supplied));
            let replaced = forwarded[1].as_deref().expect("forwarded request id");
            assert_ne!(replaced, "not-a-uuid");
            assert!(Uuid::parse_str(replaced).is_ok());
            let records = audit.records().expect("audit records");
            assert_eq!(records[0].request_id(), supplied);
            assert_eq!(records[3].request_id(), replaced);
        }

        #[tokio::test]
        async fn a_rejected_mutation_is_audited_and_never_forwarded() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let cases = [
                (
                    claims("Engineer"),
                    false,
                    StatusCode::PRECONDITION_REQUIRED,
                    "EXPLICIT_CONFIRMATION_REQUIRED",
                ),
                (
                    scoped_claims("Admin", vec!["io.channel.manage"]),
                    true,
                    StatusCode::FORBIDDEN,
                    "APPLICATION_MUTATION_FORBIDDEN",
                ),
                (
                    claims("Viewer"),
                    true,
                    StatusCode::FORBIDDEN,
                    "APPLICATION_MUTATION_FORBIDDEN",
                ),
            ];
            for (claims, confirmed, status, code) in cases {
                let response = proxy_service(
                    state.clone(),
                    ServiceName::History,
                    "storage/reconnect".to_owned(),
                    mutation(
                        claims,
                        Method::POST,
                        confirmed,
                        r#"{"password":"s3cret-dsn"}"#,
                    ),
                )
                .await;
                assert_eq!(response.status(), status);
                let request_id = response.headers()["x-request-id"]
                    .to_str()
                    .expect("request id header")
                    .to_owned();
                let recorded = audit.records().expect("audit records");
                assert_eq!(
                    recorded.last().expect("rejected record").request_id(),
                    request_id
                );
                assert_eq!(error_code(response).await, code);
            }

            assert!(forwarded(&seen).is_empty());
            let records = audit.records().expect("audit records");
            assert_eq!(records.len(), 3);
            for record in &records {
                assert_eq!(record.outcome(), AuditOutcome::Rejected);
                assert_eq!(record.capability(), CAPABILITY);
                let detail = record.detail().expect("audit detail");
                assert!(detail.contains("operation=storage.reconnect"), "{detail}");
                assert!(!detail.contains("s3cret-dsn"), "{detail}");
            }
        }

        #[tokio::test]
        async fn a_mutation_fails_closed_when_the_attempt_cannot_be_audited() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let state = app_state_with_audited_upstream(url, Arc::new(FailingAfter::new(0))).await;

            for confirmed in [true, false] {
                let response = proxy_service(
                    state.clone(),
                    ServiceName::History,
                    "config".to_owned(),
                    mutation(claims("Admin"), Method::PUT, confirmed, "{}"),
                )
                .await;
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(error_code(response).await, "COMMAND_AUDIT_UNAVAILABLE");
            }
            assert!(forwarded(&seen).is_empty());
        }

        #[tokio::test]
        async fn an_applied_mutation_reports_an_incomplete_terminal_audit() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(FailingAfter::new(1));
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let response = proxy_service(
                state,
                ServiceName::History,
                "config".to_owned(),
                mutation(claims("Admin"), Method::PUT, true, "{}"),
            )
            .await;

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["x-aether-audit"], "incomplete");
            assert_eq!(forwarded(&seen).len(), 1);
            let records = audit.inner.records().expect("audit records");
            assert_eq!(
                records.iter().map(AuditRecord::outcome).collect::<Vec<_>>(),
                vec![AuditOutcome::Attempted]
            );
        }

        #[tokio::test]
        async fn a_refused_or_unreachable_upstream_is_audited_as_failed() {
            let (refusing, _) = history_upstream(StatusCode::INTERNAL_SERVER_ERROR).await;
            let targets = [
                (
                    refusing,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "response_status=500",
                ),
                (
                    "http://127.0.0.1:1".to_owned(),
                    StatusCode::BAD_GATEWAY,
                    "response_status=502; upstream_outcome=unknown",
                ),
            ];
            for (url, status, expected) in targets {
                let audit = Arc::new(MemoryAuditSink::new());
                let state = app_state_with_audited_upstream(url, audit.clone()).await;
                let response = proxy_service(
                    state,
                    ServiceName::History,
                    "config".to_owned(),
                    mutation(claims("Admin"), Method::PUT, true, "{}"),
                )
                .await;

                assert_eq!(response.status(), status);
                assert_eq!(response.headers()["x-aether-audit"], "recorded");
                let records = audit.records().expect("audit records");
                assert_eq!(
                    records.iter().map(AuditRecord::outcome).collect::<Vec<_>>(),
                    vec![AuditOutcome::Attempted, AuditOutcome::Failed]
                );
                let detail = records[1].detail().expect("audit detail");
                assert!(detail.contains("operation=config.update"), "{detail}");
                assert!(detail.contains(expected), "{detail}");
            }
        }

        #[tokio::test]
        async fn reads_and_service_owned_commands_add_no_gateway_audit_record() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            for (service, path, method) in [
                (ServiceName::History, "config", Method::GET),
                (ServiceName::History, "data/batch-query", Method::POST),
                (ServiceName::Io, "api/channels", Method::POST),
            ] {
                let request = mutation(claims("Admin"), method, true, "{}");
                let response =
                    proxy_service(state.clone(), service, path.to_owned(), request).await;
                assert!(!response.headers().contains_key("x-aether-audit"));
            }

            assert_eq!(forwarded(&seen).len(), 2);
            assert!(audit.records().expect("audit records").is_empty());
        }

        #[tokio::test]
        async fn a_token_scoped_to_history_alone_may_change_it() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let response = proxy_service(
                state,
                ServiceName::History,
                "config".to_owned(),
                mutation(
                    scoped_claims("Engineer", vec![CAPABILITY]),
                    Method::PUT,
                    true,
                    "{}",
                ),
            )
            .await;

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(forwarded(&seen).len(), 1);
        }

        #[tokio::test]
        async fn an_unlisted_mutation_is_still_governed_and_audited() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let refused = proxy_service(
                state.clone(),
                ServiceName::History,
                "storage".to_owned(),
                mutation(claims("Viewer"), Method::DELETE, true, "{}"),
            )
            .await;
            assert_eq!(refused.status(), StatusCode::FORBIDDEN);
            assert!(forwarded(&seen).is_empty());

            let allowed = proxy_service(
                state,
                ServiceName::History,
                "storage".to_owned(),
                mutation(claims("Admin"), Method::DELETE, true, "{}"),
            )
            .await;
            assert_eq!(allowed.status(), StatusCode::OK);

            let records = audit.records().expect("audit records");
            assert_eq!(
                records.iter().map(AuditRecord::outcome).collect::<Vec<_>>(),
                vec![
                    AuditOutcome::Rejected,
                    AuditOutcome::Attempted,
                    AuditOutcome::Succeeded
                ]
            );
            for record in &records {
                let detail = record.detail().expect("audit detail");
                assert!(detail.contains("operation=unrecognized"), "{detail}");
            }
        }

        #[tokio::test]
        async fn a_method_the_gateway_cannot_forward_leaves_no_attempt_behind() {
            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let response = proxy_service(
                state,
                ServiceName::History,
                "config".to_owned(),
                mutation(claims("Admin"), Method::OPTIONS, true, "{}"),
            )
            .await;

            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert!(forwarded(&seen).is_empty());
            assert!(audit.records().expect("audit records").is_empty());
        }

        #[tokio::test]
        async fn an_encoded_query_or_fragment_cannot_relabel_a_mutation() {
            use tower::ServiceExt;

            let (url, seen) = history_upstream(StatusCode::OK).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            for path in ["storage%3Fx", "storage%3F", "storage%23", "storage%23x"] {
                let mut request = Request::builder()
                    .method(Method::PUT)
                    .uri(format!("/history/{path}"))
                    .header("x-aether-confirmed", "true")
                    .body(Body::from("{}"))
                    .expect("valid gateway request");
                request.extensions_mut().insert(claims("Admin"));
                let response = crate::service_gateway::router()
                    .with_state(state.clone())
                    .oneshot(request)
                    .await
                    .expect("gateway response");
                assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
                assert_eq!(error_code(response).await, "INVALID_SERVICE_PATH");
            }

            assert!(forwarded(&seen).is_empty());
            assert!(audit.records().expect("audit records").is_empty());
        }

        #[tokio::test]
        async fn a_caller_that_disconnects_cannot_skip_the_terminal_record() {
            let (url, seen) =
                slow_history_upstream(StatusCode::OK, Duration::from_millis(200)).await;
            let audit = Arc::new(MemoryAuditSink::new());
            let state = app_state_with_audited_upstream(url, audit.clone()).await;

            let caller = tokio::spawn(proxy_service(
                state,
                ServiceName::History,
                "storage/reconnect".to_owned(),
                mutation(claims("Admin"), Method::POST, true, "{}"),
            ));
            records_once_there_are(&audit, 1).await;
            caller.abort();

            let records = records_once_there_are(&audit, 2).await;
            assert_eq!(
                records.iter().map(AuditRecord::outcome).collect::<Vec<_>>(),
                vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
            );
            assert_eq!(forwarded(&seen).len(), 1);
        }

        mod uplink {
            use super::*;
            use crate::gateway_command_audit::uplink_command;

            const CONFIG: &str = "uplink.config.manage";
            const PUBLISH: &str = "uplink.message.publish";
            const ROUTES: [(Method, &str, &str, &str); 7] = [
                (Method::POST, "mqtt/config", CONFIG, "mqtt.config.update"),
                (Method::POST, "mqtt/disconnect", CONFIG, "mqtt.disconnect"),
                (Method::POST, "mqtt/reconnect", CONFIG, "mqtt.reconnect"),
                (
                    Method::POST,
                    "certificate/upload",
                    CONFIG,
                    "certificate.upload",
                ),
                (
                    Method::DELETE,
                    "certificate/client_key",
                    CONFIG,
                    "certificate.delete",
                ),
                (Method::POST, "alarm/broadcast", PUBLISH, "alarm.broadcast"),
                (Method::POST, "inst-sync", PUBLISH, "instance.sync"),
            ];

            #[tokio::test]
            async fn every_uplink_mutation_is_audited_under_its_capability() {
                let (url, seen) = history_upstream(StatusCode::OK).await;
                let audit = Arc::new(MemoryAuditSink::new());
                let state = app_state_with_audited_upstream(url, audit.clone()).await;

                for (method, path, _, _) in ROUTES {
                    let response = proxy_service(
                        state.clone(),
                        ServiceName::Uplink,
                        path.to_owned(),
                        mutation(
                            claims("Engineer"),
                            method,
                            true,
                            "-----BEGIN PRIVATE KEY-----",
                        ),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::OK, "{path}");
                    assert_eq!(response.headers()["x-aether-audit"], "recorded");
                }

                assert_eq!(forwarded(&seen).len(), ROUTES.len());
                let records = audit.records().expect("audit records");
                assert_eq!(records.len(), ROUTES.len() * 2);
                for (pair, (_, path, capability, operation)) in records.chunks(2).zip(ROUTES) {
                    assert_eq!(pair[0].outcome(), AuditOutcome::Attempted, "{path}");
                    assert_eq!(pair[1].outcome(), AuditOutcome::Succeeded, "{path}");
                    for record in pair {
                        assert_eq!(record.capability(), capability, "{path}");
                        assert_eq!(record.actor_id(), "user:7");
                        let detail = record.detail().expect("audit detail");
                        assert_eq!(detail, format!("operation={operation}"), "{path}");
                    }
                }
            }

            #[tokio::test]
            async fn each_uplink_scope_reaches_only_its_own_routes() {
                let (url, seen) = history_upstream(StatusCode::OK).await;
                let audit = Arc::new(MemoryAuditSink::new());
                let state = app_state_with_audited_upstream(url, audit.clone()).await;

                let cases = [
                    (vec![CONFIG], "mqtt/config", StatusCode::OK),
                    (vec![CONFIG], "alarm/broadcast", StatusCode::FORBIDDEN),
                    (vec![PUBLISH], "inst-sync", StatusCode::OK),
                    (vec![PUBLISH], "mqtt/config", StatusCode::FORBIDDEN),
                    (vec![PUBLISH], "certificate/upload", StatusCode::FORBIDDEN),
                    (
                        vec!["io.channel.manage"],
                        "mqtt/disconnect",
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        vec!["history.config.manage"],
                        "inst-sync",
                        StatusCode::FORBIDDEN,
                    ),
                ];
                let allowed = cases
                    .iter()
                    .filter(|(_, _, status)| *status == StatusCode::OK)
                    .count();
                for (scope, path, status) in cases {
                    let response = proxy_service(
                        state.clone(),
                        ServiceName::Uplink,
                        path.to_owned(),
                        mutation(scoped_claims("Admin", scope), Method::POST, true, "{}"),
                    )
                    .await;
                    assert_eq!(response.status(), status, "{path}");
                }

                assert_eq!(forwarded(&seen).len(), allowed);
                let rejected = audit
                    .records()
                    .expect("audit records")
                    .iter()
                    .filter(|record| record.outcome() == AuditOutcome::Rejected)
                    .count();
                assert_eq!(rejected, 5);
            }

            #[tokio::test]
            async fn an_unconfirmed_uplink_mutation_is_rejected_and_never_forwarded() {
                let (url, seen) = history_upstream(StatusCode::OK).await;
                let audit = Arc::new(MemoryAuditSink::new());
                let state = app_state_with_audited_upstream(url, audit.clone()).await;

                let response = proxy_service(
                    state,
                    ServiceName::Uplink,
                    "mqtt/reconnect".to_owned(),
                    mutation(claims("Admin"), Method::POST, false, "{}"),
                )
                .await;

                assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);
                assert!(forwarded(&seen).is_empty());
                let records = audit.records().expect("audit records");
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].outcome(), AuditOutcome::Rejected);
                assert_eq!(records[0].capability(), CONFIG);
            }

            #[tokio::test]
            async fn uplink_reads_add_no_audit_record() {
                let (url, seen) = history_upstream(StatusCode::OK).await;
                let audit = Arc::new(MemoryAuditSink::new());
                let state = app_state_with_audited_upstream(url, audit.clone()).await;

                for path in ["mqtt/status", "mqtt/config", "certificate/info"] {
                    let response = proxy_service(
                        state.clone(),
                        ServiceName::Uplink,
                        path.to_owned(),
                        mutation(claims("Viewer"), Method::GET, false, ""),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::OK, "{path}");
                }

                assert_eq!(forwarded(&seen).len(), 3);
                assert!(audit.records().expect("audit records").is_empty());
            }

            #[test]
            fn an_unlisted_uplink_mutation_needs_the_configuration_capability() {
                for (method, path, capability, operation) in ROUTES {
                    let command = uplink_command(path, &method);
                    assert_eq!(command.capability().name(), capability, "{path}");
                    assert_eq!(command.operation(), operation, "{path}");
                }
                for (method, path) in [
                    (Method::PUT, "mqtt/config"),
                    (Method::POST, "certificate/client_key"),
                    (Method::DELETE, "certificate/client_key/extra"),
                    (Method::DELETE, "certificate"),
                    (Method::POST, "alarm/config"),
                    (Method::POST, "anything/else"),
                ] {
                    let command = uplink_command(path, &method);
                    assert_eq!(command.capability().name(), CONFIG, "{path}");
                    assert_eq!(command.operation(), "unrecognized", "{path}");
                }
            }
        }

        #[test]
        fn every_history_mutation_route_has_a_named_audit_operation() {
            use crate::gateway_command_audit::history_command;

            for (method, path, operation) in [
                (Method::PUT, "config", "config.update"),
                (Method::PUT, "storage", "storage.update"),
                (Method::POST, "storage/test", "storage.test"),
                (Method::POST, "storage/reconnect", "storage.reconnect"),
                (Method::DELETE, "storage", "unrecognized"),
                (Method::POST, "data/unknown", "unrecognized"),
            ] {
                assert_eq!(history_command(path, &method).operation(), operation);
            }
        }
    }
}
