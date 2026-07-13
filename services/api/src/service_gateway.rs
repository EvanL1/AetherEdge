use std::sync::Arc;

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, Method, Request, Response, StatusCode, header},
    response::IntoResponse,
    routing::any,
};
use serde_json::json;

use crate::config::GatewayConfig;
use crate::state::AppState;

const MAX_GATEWAY_BODY_BYTES: usize = 16 * 1024 * 1024;

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

    fn base_url(self, config: &GatewayConfig) -> &str {
        match self {
            Self::Io => &config.io_service_url,
            Self::Automation => &config.automation_service_url,
            Self::History => &config.history_service_url,
            Self::Uplink => &config.uplink_service_url,
            Self::Alarm => &config.alarm_service_url,
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
    request: Request<Body>,
) -> Response<Body> {
    if validate_relative_path(&path).is_err() {
        return gateway_error(
            StatusCode::BAD_REQUEST,
            "INVALID_SERVICE_PATH",
            "the internal application path is invalid",
        );
    }
    forward_to_upstream(
        &state.service_client,
        service.base_url(&state.config),
        &service.downstream_path(&path),
        request,
    )
    .await
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
) -> Response<Body> {
    if !matches!(
        *request.method(),
        Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE | Method::HEAD
    ) {
        return gateway_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "the method is not supported by the application gateway",
        );
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

    use super::{ServiceName, forward_to_upstream, validate_relative_path};

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
}
