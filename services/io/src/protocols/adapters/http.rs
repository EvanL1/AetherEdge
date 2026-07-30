//! HTTP Protocol Adapter
//!
//! Data collection via HTTP REST API polling with JSONPath mapping.
//!
//! The unified channel task owns the polling loop and reconnect policy. This
//! adapter performs one bounded request per `poll_once` call.
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "url": "http://192.168.1.100/api/data",
//!   "method": "GET",
//!   "headers": {"Authorization": "Bearer xxx"},
//!   "poll_interval_ms": 5000,
//!   "timeout_ms": 3000,
//!   "json_mapping": {
//!     "timestamp_path": "$.timestamp"
//!   }
//! }
//! ```

use aether_config::io::MAX_CHANNEL_TIMING_MS;
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tracing::{debug, info};

use crate::core::channels::RuntimeChannelConfig;
use crate::protocols::ChannelRuntime;
use crate::protocols::adapters::json_mapper::{JsonMapper, JsonMappingConfig};
use crate::protocols::core::data::DataBatch;
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::traits::{ConnectionState, Diagnostics, PollResult};

/// HTTP method for requests
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
}

impl From<HttpMethod> for Method {
    fn from(m: HttpMethod) -> Self {
        match m {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Put => Method::PUT,
        }
    }
}

/// HTTP channel parameters (from database config JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HttpParamsConfig {
    /// Target URL
    url: String,

    /// HTTP method for polling
    #[serde(default)]
    method: HttpMethod,

    /// Request headers
    #[serde(default)]
    headers: HashMap<String, String>,

    /// Request body (for POST/PUT)
    #[serde(default)]
    body: Option<String>,

    /// Request timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,

    /// JSON mapping configuration
    #[serde(default)]
    json_mapping: JsonMappingConfig,
}

fn default_timeout_ms() -> u64 {
    3000
}

