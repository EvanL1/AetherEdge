use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_data_processing::{decode_result, encode_request};
use aether_domain::{DataProcessingRequest, ProcessingOptions, ProcessingResult, TaskKind};
use aether_ports::{
    DataProcessor, DataProcessorDescriptor, PortError, PortErrorKind, PortResult, ProcessorHealth,
};
use async_trait::async_trait;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue,
};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;

use crate::{HttpDataProcessorConfig, JSON_MEDIA_TYPE};

/// Bounded, request-driven HTTP implementation of [`DataProcessor`].
pub struct HttpDataProcessor {
    config: HttpDataProcessorConfig,
    client: Client,
}

impl HttpDataProcessor {
    /// Builds a rustls client with redirects and ambient proxy discovery disabled.
    pub fn new(config: HttpDataProcessorConfig) -> PortResult<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(JSON_MEDIA_TYPE));
        if let Some(secret) = config.bearer_secret() {
            headers.insert(AUTHORIZATION, secret.authorization_value()?);
        }
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.connect_timeout())
            .timeout(config.request_timeout())
            .no_proxy()
            .user_agent(concat!(
                "aether-http-data-processor/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|_| permanent("HTTP processor client could not be initialized"))?;
        Ok(Self { config, client })
    }

    async fn send_process(&self, request: &DataProcessingRequest) -> PortResult<ProcessingResult> {
        self.validate_request_route(request)?;
        let body = encode_request(request)
            .map_err(|_| rejected("processing request does not satisfy the v1 wire contract"))?;
        if body.len() > self.config.descriptor().max_request_bytes() {
            return Err(rejected(
                "processing request exceeds the configured byte limit",
            ));
        }
        let timeout = self.timeout_before(request.deadline().get())?;
        let response = self
            .client
            .post(self.config.process_url().clone())
            .header(CONTENT_TYPE, JSON_MEDIA_TYPE)
            .timeout(timeout)
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return decode_processor_failure(
                response,
                self.config.max_response_bytes(),
                Some(request.request_id()),
            )
            .await;
        }
        ensure_process_media_type(response.headers().get(CONTENT_TYPE))?;
        let bytes = read_limited(response, self.config.max_response_bytes()).await?;
        decode_result(&bytes)
            .map_err(|_| invalid_data("processor response violates the v1 result contract"))
    }

    fn validate_request_route(&self, request: &DataProcessingRequest) -> PortResult<()> {
        let task_kind = match request.options() {
            ProcessingOptions::Forecast(_) => TaskKind::Forecast,
        };
        let descriptor = self.config.descriptor();
        if !descriptor.supports(task_kind)
            || !descriptor.supports_contract(request.processor_contract())
        {
            return Err(permanent(
                "processor route does not support the selected contract",
            ));
        }
        if request.frame().cell_count() > descriptor.max_frame_samples() {
            return Err(rejected(
                "processing frame exceeds the configured sample limit",
            ));
        }
        Ok(())
    }

    fn timeout_before(&self, deadline_ms: u64) -> PortResult<Duration> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| permanent("system clock cannot represent the processing deadline"))?;
        let now_ms = u64::try_from(now.as_millis())
            .map_err(|_| permanent("system clock cannot represent the processing deadline"))?;
        let remaining = deadline_ms
            .checked_sub(now_ms)
            .filter(|milliseconds| *milliseconds > 0)
            .ok_or_else(|| timeout("processing deadline has elapsed"))?;
        Ok(self
            .config
            .request_timeout()
            .min(Duration::from_millis(remaining)))
    }

    async fn check_health(&self) -> PortResult<ProcessorHealth> {
        let response = self
            .client
            .get(self.config.health_url().clone())
            .timeout(self.config.request_timeout())
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return decode_processor_failure(response, self.config.max_response_bytes(), None)
                .await;
        }
        ensure_health_media_type(response.headers().get(CONTENT_TYPE))?;
        let bytes = read_limited(response, self.config.max_response_bytes()).await?;
        let health: HealthResponse = serde_json::from_slice(&bytes)
            .map_err(|_| invalid_data("processor health response is invalid"))?;
        let descriptor = self.config.descriptor();
        if health.processor != descriptor.id()
            || health.version != descriptor.version()
            || !descriptor.supports_contract(&health.contract)
        {
            return Err(invalid_data(
                "processor health identity does not match its route",
            ));
        }
        match health.status.as_str() {
            "ok" | "healthy" => Ok(ProcessorHealth::Healthy),
            "degraded" => Ok(ProcessorHealth::Degraded),
            "unavailable" => Ok(ProcessorHealth::Unavailable),
            _ => Err(invalid_data("processor health status is invalid")),
        }
    }
}

