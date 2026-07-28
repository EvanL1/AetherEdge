//! Console-first tracing and HTTP access-log middleware for local services.

use std::sync::{Mutex, OnceLock};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;

type EnvFilterReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static LOG_FILTER_HANDLE: OnceLock<EnvFilterReloadHandle> = OnceLock::new();
static CURRENT_LOG_LEVEL: OnceLock<Mutex<String>> = OnceLock::new();

/// Initialize process logging on stdout/stderr. The host runtime owns capture,
/// rotation, retention, and retrieval through journald or container logs.
pub fn init(service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let filter_text = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| format!("info,{service_name}=debug,api_access=info"));
    let filter = EnvFilter::try_new(&filter_text)?;
    let (filter_layer, filter_handle) = reload::Layer::new(filter);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer())
        .try_init()?;

    let _ = LOG_FILTER_HANDLE.set(filter_handle);
    let _ = CURRENT_LOG_LEVEL.set(Mutex::new(filter_text));
    Ok(())
}

/// Change the process log filter at runtime.
pub fn set_log_level(level: &str) -> Result<(), String> {
    let handle = LOG_FILTER_HANDLE
        .get()
        .ok_or_else(|| "Logging system not initialized".to_string())?;
    let filter = EnvFilter::try_new(level)
        .map_err(|error| format!("Invalid log level '{level}': {error}"))?;
    handle
        .reload(filter)
        .map_err(|error| format!("Failed to reload log filter: {error}"))?;
    if let Some(current) = CURRENT_LOG_LEVEL.get()
        && let Ok(mut current) = current.lock()
    {
        *current = level.to_string();
    }
    tracing::info!(%level, "log filter changed");
    Ok(())
}

/// Return the active process log filter.
pub fn get_log_level() -> String {
    CURRENT_LOG_LEVEL
        .get()
        .and_then(|current| current.lock().ok())
        .map(|current| current.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Redact sensitive fields in JSON string
///
/// Recursively searches for sensitive field names and replaces their values with "***REDACTED***".
/// Handles nested objects and arrays.
///
/// # Sensitive Fields
/// - password
/// - token
/// - api_key
/// - secret
/// - authorization
/// - credential
/// - community
/// - complete `parameters` and `logging` containers
///
/// # Example
/// ```rust,ignore
/// let json = r#"{"username":"admin","password":"secret123"}"#;
/// let redacted = redact_sensitive_fields(json);
/// // Result: r#"{"username":"admin","password":"***REDACTED***"}"#
/// ```
#[allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (compile-time safe, never panics)
fn redact_sensitive_fields(json_str: &str) -> String {
    use serde_json::{Value, json};

    const SENSITIVE_KEYS: &[&str] = &[
        "password",
        "token",
        "api_key",
        "secret",
        "authorization",
        "credential",
        "community",
    ];
    const OPAQUE_SENSITIVE_KEYS: &[&str] = &["parameters", "logging"];

    // Try to parse as JSON
    let Ok(mut value) = serde_json::from_str::<Value>(json_str) else {
        // Never copy a malformed payload into logs. It may contain a secret
        // precisely because parsing/redaction failed.
        return "<unparseable json omitted>".to_string();
    };

    // Recursive redaction function
    fn redact_recursive(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    let key_lower = key.to_lowercase();
                    if OPAQUE_SENSITIVE_KEYS.contains(&key_lower.as_str())
                        || SENSITIVE_KEYS.iter().any(|&k| key_lower.contains(k))
                    {
                        // Replace sensitive value with redacted marker
                        *val = json!("***REDACTED***");
                    } else {
                        // Recursively process nested objects/arrays
                        redact_recursive(val);
                    }
                }
            },
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    redact_recursive(item);
                }
            },
            _ => {},
        }
    }

    redact_recursive(&mut value);

    // Serialize back to string (compact format)
    serde_json::to_string(&value)
        .unwrap_or_else(|_| "<json redaction failed; body omitted>".to_string())
}

/// Truncate body string to maximum length
///
/// If the body exceeds max_length, it will be truncated and a suffix will be added
/// indicating how many bytes were truncated.
///
/// # Example
/// ```rust,ignore
/// let long_body = "a".repeat(1000);
/// let truncated = truncate_body(&long_body, 500);
/// // Result: "aaa...aaa[truncated 500 bytes]"
/// ```
fn truncate_body(body: &str, max_length: usize) -> String {
    if body.len() <= max_length {
        body.to_string()
    } else {
        // `max_length` is a byte budget, but slicing Rust strings requires a
        // UTF-8 character boundary. Walk back by at most three bytes so a
        // legitimate JSON body containing CJK text or emoji can never panic
        // (the workspace release profile aborts the whole service on panic).
        let mut boundary = max_length;
        while boundary > 0 && !body.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let truncated_bytes = body.len() - boundary;
        format!("{}[truncated {} bytes]", &body[..boundary], truncated_bytes)
    }
}

/// Returns true for request families whose payload values must never enter
/// access logs, even after generic field-name redaction.
fn request_body_logging_forbidden(path: &str) -> bool {
    path == "/api/channels" || path.starts_with("/api/channels/")
}