impl HttpParamsConfig {
    fn blocked_ip(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
            },
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unicast_link_local()
                    || v6.is_unspecified()
                    || v6
                        .to_ipv4_mapped()
                        .is_some_and(|v4| Self::blocked_ip(v4.into()))
            },
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(GatewayError::Config(
                "HTTP url must be a non-empty string".to_string(),
            ));
        }
        Self::validate_url(url)?;
        if !(1..=MAX_CHANNEL_TIMING_MS).contains(&self.timeout_ms) {
            return Err(GatewayError::Config(format!(
                "HTTP timeout_ms must be between 1 and {MAX_CHANNEL_TIMING_MS} milliseconds"
            )));
        }
        for (name, value) in &self.headers {
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                GatewayError::Config(format!("Invalid HTTP header name '{name}': {error}"))
            })?;
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()).map_err(|error| {
                GatewayError::Config(format!("Invalid HTTP header value for '{name}': {error}"))
            })?;
        }
        Ok(())
    }

    /// Validate URL to prevent SSRF attacks targeting internal services.
    ///
    /// Blocks loopback, link-local, and unspecified addresses. RFC 1918 private
    /// addresses (10.x, 172.16-31.x, 192.168.x) are intentionally ALLOWED because
    /// this is an industrial gateway that communicates with devices on private networks.
    fn validate_url(url: &str) -> Result<()> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| GatewayError::Config(format!("Invalid URL: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| GatewayError::Config("URL has no host".into()))?;
        let host_lower = host.to_lowercase();
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(GatewayError::Config(
                "HTTP url must use the http or https scheme".to_string(),
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(GatewayError::Config(
                "HTTP URL credentials must be supplied through request headers".to_string(),
            ));
        }
        if parsed.fragment().is_some() {
            return Err(GatewayError::Config(
                "HTTP URL fragments are not sent to devices and are therefore unsupported"
                    .to_string(),
            ));
        }

        // Block well-known loopback aliases as well as the RFC 6761
        // localhost namespace. Private-LAN device names remain allowed.
        if host_lower == "localhost"
            || host_lower.ends_with(".localhost")
            || matches!(
                host_lower.as_str(),
                "localhost.localdomain"
                    | "localhost6"
                    | "localhost6.localdomain6"
                    | "ip6-localhost"
                    | "ip6-loopback"
            )
        {
            return Err(GatewayError::Config(format!(
                "SSRF protection: blocked request to internal address '{host}'"
            )));
        }

        // Industrial devices commonly use RFC 1918 addresses, so only
        // loopback, link-local, and unspecified IPs are rejected.
        let ip_literal = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if let Ok(ip) = ip_literal.parse::<std::net::IpAddr>()
            && Self::blocked_ip(ip)
        {
            return Err(GatewayError::Config(format!(
                "SSRF protection: blocked request to internal address '{host}'"
            )));
        }

        Ok(())
    }
}

/// Build an HTTP request with configured method, headers, and optional body.
fn build_request(client: &Client, config: &HttpParamsConfig, url: &str) -> reqwest::RequestBuilder {
    let mut request = client.request(config.method.into(), url);
    for (key, value) in &config.headers {
        request = request.header(key.as_str(), value.as_str());
    }
    if let Some(body) = &config.body {
        request = request
            .header("Content-Type", "application/json")
            .body(body.clone());
    }
    request
}

const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Read a response with a hard limit even when the peer omits Content-Length.
async fn read_bounded_body(response: reqwest::Response) -> Result<Bytes> {
    if let Some(len) = response.content_length()
        && len > MAX_RESPONSE_BYTES as u64
    {
        return Err(GatewayError::Protocol(format!(
            "Response too large: {len} bytes (max {MAX_RESPONSE_BYTES} bytes)"
        )));
    }

    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            GatewayError::Protocol(format!("Failed to read response body: {error}"))
        })?;
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| GatewayError::Protocol("HTTP response size overflow".to_string()))?;
        if next_len > MAX_RESPONSE_BYTES {
            return Err(GatewayError::Protocol(format!(
                "Response exceeds {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

/// HTTP Channel implementation (Polling mode)
///
/// Polls a device REST API at configured intervals and extracts
/// data points from JSON responses using JSONPath mappings.
pub(crate) struct HttpChannel {
    /// Channel configuration
    config: HttpParamsConfig,
    /// Channel ID
    channel_id: u32,
    /// JSON mapper compiled from the immutable runtime snapshot
    mapper: JsonMapper,
    /// HTTP client
    client: Option<Client>,
    /// Connection state
    state: AtomicU8,
    /// Diagnostics
    diagnostics: Arc<AtomicDiagnostics>,
}

impl HttpChannel {
    /// Create a new HTTP channel from one complete runtime snapshot.
    pub(crate) fn new(config: HttpParamsConfig, runtime: &RuntimeChannelConfig) -> Result<Self> {
        config.validate()?;
        let mapper = JsonMapper::from_runtime_config(runtime)?.with_config(&config.json_mapping)?;

        info!(
            channel_id = runtime.id(),
            mapping_count = mapper.len(),
            "Compiled HTTP JSON mappings"
        );

        Ok(Self {
            config,
            channel_id: runtime.id(),
            mapper,
            client: None,
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
        })
    }

    /// Set connection state
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
    }

    /// Create HTTP client
    fn create_client(&self) -> Result<Client> {
        Client::builder()
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| GatewayError::Protocol(format!("Failed to create HTTP client: {e}")))
    }

    /// Execute a single poll request
    async fn poll_once_internal(&self) -> Result<DataBatch> {
        let url = &self.config.url;
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| GatewayError::Protocol("HTTP client not initialized".to_string()))?;
        let mapper = &self.mapper;

        let response = build_request(client, &self.config, url)
            .send()
            .await
            .map_err(|e| GatewayError::Protocol(format!("HTTP request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(GatewayError::Protocol(format!(
                "HTTP request returned {}: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            )));
        }
        let body = read_bounded_body(response).await?;

        let batch = mapper.parse(&body)?;
        debug!(
            channel_id = self.channel_id,
            url = %url,
            points = batch.len(),
            "HTTP poll completed"
        );
        Ok(batch)
    }
}

#[async_trait]
impl ChannelRuntime for HttpChannel {
    async fn connect(&mut self) -> Result<()> {
        if self.client.is_some() {
            return Ok(());
        }

        self.set_state(ConnectionState::Connecting);

        // Create HTTP client
        let client = self.create_client()?;
        self.client = Some(client);

        self.set_state(ConnectionState::Connected);

        info!(
            channel_id = self.channel_id,
            url = %self.config.url,
            "HTTP channel connected"
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.client = None;
        self.set_state(ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "HTTP channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        match self.poll_once_internal().await {
            Ok(batch) => {
                self.diagnostics.add_read(batch.len() as u64);
                self.set_state(ConnectionState::Connected);
                PollResult::success(batch)
            },
            Err(e) => {
                self.diagnostics.record_error(e.to_string());
                self.client = None;
                self.set_state(ConnectionState::Error);
                PollResult::success(DataBatch::new())
            },
        }
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "http".to_string(),
            connection_state: self.connection_state(),
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: Default::default(),
        })
    }

    fn connection_state(&self) -> ConnectionState {
        ConnectionState::from(self.state.load(Ordering::SeqCst))
    }
}

impl std::fmt::Debug for HttpChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpChannel")
            .field("channel_id", &self.channel_id)
            .field("url", &self.config.url)
            .field("state", &self.connection_state())
            .finish()
    }
}

use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

