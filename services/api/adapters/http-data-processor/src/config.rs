use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use aether_ports::{DataBoundary, DataProcessorDescriptor, PortError, PortErrorKind, PortResult};
use reqwest::Url;

/// Explicit bearer token supplied by the composition root.
///
/// The type has no environment-loading API and always redacts its value from
/// `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct BearerSecret(String);

impl BearerSecret {
    /// Validates an explicit RFC 6750 bearer-token value.
    pub fn new(value: impl Into<String>) -> PortResult<Self> {
        let value = value.into();
        let valid = value.len() >= 32
            && value.len() <= 8_192
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
            });
        if !valid {
            return Err(permanent("bearer secret is invalid"));
        }
        Ok(Self(value))
    }

    pub(crate) fn authorization_value(&self) -> PortResult<reqwest::header::HeaderValue> {
        let value = format!("Bearer {}", self.0);
        let mut header = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| permanent("bearer secret is invalid"))?;
        header.set_sensitive(true);
        Ok(header)
    }
}

impl fmt::Debug for BearerSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerSecret([REDACTED])")
    }
}

/// Validated endpoints and hard limits for one HTTP processor adapter.
#[derive(Clone)]
pub struct HttpDataProcessorConfig {
    base_url: Url,
    process_url: Url,
    health_url: Url,
    descriptor: DataProcessorDescriptor,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_response_bytes: usize,
    bearer_secret: Option<BearerSecret>,
}

impl HttpDataProcessorConfig {
    /// Creates an explicit configuration rooted at one processor origin.
    ///
    /// The adapter derives `POST /v1/process` and `GET /v1/health`. Plain HTTP
    /// is accepted only for a local boundary on `localhost` or an IP loopback.
    pub fn new(
        base_url: impl AsRef<str>,
        descriptor: DataProcessorDescriptor,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> PortResult<Self> {
        if connect_timeout.is_zero() || request_timeout.is_zero() || max_response_bytes == 0 {
            return Err(permanent("HTTP processor limits must be positive"));
        }
        let base_url = validate_base_url(base_url.as_ref(), descriptor.data_boundary())?;
        let process_url = base_url
            .join("v1/process")
            .map_err(|_| permanent("HTTP processor endpoint is invalid"))?;
        let health_url = base_url
            .join("v1/health")
            .map_err(|_| permanent("HTTP processor endpoint is invalid"))?;
        Ok(Self {
            base_url,
            process_url,
            health_url,
            descriptor,
            connect_timeout,
            request_timeout,
            max_response_bytes,
            bearer_secret: None,
        })
    }

    /// Adds a composition-root supplied token. No ambient environment is read.
    #[must_use]
    pub fn with_bearer_secret(mut self, secret: BearerSecret) -> Self {
        self.bearer_secret = Some(secret);
        self
    }

    /// Returns the discoverable port descriptor, including contract and limits.
    #[must_use]
    pub const fn descriptor(&self) -> &DataProcessorDescriptor {
        &self.descriptor
    }

    /// Returns the hard response-body byte limit.
    #[must_use]
    pub const fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    pub(crate) const fn process_url(&self) -> &Url {
        &self.process_url
    }

    pub(crate) const fn health_url(&self) -> &Url {
        &self.health_url
    }

    pub(crate) const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    pub(crate) const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub(crate) const fn bearer_secret(&self) -> Option<&BearerSecret> {
        self.bearer_secret.as_ref()
    }
}

impl fmt::Debug for HttpDataProcessorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpDataProcessorConfig")
            .field("base_url", &self.base_url)
            .field("descriptor", &self.descriptor)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("bearer_secret", &self.bearer_secret)
            .finish()
    }
}

fn validate_base_url(value: &str, boundary: DataBoundary) -> PortResult<Url> {
    let url = Url::parse(value).map_err(|_| permanent("HTTP processor endpoint is invalid"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(permanent("HTTP processor endpoint is invalid"));
    }
    match (boundary, url.scheme()) {
        (DataBoundary::Local, "http" | "https") if is_loopback_host(&url) => {},
        (DataBoundary::Remote, "https") => {},
        (DataBoundary::Remote, "http") => {
            return Err(permanent("remote HTTP processor requires TLS"));
        },
        _ => return Err(permanent("HTTP processor endpoint is invalid")),
    }
    Ok(url)
}

fn is_loopback_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn permanent(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}
