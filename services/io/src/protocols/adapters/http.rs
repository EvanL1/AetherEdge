//! HTTP Protocol Adapter
//!
//! Data collection via HTTP REST API polling with JSONPath mapping.
//!
//! ## Design Overview
//!
//! Two operating modes:
//! - **Polling**: io actively fetches data from device REST APIs
//! - **Webhook**: Device pushes data to io (requires API route integration)
//!
//! This file implements Polling mode. Webhook mode requires integration
//! with the io API server and is handled separately.
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "mode": "polling",
//!   "url": "http://192.168.1.100/api/data",
//!   "method": "GET",
//!   "headers": {"Authorization": "Bearer xxx"},
//!   "interval_ms": 5000,
//!   "timeout_ms": 3000,
//!   "json_mapping": {
//!     "timestamp_path": "$.timestamp"
//!   }
//! }
//! ```

use async_trait::async_trait;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info, trace};

use crate::protocols::ChannelRuntime;
use crate::protocols::core::data::DataBatch;
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::json_mapper::{JsonMapper, JsonMappingConfig};
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
};

/// HTTP channel operating mode
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMode {
    /// Polling mode: io fetches data at intervals
    #[default]
    Polling,
    /// Webhook mode: device pushes data to io
    Webhook,
}

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
    /// Operating mode
    #[serde(default)]
    pub mode: HttpMode,

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

    /// Polling interval in milliseconds
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,

    /// Request timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    /// JSON mapping configuration
    #[serde(default)]
    pub json_mapping: JsonMappingConfig,

    // === Webhook mode specific ===
    /// Webhook listen path (for webhook mode)
    #[serde(default)]
    pub listen_path: Option<String>,

    /// Authentication token for webhook validation
    #[serde(default)]
    pub auth_token: Option<String>,

    /// Maximum retries on failure
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Retry delay in milliseconds
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,
}

fn default_interval_ms() -> u64 {
    5000
}

fn default_timeout_ms() -> u64 {
    3000
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_delay_ms() -> u64 {
    1000
}

impl Default for HttpParamsConfig {
    fn default() -> Self {
        Self {
            mode: HttpMode::Polling,
            url: None,
            method: HttpMethod::GET,
            headers: HashMap::new(),
            body: None,
            interval_ms: default_interval_ms(),
            timeout_ms: default_timeout_ms(),
            json_mapping: JsonMappingConfig::default(),
            listen_path: None,
            auth_token: None,
            max_retries: default_max_retries(),
            retry_delay_ms: default_retry_delay_ms(),
        }
    }
}

impl HttpParamsConfig {
    /// Convert to runtime configuration
    pub fn to_config(&self) -> HttpConfig {
        HttpConfig {
            mode: self.mode,
            url: self.url.clone(),
            method: self.method,
            headers: self.headers.clone(),
            body: self.body.clone(),
            interval: Duration::from_millis(self.interval_ms),
            timeout: Duration::from_millis(self.timeout_ms),
            json_mapping: self.json_mapping.clone(),
            listen_path: self.listen_path.clone(),
            auth_token: self.auth_token.clone(),
            max_retries: self.max_retries,
            retry_delay: Duration::from_millis(self.retry_delay_ms),
        }
    }
}

/// HTTP runtime configuration
#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub mode: HttpMode,
    pub url: Option<String>,
    pub method: HttpMethod,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub interval: Duration,
    pub timeout: Duration,
    pub json_mapping: JsonMappingConfig,
    pub listen_path: Option<String>,
    pub auth_token: Option<String>,
    pub max_retries: u32,
    pub retry_delay: Duration,
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
    /// JSON mapper (loaded from database)
    mapper: Option<Arc<JsonMapper>>,
    /// HTTP client
    client: Option<Client>,
    /// Polling task handle
    poll_task_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection state
    state: AtomicU8,
    /// Event broadcast sender (for webhook mode or optional polling events)
    event_tx: DataEventSender,
    /// Diagnostics
    diagnostics: Arc<AtomicDiagnostics>,
    /// Database pool for loading mappings
    db_pool: Option<SqlitePool>,
    /// Consecutive failure count
    consecutive_failures: std::sync::atomic::AtomicU32,
}