impl fmt::Debug for HttpDataProcessor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpDataProcessor")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl DataProcessor for HttpDataProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        self.config.descriptor()
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        self.check_health().await
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        self.send_process(&request).await
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthResponse {
    status: String,
    processor: String,
    version: String,
    contract: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessorErrorEnvelope {
    schema: String,
    request_id: Option<String>,
    code: String,
    category: ProcessorErrorCategory,
    message: String,
    retryable: bool,
    details: Option<ProcessorErrorDetails>,
}

impl ProcessorErrorEnvelope {
    fn validate(
        &self,
        status: StatusCode,
        expected_request_id: Option<&str>,
    ) -> PortResult<Option<u64>> {
        if self.schema != "aether.data-processing.error.v1"
            || !is_stable_code(&self.code)
            || !is_contract_message(&self.message)
            || self
                .request_id
                .as_deref()
                .is_some_and(|request_id| !is_uuid(request_id))
            || expected_request_id.is_some_and(|expected| {
                self.request_id
                    .as_deref()
                    .is_some_and(|actual| actual != expected)
            })
            || expected_category(status) != Some(self.category)
            || !self.category.retryable_is_valid(self.retryable)
        {
            return Err(invalid_data(
                "processor error response violates the v1 error contract",
            ));
        }

        let Some(details) = self.details.as_ref() else {
            return Ok(None);
        };
        if details
            .path
            .as_deref()
            .is_some_and(|path| !path.starts_with('/') || path.chars().count() > 2_048)
            || details
                .rule
                .as_deref()
                .is_some_and(|rule| rule.is_empty() || rule.chars().count() > 2_048)
            || details.retry_after_seconds == Some(0)
            || (!self.retryable && details.retry_after_seconds.is_some())
        {
            return Err(invalid_data(
                "processor error response violates the v1 error contract",
            ));
        }
        match details.retry_after_seconds {
            Some(seconds) => seconds.checked_mul(1_000).map(Some).ok_or_else(|| {
                invalid_data("processor error response violates the v1 error contract")
            }),
            None => Ok(None),
        }
    }

    fn kind(&self) -> PortErrorKind {
        match self.category {
            ProcessorErrorCategory::InvalidRequest | ProcessorErrorCategory::ResourceLimit => {
                PortErrorKind::Rejected
            },
            ProcessorErrorCategory::Authorization | ProcessorErrorCategory::NotFound => {
                PortErrorKind::Permanent
            },
            ProcessorErrorCategory::Conflict => PortErrorKind::Conflict,
            ProcessorErrorCategory::InvalidData => PortErrorKind::InvalidData,
            ProcessorErrorCategory::Capacity => PortErrorKind::Unavailable,
            ProcessorErrorCategory::Internal | ProcessorErrorCategory::Unavailable => {
                if self.retryable {
                    PortErrorKind::Unavailable
                } else {
                    PortErrorKind::Permanent
                }
            },
            ProcessorErrorCategory::Timeout => PortErrorKind::Timeout,
        }
    }

    fn safe_message(&self, retry_after_ms: Option<u64>) -> String {
        format!(
            "processor error code={} category={} retryable={} retry_after_ms={} request_id={}",
            self.code,
            self.category.as_str(),
            self.retryable,
            retry_after_ms.map_or_else(|| "none".to_string(), |delay| delay.to_string()),
            self.request_id.as_deref().unwrap_or("none")
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessorErrorDetails {
    path: Option<String>,
    rule: Option<String>,
    retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessorErrorCategory {
    InvalidRequest,
    Authorization,
    NotFound,
    Conflict,
    ResourceLimit,
    InvalidData,
    Capacity,
    Internal,
    Unavailable,
    Timeout,
}

impl ProcessorErrorCategory {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Authorization => "authorization",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::ResourceLimit => "resource_limit",
            Self::InvalidData => "invalid_data",
            Self::Capacity => "capacity",
            Self::Internal => "internal",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
        }
    }

    const fn retryable_is_valid(self, retryable: bool) -> bool {
        match self {
            Self::InvalidRequest
            | Self::Authorization
            | Self::NotFound
            | Self::ResourceLimit
            | Self::InvalidData => !retryable,
            Self::Conflict | Self::Capacity | Self::Timeout => retryable,
            Self::Internal | Self::Unavailable => true,
        }
    }
}

async fn decode_processor_failure<T>(
    response: Response,
    max_response_bytes: usize,
    expected_request_id: Option<&str>,
) -> PortResult<T> {
    let status = response.status();
    if !status.is_client_error() && !status.is_server_error() {
        return Err(permanent("processor returned an unsupported HTTP status"));
    }
    ensure_process_media_type(response.headers().get(CONTENT_TYPE))?;
    let bytes = read_limited(response, max_response_bytes).await?;
    let envelope: ProcessorErrorEnvelope = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_data("processor error response violates the v1 error contract"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_data("processor error response violates the v1 error contract"))?;
    if contains_json_null(&value) {
        return Err(invalid_data(
            "processor error response violates the v1 error contract",
        ));
    }
    let retry_after_ms = envelope.validate(status, expected_request_id)?;
    Err(PortError::new(
        envelope.kind(),
        envelope.safe_message(retry_after_ms),
    ))
}

const fn expected_category(status: StatusCode) -> Option<ProcessorErrorCategory> {
    match status.as_u16() {
        400 | 405 | 406 | 415 => Some(ProcessorErrorCategory::InvalidRequest),
        401 | 403 => Some(ProcessorErrorCategory::Authorization),
        404 | 410 => Some(ProcessorErrorCategory::NotFound),
        409 => Some(ProcessorErrorCategory::Conflict),
        413 => Some(ProcessorErrorCategory::ResourceLimit),
        422 => Some(ProcessorErrorCategory::InvalidData),
        429 => Some(ProcessorErrorCategory::Capacity),
        500 => Some(ProcessorErrorCategory::Internal),
        502 | 503 => Some(ProcessorErrorCategory::Unavailable),
        408 | 504 => Some(ProcessorErrorCategory::Timeout),
        _ => None,
    }
}

fn is_stable_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_uppercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_contract_message(value: &str) -> bool {
    let length = value.chars().count();
    (1..=1_024).contains(&length)
        && value.chars().all(|character| {
            let codepoint = u32::from(character);
            !matches!(codepoint, 0x00..=0x08 | 0x0b..=0x0c | 0x0e..=0x1f | 0x7f)
        })
}

fn is_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    let bytes = value.as_bytes();
    bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    }) && matches!(bytes[14], b'1'..=b'8')
        && matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b')
}

