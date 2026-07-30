use std::fmt;

use aether_ports::{
    CloudEnrollmentClaim, CloudEnrollmentClient, CloudEnrollmentClientError, CloudEnrollmentReceipt,
};
use async_trait::async_trait;
use bytes::Bytes;
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response, StatusCode};
use zeroize::Zeroizing;

use crate::config::HttpCloudEnrollmentConfig;
use crate::wire::{
    claim_endpoint, decode_claimed_response, encode_claim_request, validate_json_content_type,
};

const JSON_MEDIA_TYPE: &str = "application/json";
const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

/// Strict HTTP implementation of the AetherCloud Gateway Claim port.
pub struct HttpCloudEnrollmentClient {
    config: HttpCloudEnrollmentConfig,
    client: Client,
}

impl HttpCloudEnrollmentClient {
    /// Builds a rustls client with redirects and ambient proxy discovery
    /// disabled.
    pub fn new(config: HttpCloudEnrollmentConfig) -> Result<Self, CloudEnrollmentClientError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(JSON_MEDIA_TYPE));
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.connect_timeout())
            .timeout(config.request_timeout())
            .no_proxy()
            .user_agent(concat!(
                "aether-cloud-enrollment/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|_| CloudEnrollmentClientError::InvalidConfiguration)?;
        Ok(Self { config, client })
    }

    async fn claim_once(
        &self,
        claim: &CloudEnrollmentClaim,
    ) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError> {
        let target = claim.target();
        // The target type can contain plain HTTP only when its caller selected
        // the explicit loopback-development policy. Revalidation here still
        // limits that exception to the exact two supported host spellings.
        let endpoint = claim_endpoint(target.cloud_origin(), true)
            .map_err(|_| CloudEnrollmentClientError::InvalidConfiguration)?;
        let tenant_id = target.tenant_id().to_string();
        let project_id = target.project_id().to_string();
        let gateway_id = target.gateway_id().to_string();
        let body = Zeroizing::new(
            encode_claim_request(
                &tenant_id,
                &project_id,
                &gateway_id,
                claim.enrollment_token().expose(),
                claim.public_key().as_bytes(),
                claim.fingerprint().as_str(),
            )
            .map_err(|_| CloudEnrollmentClientError::InvalidConfiguration)?,
        );
        // `Bytes::from_owner` retains the zeroizing owner without another copy.
        // Once reqwest/hyper releases its final clone, the encoded token-bearing
        // JSON buffer is overwritten before deallocation.
        let body = Bytes::from_owner(body);
        let idempotency_key = HeaderValue::from_str(claim.idempotency_key().as_str())
            .map_err(|_| CloudEnrollmentClientError::InvalidConfiguration)?;

        let response = self
            .client
            .post(endpoint)
            .header(CONTENT_TYPE, JSON_MEDIA_TYPE)
            .header(IDEMPOTENCY_KEY, idempotency_key)
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;

        if !response.status().is_success() {
            return decode_failure(response, self.config.max_response_bytes()).await;
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .ok_or(CloudEnrollmentClientError::InvalidResponse)?;
        validate_json_content_type(content_type)
            .map_err(|_| CloudEnrollmentClientError::InvalidResponse)?;
        let response_body = read_limited(response, self.config.max_response_bytes()).await?;
        let claimed = decode_claimed_response(&response_body, &gateway_id)
            .map_err(|_| CloudEnrollmentClientError::InvalidResponse)?;
        Ok(CloudEnrollmentReceipt::new(
            claimed.gateway_id,
            claimed.revision,
        ))
    }
}

impl fmt::Debug for HttpCloudEnrollmentClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpCloudEnrollmentClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CloudEnrollmentClient for HttpCloudEnrollmentClient {
    async fn claim(
        &self,
        claim: CloudEnrollmentClaim,
    ) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError> {
        tokio::time::timeout(self.config.total_timeout(), self.claim_once(&claim))
            .await
            .map_err(|_| CloudEnrollmentClientError::Timeout)?
    }
}

async fn decode_failure(
    response: Response,
    max_response_bytes: usize,
) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError> {
    let status = response.status();
    drain_limited(response, max_response_bytes).await?;
    Err(match status {
        StatusCode::CONFLICT => CloudEnrollmentClientError::Conflict,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS => {
            CloudEnrollmentClientError::Unavailable
        },
        status if status.is_client_error() => CloudEnrollmentClientError::Rejected,
        status if status.is_server_error() => CloudEnrollmentClientError::Unavailable,
        _ => CloudEnrollmentClientError::InvalidResponse,
    })
}

async fn read_limited(
    mut response: Response,
    max_bytes: usize,
) -> Result<Vec<u8>, CloudEnrollmentClientError> {
    if declared_body_exceeds(&response, max_bytes) {
        return Err(CloudEnrollmentClientError::InvalidResponse);
    }

    let capacity = response.content_length().map_or(0, |length| {
        usize::try_from(length).map_or(max_bytes, |length| length.min(max_bytes))
    });
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or(CloudEnrollmentClientError::InvalidResponse)?;
        if next_len > max_bytes {
            return Err(CloudEnrollmentClientError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn drain_limited(
    mut response: Response,
    max_bytes: usize,
) -> Result<(), CloudEnrollmentClientError> {
    if declared_body_exceeds(&response, max_bytes) {
        return Err(CloudEnrollmentClientError::InvalidResponse);
    }

    let mut received_bytes = 0_usize;
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        received_bytes = received_bytes
            .checked_add(chunk.len())
            .ok_or(CloudEnrollmentClientError::InvalidResponse)?;
        if received_bytes > max_bytes {
            return Err(CloudEnrollmentClientError::InvalidResponse);
        }
    }
    Ok(())
}

fn declared_body_exceeds(response: &Response, max_bytes: usize) -> bool {
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_bytes as u64)
}

fn transport_error(error: reqwest::Error) -> CloudEnrollmentClientError {
    if error.is_timeout() {
        CloudEnrollmentClientError::Timeout
    } else if error.is_builder() {
        CloudEnrollmentClientError::InvalidConfiguration
    } else {
        CloudEnrollmentClientError::Unavailable
    }
}
