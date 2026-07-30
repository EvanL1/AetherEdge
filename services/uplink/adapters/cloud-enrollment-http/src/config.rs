use std::time::Duration;

use aether_ports::CloudEnrollmentClientError;

/// Hard upper bound for a Cloud Claim response body.
pub const MAX_CLAIM_RESPONSE_BYTES: usize = 64 * 1024;

/// Explicit HTTP deadlines and response bound for one enrollment client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpCloudEnrollmentConfig {
    connect_timeout: Duration,
    request_timeout: Duration,
    total_timeout: Duration,
    max_response_bytes: usize,
}

impl HttpCloudEnrollmentConfig {
    /// Creates a client configuration with positive deadlines and a response
    /// limit no larger than 64 KiB.
    pub fn new(
        connect_timeout: Duration,
        request_timeout: Duration,
        total_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, CloudEnrollmentClientError> {
        if connect_timeout.is_zero()
            || request_timeout.is_zero()
            || total_timeout.is_zero()
            || max_response_bytes == 0
            || max_response_bytes > MAX_CLAIM_RESPONSE_BYTES
        {
            return Err(CloudEnrollmentClientError::InvalidConfiguration);
        }
        Ok(Self {
            connect_timeout,
            request_timeout,
            total_timeout,
            max_response_bytes,
        })
    }

    pub(crate) const fn connect_timeout(self) -> Duration {
        self.connect_timeout
    }

    pub(crate) const fn request_timeout(self) -> Duration {
        self.request_timeout
    }

    pub(crate) const fn total_timeout(self) -> Duration {
        self.total_timeout
    }

    pub(crate) const fn max_response_bytes(self) -> usize {
        self.max_response_bytes
    }
}

impl Default for HttpCloudEnrollmentConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(20),
            total_timeout: Duration::from_secs(30),
            max_response_bytes: MAX_CLAIM_RESPONSE_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpCloudEnrollmentConfig, MAX_CLAIM_RESPONSE_BYTES};
    use std::time::Duration;

    #[test]
    fn configuration_requires_positive_bounded_limits() {
        let valid = HttpCloudEnrollmentConfig::new(
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
            MAX_CLAIM_RESPONSE_BYTES,
        )
        .expect("valid configuration");
        assert_eq!(valid.max_response_bytes(), MAX_CLAIM_RESPONSE_BYTES);

        for (connect, request, total, max_bytes) in [
            (
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            (
                Duration::from_secs(1),
                Duration::ZERO,
                Duration::from_secs(1),
                1,
            ),
            (
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::ZERO,
                1,
            ),
            (
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                0,
            ),
            (
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                MAX_CLAIM_RESPONSE_BYTES + 1,
            ),
        ] {
            assert!(HttpCloudEnrollmentConfig::new(connect, request, total, max_bytes).is_err());
        }
    }
}