fn contains_json_null(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Array(values) => values.iter().any(contains_json_null),
        serde_json::Value::Object(fields) => fields.values().any(contains_json_null),
        _ => false,
    }
}

async fn read_limited(mut response: Response, max_bytes: usize) -> PortResult<Vec<u8>> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(invalid_data(
            "processor response exceeds the configured byte limit",
        ));
    }

    let mut body = Vec::with_capacity(response.content_length().map_or(0, |length| {
        usize::try_from(length).map_or(max_bytes, |length| length.min(max_bytes))
    }));
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| invalid_data("processor response exceeds the configured byte limit"))?;
        if next_len > max_bytes {
            return Err(invalid_data(
                "processor response exceeds the configured byte limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn ensure_process_media_type(value: Option<&HeaderValue>) -> PortResult<()> {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return Err(invalid_data("processor response media type is invalid"));
    };
    let mut parts = value.split(';');
    if !parts.next().is_some_and(|media_type| {
        media_type
            .trim()
            .eq_ignore_ascii_case("application/vnd.aether.data-processing+json")
    }) {
        return Err(invalid_data("processor response media type is invalid"));
    }
    let mut version_count = 0_u8;
    let mut charset_count = 0_u8;
    for parameter in parts {
        let parameter = parameter.trim();
        if parameter.eq_ignore_ascii_case("version=1") {
            version_count = version_count.saturating_add(1);
        } else if parameter.eq_ignore_ascii_case("charset=utf-8") {
            charset_count = charset_count.saturating_add(1);
        } else {
            return Err(invalid_data("processor response media type is invalid"));
        }
    }
    if version_count == 1 && charset_count <= 1 {
        Ok(())
    } else {
        Err(invalid_data("processor response media type is invalid"))
    }
}

fn ensure_health_media_type(value: Option<&HeaderValue>) -> PortResult<()> {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return Err(invalid_data("processor health media type is invalid"));
    };
    let media_type = value.split(';').next().map(str::trim).unwrap_or_default();
    if media_type.eq_ignore_ascii_case("application/json")
        || media_type.eq_ignore_ascii_case("application/vnd.aether.data-processing+json")
    {
        Ok(())
    } else {
        Err(invalid_data("processor health media type is invalid"))
    }
}

fn transport_error(error: reqwest::Error) -> PortError {
    if error.is_timeout() {
        timeout("HTTP processor request timed out")
    } else if error.is_builder() {
        permanent("HTTP processor request could not be built")
    } else {
        PortError::new(
            PortErrorKind::Unavailable,
            "HTTP processor transport is unavailable",
        )
    }
}

fn rejected(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Rejected, message)
}

fn invalid_data(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn permanent(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}

fn timeout(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Timeout, message)
}
