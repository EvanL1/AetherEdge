//! HTTP Protocol Adapter
//!
//! Data collection via HTTP REST API polling with JSONPath mapping.
//!
//! ## Design Overview
//!
//! The adapter polls device REST APIs and maps JSON responses into acquisition
//! samples. Incoming webhook hosting is intentionally outside the IO runtime.
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "mode": "polling",
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

use async_trait::async_trait;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tracing::{debug, info};

use crate::protocols::core::data::DataBatch;
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::json_mapper::{JsonMapper, JsonMappingConfig};
use crate::protocols::core::traits::{ConnectionState, DataEventReceiver, Diagnostics, PollResult};
use crate::protocols::gateway::ChannelRuntime;

/// HTTP method for requests
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    GET,
    POST,
    PUT,
}

impl From<HttpMethod> for Method {
    fn from(m: HttpMethod) -> Self {
        match m {
            HttpMethod::GET => Method::GET,
            HttpMethod::POST => Method::POST,
            HttpMethod::PUT => Method::PUT,
        }
    }
}

/// HTTP channel parameters (from database config JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpParamsConfig {
    /// Target URL for polling mode
    #[serde(default)]
    pub url: Option<String>,

    /// HTTP method for polling
    #[serde(default)]
    pub method: HttpMethod,

    /// Request headers
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Request body (for POST/PUT)
    #[serde(default)]
    pub body: Option<String>,

    /// Polling interval in milliseconds.
    #[serde(default = "default_interval_ms", alias = "interval_ms")]
    pub poll_interval_ms: u64,

    /// Request timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    /// JSON mapping configuration
    #[serde(default)]
    pub json_mapping: JsonMappingConfig,
}

fn default_interval_ms() -> u64 {
    5000
}

fn default_timeout_ms() -> u64 {
    3000
}

impl Default for HttpParamsConfig {
    fn default() -> Self {
        Self {
            url: None,
            method: HttpMethod::GET,
            headers: HashMap::new(),
            body: None,
            poll_interval_ms: default_interval_ms(),
            timeout_ms: default_timeout_ms(),
            json_mapping: JsonMappingConfig::default(),
        }
    }
}

impl HttpParamsConfig {
    /// Validate the complete polling contract before desired state commits.
    pub fn validate(&self) -> Result<()> {
        let url = self
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| GatewayError::Config("HTTP polling URL must be nonblank".into()))?;
        HttpChannel::validate_url(url)?;
        for (name, value) in [
            ("poll_interval_ms", self.poll_interval_ms),
            ("timeout_ms", self.timeout_ms),
        ] {
            if !(1..=86_400_000).contains(&value) {
                return Err(GatewayError::Config(format!(
                    "HTTP {name} must be between 1 and 86400000"
                )));
            }
        }
        Ok(())
    }

    /// Convert to runtime configuration
    pub fn to_config(&self) -> HttpConfig {
        HttpConfig {
            url: self.url.clone(),
            method: self.method,
            headers: self.headers.clone(),
            body: self.body.clone(),
            timeout: Duration::from_millis(self.timeout_ms),
        }
    }
}

/// HTTP runtime configuration
#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub url: Option<String>,
    pub method: HttpMethod,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub timeout: Duration,
}