impl HttpChannel {
    /// Create a new HTTP channel
    pub fn new(config: HttpConfig, channel_id: u32, name: String) -> Self {
        let (event_tx, _) = broadcast::channel(256);

        Self {
            config,
            channel_id,
            name,
            mapper: None,
            client: None,
            poll_task_handle: None,
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            event_tx,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            db_pool: None,
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Load JSON mappings from database
    async fn load_mapper(&mut self) -> Result<()> {
        if self.mapper.is_some() {
            return Ok(());
        }

        let pool = self.db_pool.as_ref().ok_or_else(|| {
            GatewayError::Config("Database pool not set for HTTP channel".to_string())
        })?;

        let mapper = JsonMapper::from_database(pool, self.channel_id)
            .await?
            .with_config(&self.config.json_mapping)?;

        info!(
            channel_id = self.channel_id,
            mapping_count = mapper.len(),
            "Loaded HTTP JSON mappings"
        );

        self.mapper = Some(Arc::new(mapper));
        Ok(())
    }

    /// Set connection state
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
        if let Err(e) = self.event_tx.send(DataEvent::ConnectionChanged(state)) {
            trace!("No subscribers for ConnectionChanged event: {e}");
        }
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
        let mapper = self
            .mapper
            .as_ref()
            .ok_or_else(|| GatewayError::Config("JSON mapper not configured".to_string()))?;

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

    /// Run the polling loop (for background polling mode)
    async fn run_poll_loop(
        channel_id: u32,
        client: Client,
        config: HttpConfig,
        mapper: Arc<JsonMapper>,
        state: Arc<AtomicU8>,
        event_tx: DataEventSender,
        diagnostics: Arc<AtomicDiagnostics>,
    ) {
        let url = match &config.url {
            Some(u) => u.clone(),
            None => {
                error!(channel_id, "No URL configured for HTTP polling");
                return;
            },
        };

        info!(
            channel_id,
            url = %url,
            interval_ms = config.interval.as_millis(),
            "HTTP polling loop started"
        );

        let mut interval = tokio::time::interval(config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut consecutive_failures = 0u32;

        loop {
            interval.tick().await;

            let response = match build_request(&client, &config, &url).send().await {
                Ok(r) => r,
                Err(e) => {
                    consecutive_failures += 1;
                    debug!(channel_id, error = %e, "HTTP request failed");
                    diagnostics.record_error(e.to_string());
                    if consecutive_failures >= config.max_retries && config.max_retries > 0 {
                        state.store(ConnectionState::Error as u8, Ordering::SeqCst);
                        let _ = event_tx.send(DataEvent::ConnectionChanged(ConnectionState::Error));
                        let _ = event_tx.send(DataEvent::Error(e.to_string()));
                    }
                    continue;
                },
            };

            if !response.status().is_success() {
                consecutive_failures += 1;
                let status = response.status().as_u16();
                debug!(channel_id, status, "HTTP request returned error status");
                diagnostics.record_error(format!("HTTP status {status}"));
                continue;
            }

            if let Some(len) = response.content_length()
                && len > 10 * 1024 * 1024
            {
                diagnostics.record_error(format!("Response too large: {len} bytes (max 10MB)"));
                consecutive_failures += 1;
                continue;
            }

            let body = match response.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    debug!(channel_id, error = %e, "Failed to read response body");
                    diagnostics.record_error(e.to_string());
                    continue;
                },
            };

            match mapper.parse(&body) {
                Ok(batch) => {
                    if !batch.is_empty() {
                        diagnostics.add_read(batch.len() as u64);
                        let _ = event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));
                    }
                    consecutive_failures = 0;
                    state.store(ConnectionState::Connected as u8, Ordering::SeqCst);
                },
                Err(e) => {
                    debug!(channel_id, error = %e, "Failed to parse HTTP response");
                    diagnostics.record_error(e.to_string());
                },
            }
        }
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
        // Webhook mode is event-driven, polling mode is not
        self.config.mode == HttpMode::Webhook
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

        // Load JSON mappings if not already loaded
        self.load_mapper().await?;

        // Create HTTP client
        let client = self.create_client()?;
        self.client = Some(client);

        self.set_state(ConnectionState::Connected);