/// HTTP API request logger middleware
///
/// Provides selective HTTP request logging with request body recording:
/// - **INFO level**: Logs only POST/PUT/PATCH/DELETE requests (no body)
/// - **DEBUG level**: Logs all requests with body content (truncated & redacted)
///
/// Logs use the `api_access` target on the process console stream.
///
/// # Design Decisions
///
/// - **Body Recording at DEBUG**: Request body is only read and logged at DEBUG level
/// - **Sensitive Field Redaction**: password, token, api_key, secret, authorization are filtered
/// - **Body Truncation**: Body limited to 500 characters to prevent log bloat
/// - **Simplified Fields**: Removed redundant headers (user_agent, content_type, content_length, is_error)
/// - **No Duplicate Logging**: INFO and DEBUG levels are mutually exclusive
///
/// # Logged Information
/// - HTTP method (POST, GET, PUT, DELETE, PATCH)
/// - Request path (e.g., `/api/channels`, `/api/instances`)
/// - HTTP status code (e.g., 200, 404, 500)
/// - Response duration in milliseconds
/// - Request body (DEBUG only, truncated to 500 chars, sensitive fields redacted)
///
/// # Example Log Output
///
/// INFO level:
/// ```text
/// INFO  HTTP request method=POST path=/api/instances status=200 duration_ms=15
/// INFO  HTTP request method=PUT path=/api/channels/1 status=200 duration_ms=23
/// ```
///
/// DEBUG level:
/// ```text
/// DEBUG HTTP request method=POST path=/api/instances status=200 duration_ms=15 request_body={"instance_name":"test","properties":{...}}[truncated 234 bytes]
/// DEBUG HTTP request method=GET path=/health status=200 duration_ms=5 request_body=-
/// DEBUG HTTP request method=POST path=/api/auth/login status=200 duration_ms=50 request_body={"username":"admin","password":"***REDACTED***"}
/// ```
///
/// # Usage
///
/// Add this middleware to your Axum router **before** `.with_state()`:
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use common::logging::http_request_logger;
///
/// let app = Router::new()
///     // ... routes ...
///     .layer(middleware::from_fn(http_request_logger))  // BEFORE .with_state()
///     .with_state(state);
/// ```
pub async fn http_request_logger(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use std::time::Instant;
    use tracing::{Level, debug, info, level_enabled};

    const MAX_BODY_LENGTH: usize = 500;
    const MAX_BODY_READ: usize = 2048;

    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    // Channel payloads contain protocol credentials and per-channel log paths.
    // Their application contract forbids recording parameter/logging values,
    // so the access logger never captures any body below this route prefix --
    // including malformed JSON that cannot be structurally redacted.
    let body_logging_forbidden = request_body_logging_forbidden(uri.path());

    // Decide whether to read body (DEBUG + modifying method + known-small JSON
    // only). An absent Content-Length may be chunked; do not consume it merely
    // for diagnostics because a bounded read cannot reconstruct an oversized
    // stream after failure.
    let should_read_body = level_enabled!(Level::DEBUG)
        && !body_logging_forbidden
        && matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
        && req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"))
        && req
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|length| length <= MAX_BODY_READ);

    let (req, body_str) = if should_read_body {
        extract_request_body(req, MAX_BODY_READ, MAX_BODY_LENGTH).await
    } else {
        (req, "-".to_string())
    };

    let response = next.run(req).await;
    let duration = start.elapsed();
    let status = response.status();

    // INFO: modifying methods (no body); DEBUG: all requests with body
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        info!(
            target: "api_access",
            method = %method,
            path = %uri.path(),
            status = %status.as_u16(),
            duration_ms = %duration.as_millis(),
            "HTTP request"
        );
    }

    if body_str != "-" {
        debug!(
            target: "api_access",
            method = %method,
            path = %uri.path(),
            status = %status.as_u16(),
            duration_ms = %duration.as_millis(),
            request_body = %body_str,
            "HTTP request (detailed)"
        );
    } else if !matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        debug!(
            target: "api_access",
            method = %method,
            path = %uri.path(),
            status = %status.as_u16(),
            duration_ms = %duration.as_millis(),
            "HTTP request"
        );
    }

    response
}

/// Extract and redact request body for logging.
///
/// Reads the body, applies sensitive field redaction, truncates to max length,
/// and reconstructs the request with the original bytes.
async fn extract_request_body(
    req: axum::extract::Request,
    max_read: usize,
    max_display: usize,
) -> (axum::extract::Request, String) {
    use axum::body::Body;

    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, max_read).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Body read failure: {}", e);
            let new_req = axum::extract::Request::from_parts(parts, Body::empty());
            return (new_req, "-".to_string());
        },
    };

    let body_str = match std::str::from_utf8(&bytes) {
        Ok(s) => truncate_body(&redact_sensitive_fields(s), max_display),
        Err(_) => "<binary data>".to_string(),
    };

    let new_req = axum::extract::Request::from_parts(parts, Body::from(bytes));
    (new_req, body_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_json_fields_are_redacted() {
        let redacted = redact_sensitive_fields(
            r#"{"password":"secret","nested":{"api_key":"key"},"name":"device"}"#,
        );
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains(":\"key\""));
        assert!(redacted.contains("device"));
    }

    #[test]
    fn malformed_json_is_omitted() {
        assert_eq!(
            redact_sensitive_fields("token=secret"),
            "<unparseable json omitted>"
        );
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        let output = truncate_body("设备数据", 5);
        assert!(output.starts_with("设"));
        assert!(output.contains("truncated"));
    }

    #[test]
    fn channel_payloads_are_never_logged() {
        assert!(request_body_logging_forbidden("/api/channels"));
        assert!(request_body_logging_forbidden("/api/channels/7"));
        assert!(!request_body_logging_forbidden("/api/instances"));
    }
}