impl HasMetadata for HttpChannel {
    #[allow(clippy::disallowed_methods)]
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "http",
            display_name: "HTTP JSON Polling",
            description: "Read-only HTTP polling with point-owned JSONPath mappings.",
            is_recommended: true,
            example_config: serde_json::json!({
                "url": "http://192.168.1.100/api/data",
                "method": "GET",
                "poll_interval_ms": 5000,
                "timeout_ms": 3000
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "url",
                    "URL",
                    "HTTP or HTTPS endpoint polled for JSON data",
                    ParameterType::String,
                )
                .with_min_length(1),
                ParameterMetadata::optional(
                    "method",
                    "Method",
                    "HTTP request method: GET, POST, or PUT",
                    ParameterType::String,
                    serde_json::json!("GET"),
                ),
                ParameterMetadata::optional(
                    "headers",
                    "Headers",
                    "Request headers",
                    ParameterType::Object,
                    serde_json::json!({}),
                ),
                ParameterMetadata::optional(
                    "body",
                    "Body",
                    "Optional request body for POST or PUT",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "timeout_ms",
                    "Request Timeout (ms)",
                    "Timeout for one HTTP request",
                    ParameterType::Integer,
                    serde_json::json!(3000),
                )
                .with_integer_range(1, 86_400_000),
                ParameterMetadata::optional(
                    "json_mapping",
                    "JSON Mapping",
                    "Optional source timestamp JSONPath settings",
                    ParameterType::Object,
                    serde_json::json!({}),
                ),
            ],
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};

    fn runtime_snapshot(parameters: HashMap<String, serde_json::Value>) -> RuntimeChannelConfig {
        RuntimeChannelConfig::from_base(ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "http-test".to_string(),
                description: None,
                protocol: "http".to_string(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        })
    }

    #[test]
    fn test_http_params_deserialize() {
        let json = r#"{
            "url": "http://192.168.1.100/api/data",
            "method": "GET",
            "headers": {"Authorization": "Bearer xxx"},
            "timeout_ms": 3000
        }"#;

        let params: HttpParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.url, "http://192.168.1.100/api/data");
        assert_eq!(params.timeout_ms, 3000);
        assert!(params.headers.contains_key("Authorization"));
    }

    #[test]
    fn http_params_require_url_and_reject_retired_mode() {
        assert!(serde_json::from_str::<HttpParamsConfig>(r#"{"method":"GET"}"#).is_err());
        assert!(
            serde_json::from_str::<HttpParamsConfig>(
                r#"{"url":"http://192.0.2.10/data","mode":"polling"}"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<HttpParamsConfig>(
                r#"{"url":"http://192.0.2.10/data","mode":"webhook"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn test_http_method_deserialize() {
        assert_eq!(
            serde_json::from_str::<HttpMethod>(r#""GET""#).unwrap(),
            HttpMethod::Get
        );
        assert_eq!(
            serde_json::from_str::<HttpMethod>(r#""POST""#).unwrap(),
            HttpMethod::Post
        );
    }

    #[test]
    fn webhook_and_retired_polling_fields_fail_before_connect() {
        for (key, value) in [
            ("mode", serde_json::json!("webhook")),
            ("listen_path", serde_json::json!("/hooks/device")),
            ("interval_ms", serde_json::json!(5000)),
            ("max_retries", serde_json::json!(3)),
        ] {
            let mut parameters = serde_json::json!({
                "url": "http://192.0.2.10/data"
            });
            parameters
                .as_object_mut()
                .unwrap()
                .insert(key.to_string(), value);
            assert!(serde_json::from_value::<HttpParamsConfig>(parameters).is_err());
        }
    }

    #[test]
    fn invalid_url_fails_before_connect() {
        let mut parameters = HashMap::new();
        parameters.insert("url".to_string(), serde_json::json!("file:///etc/passwd"));
        let runtime = runtime_snapshot(parameters);
        let config = HttpParamsConfig {
            url: "file:///etc/passwd".to_string(),
            method: HttpMethod::Get,
            headers: HashMap::new(),
            body: None,
            timeout_ms: 3000,
            json_mapping: JsonMappingConfig::default(),
        };

        let error = HttpChannel::new(config, &runtime).expect_err("unsupported scheme");
        assert!(
            error.to_string().contains("URL has no host")
                || error.to_string().contains("http or https")
        );
    }

    #[test]
    fn ssrf_validation_blocks_ipv6_and_localhost_aliases() {
        for url in [
            "http://127.0.0.1/data",
            "http://169.254.1.2/data",
            "http://0.0.0.0/data",
            "http://[::1]/data",
            "http://[::]/data",
            "http://[fe80::1]/data",
            "http://[::ffff:127.0.0.1]/data",
            "http://device.localhost/data",
            "http://localhost.localdomain/data",
            "http://ip6-localhost/data",
            "http://ip6-loopback/data",
        ] {
            assert!(HttpParamsConfig::validate_url(url).is_err(), "{url}");
        }

        for url in [
            "http://192.168.1.10/data",
            "http://10.0.0.20/data",
            "http://[fd00::20]/data",
            "https://device.internal/data",
        ] {
            assert!(HttpParamsConfig::validate_url(url).is_ok(), "{url}");
        }
    }
}