        info!(
            channel_id = self.channel_id,
            mode = ?self.config.mode,
            url = ?self.config.url,
            "HTTP channel connected"
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Stop polling task if running
        if let Some(handle) = self.poll_task_handle.take() {
            handle.abort();
        }

        self.client = None;
        self.set_state(ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "HTTP channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        // Only polling mode supports poll_once
        if self.config.mode != HttpMode::Polling {
            return PollResult::success(DataBatch::new());
        }

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
        // Both modes support subscribe for monitoring
        Some(self.event_tx.subscribe())
    }

    async fn start_events(&mut self) -> Result<()> {
        // Connect if not already connected
        if self.client.is_none() {
            self.connect().await?;
        }

        // For polling mode, start background polling task
        if self.config.mode == HttpMode::Polling && self.poll_task_handle.is_none() {
            let mapper = self.mapper.clone().ok_or_else(|| {
                GatewayError::Config("Mapper not loaded for HTTP polling".to_string())
            })?;
            let client = self
                .client
                .clone()
                .ok_or_else(|| GatewayError::Config("HTTP client not initialized".to_string()))?;
            let config = self.config.clone();
            let channel_id = self.channel_id;
            let state = Arc::new(AtomicU8::new(ConnectionState::Connected as u8));
            let event_tx = self.event_tx.clone();
            let diagnostics = self.diagnostics.clone();

            let handle = tokio::spawn(async move {
                Self::run_poll_loop(
                    channel_id,
                    client,
                    config,
                    mapper,
                    state,
                    event_tx,
                    diagnostics,
                )
                .await;
            });

            self.poll_task_handle = Some(handle);
            info!(channel_id = self.channel_id, "HTTP polling task started");
        }

        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        // Stop polling task
        if let Some(handle) = self.poll_task_handle.take() {
            handle.abort();
            info!(channel_id = self.channel_id, "HTTP polling task stopped");
        }
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
            .field("mode", &self.config.mode)
            .field("url", &self.config.url)
            .field("state", &self.connection_state())
            .finish()
    }
}

// ============================================================================
// Webhook Handler (for API integration)
// ============================================================================

/// Webhook data handler for processing incoming HTTP POST requests
///
/// This struct is designed to be integrated with the io API router.
/// It validates incoming requests and processes JSON payloads.
#[derive(Clone)]
pub struct WebhookHandler {
    /// Channel ID
    pub channel_id: u32,
    /// JSON mapper
    pub mapper: Arc<JsonMapper>,
    /// Event sender
    pub event_tx: DataEventSender,
    /// Authentication token (optional)
    pub auth_token: Option<String>,
    /// Diagnostics
    pub diagnostics: Arc<AtomicDiagnostics>,
}

impl WebhookHandler {
    /// Create a new webhook handler
    pub fn new(
        channel_id: u32,
        mapper: Arc<JsonMapper>,
        event_tx: DataEventSender,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            channel_id,
            mapper,
            event_tx,
            auth_token,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
        }
    }

    /// Process incoming webhook payload
    ///
    /// Returns the number of extracted data points, or an error.
    pub fn process(&self, payload: &[u8], auth_header: Option<&str>) -> Result<usize> {
        // Validate auth token if configured
        if let Some(expected) = &self.auth_token {
            let provided = auth_header.ok_or_else(|| {
                GatewayError::Protocol("Missing authorization header".to_string())
            })?;

            // Support "Bearer <token>" format
            let token = provided.strip_prefix("Bearer ").unwrap_or(provided);
            if token != expected {
                return Err(GatewayError::Protocol(
                    "Invalid authorization token".to_string(),
                ));
            }
        }

        // Parse payload
        let batch = self.mapper.parse(payload)?;
        let count = batch.len();

        if !batch.is_empty() {
            self.diagnostics.add_read(count as u64);
            let _ = self.event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));
        }

        debug!(
            channel_id = self.channel_id,
            points = count,
            "Processed webhook payload"
        );

        Ok(count)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_http_params_default() {
        let params = HttpParamsConfig::default();
        assert_eq!(params.mode, HttpMode::Polling);
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
        assert_eq!(params.mode, HttpMode::Polling);
        assert_eq!(
            params.url,
            Some("http://192.168.1.100/api/data".to_string())
        );
        assert_eq!(params.interval_ms, 5000);
        assert!(params.headers.contains_key("Authorization"));
    }

    #[test]
    fn test_http_mode_deserialize() {
        assert_eq!(
            serde_json::from_str::<HttpMode>(r#""polling""#).unwrap(),
            HttpMode::Polling
        );
        assert_eq!(
            serde_json::from_str::<HttpMode>(r#""webhook""#).unwrap(),
            HttpMode::Webhook
        );
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