/// Build an HTTP request with configured method, headers, and optional body.
fn build_request(client: &Client, config: &HttpConfig, url: &str) -> reqwest::RequestBuilder {
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

/// Guard against oversized responses to prevent OOM (max 10MB).
fn check_response_size(response: &reqwest::Response) -> Result<()> {
    if let Some(len) = response.content_length()
        && len > 10 * 1024 * 1024
    {
        return Err(GatewayError::Protocol(format!(
            "Response too large: {len} bytes (max 10MB)"
        )));
    }
    Ok(())
}

/// HTTP Channel implementation (Polling mode)
///
/// Polls a device REST API at configured intervals and extracts
/// data points from JSON responses using JSONPath mappings.
pub struct HttpChannel {
    /// Channel configuration
    config: HttpConfig,
    /// Channel ID
    channel_id: u32,
    /// Channel name
    name: String,
    /// JSON mapper compiled from the channel's physical topology generation.
    mapper: Arc<JsonMapper>,
    /// HTTP client
    client: Option<Client>,
    /// Connection state
    state: AtomicU8,
    /// Diagnostics
    diagnostics: Arc<AtomicDiagnostics>,
    /// Consecutive failure count
    consecutive_failures: std::sync::atomic::AtomicU32,
}

impl HttpChannel {
    /// Create a new HTTP channel
    pub fn new(config: HttpConfig, channel_id: u32, name: String, mapper: Arc<JsonMapper>) -> Self {
        Self {
            config,
            channel_id,
            name,
            mapper,
            client: None,
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Set connection state
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
    }

    /// Create HTTP client
    fn create_client(&self) -> Result<Client> {
        Client::builder()
            .timeout(self.config.timeout)
            .build()
            .map_err(|e| GatewayError::Protocol(format!("Failed to create HTTP client: {e}")))
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

        // Block well-known internal hostnames
        if host_lower == "localhost" || host_lower == "0.0.0.0" {
            return Err(GatewayError::Config(format!(
                "SSRF protection: blocked request to internal address '{host}'"
            )));
        }

        // IP-based checks: block loopback and link-local addresses
        // Note: RFC 1918 private addresses are allowed (industrial devices live there)
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            let is_blocked = match ip {
                std::net::IpAddr::V4(v4) => {
                    v4.is_loopback()       // 127.0.0.0/8
                    || v4.is_link_local()  // 169.254.0.0/16
                    || v4.is_unspecified() // 0.0.0.0
                },
                std::net::IpAddr::V6(v6) => {
                    v6.is_loopback()       // ::1
                    || v6.is_unspecified() // ::
                },
            };
            if is_blocked {
                return Err(GatewayError::Config(format!(
                    "SSRF protection: blocked request to internal address '{host}'"
                )));
            }
        }

        Ok(())
    }

    /// Execute a single poll request
    async fn poll_once_internal(&self) -> Result<DataBatch> {
        let url = self.config.url.as_ref().ok_or_else(|| {
            GatewayError::Config("No URL configured for HTTP polling".to_string())
        })?;
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
        check_response_size(&response)?;

        let body = response
            .bytes()
            .await
            .map_err(|e| GatewayError::Protocol(format!("Failed to read response body: {e}")))?;

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
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "http"
    }

    fn is_event_driven(&self) -> bool {
        false
    }

    async fn connect(&mut self) -> Result<()> {
        if self.client.is_some() {
            return Ok(());
        }

        self.set_state(ConnectionState::Connecting);

        // SSRF protection: validate URL once at connect time
        if let Some(url) = &self.config.url {
            Self::validate_url(url)?;
        }

        // Create HTTP client
        let client = self.create_client()?;
        self.client = Some(client);

        self.set_state(ConnectionState::Connected);

        info!(
            channel_id = self.channel_id,
            url = ?self.config.url,
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
                self.consecutive_failures.store(0, Ordering::SeqCst);
                self.diagnostics.add_read(batch.len() as u64);
                PollResult::success(batch)
            },
            Err(e) => {
                // Protocol-level error (not point-level), return empty result
                // Error is already recorded in diagnostics
                self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
                self.diagnostics.record_error(e.to_string());
                PollResult::success(DataBatch::new())
            },
        }
    }

    async fn write_control(&mut self, _commands: &[(u32, f64)]) -> Result<usize> {
        // HTTP polling is typically read-only
        // Control would require POSTing to device-specific endpoints
        Err(GatewayError::Protocol(
            "HTTP channel does not support control commands".to_string(),
        ))
    }

    async fn write_adjustment(&mut self, _adjustments: &[(u32, f64)]) -> Result<usize> {
        Err(GatewayError::Protocol(
            "HTTP channel does not support adjustment commands".to_string(),
        ))
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        None
    }

    async fn start_events(&mut self) -> Result<()> {
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        Ok(())
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
            .field("name", &self.name)
            .field("url", &self.config.url)
            .field("state", &self.connection_state())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_http_params_default() {
        let params = HttpParamsConfig::default();
        assert!(params.url.is_none());
        assert_eq!(params.method, HttpMethod::GET);
        assert!(params.headers.is_empty());
    }

    #[test]
    fn test_http_params_deserialize() {
        let json = r#"{
            "mode": "polling",
            "url": "http://192.168.1.100/api/data",
            "method": "GET",
            "headers": {"Authorization": "Bearer xxx"},
            "interval_ms": 5000,
            "timeout_ms": 3000
        }"#;

        let params: HttpParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            params.url,
            Some("http://192.168.1.100/api/data".to_string())
        );
        assert_eq!(params.poll_interval_ms, 5000);
        assert!(params.headers.contains_key("Authorization"));
    }
    #[test]
    fn test_http_method_deserialize() {
        assert_eq!(
            serde_json::from_str::<HttpMethod>(r#""GET""#).unwrap(),
            HttpMethod::GET
        );
        assert_eq!(
            serde_json::from_str::<HttpMethod>(r#""POST""#).unwrap(),
            HttpMethod::POST
        );
    }
}
