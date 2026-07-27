//! User-broker endpoint, authentication, TLS, and migration configuration.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use thiserror::Error;

use crate::TopicNamespace;

const MAX_CREDENTIAL_BYTES: usize = 1_024;
const MAX_CERTIFICATE_BYTES: u64 = 1024 * 1024;

/// Explicit legacy/CloudLink migration mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkMigrationMode {
    /// Existing unversioned adapter only. Deprecated but compatibility-safe.
    #[default]
    Legacy,
    /// Experimental CloudLink v1 namespace only.
    CloudLinkV1,
    /// Strictly isolated legacy and CloudLink namespaces during migration.
    Dual,
}

impl CloudLinkMigrationMode {
    /// Returns whether the deprecated compatibility adapter is selected.
    #[must_use]
    pub const fn legacy_enabled(self) -> bool {
        matches!(self, Self::Legacy | Self::Dual)
    }

    /// Returns whether exactly one CloudLink v1 stream owner is selected.
    #[must_use]
    pub const fn cloudlink_enabled(self) -> bool {
        matches!(self, Self::CloudLinkV1 | Self::Dual)
    }
}

impl FromStr for CloudLinkMigrationMode {
    type Err = CloudLinkMqttError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "cloudlink-v1" => Ok(Self::CloudLinkV1),
            "dual" => Ok(Self::Dual),
            _ => Err(CloudLinkMqttError::InvalidConfiguration(
                "CloudLink migration mode must be legacy, cloudlink-v1, or dual",
            )),
        }
    }
}

/// Whether deployment policy permits plaintext development transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentSecurity {
    /// Local/integration-only plaintext may be explicitly selected.
    Development,
    /// TLS is mandatory and downgrade is forbidden.
    Production,
}

/// Secret wrapper whose diagnostics never expose its contents.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps a configured broker password.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

/// Optional MQTT mTLS client identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttClientIdentity {
    /// PEM client certificate path.
    pub certificate_path: PathBuf,
    /// Unencrypted PKCS#8 PEM private-key path.
    pub private_key_path: PathBuf,
}

/// MQTT TLS trust mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudLinkTlsConfig {
    /// Explicit development-only plaintext.
    Disabled,
    /// Platform trust store, with no client certificate.
    SystemRoots,
    /// Operator-provided CA and optional mTLS identity.
    Custom {
        /// PEM CA bundle path.
        ca_path: PathBuf,
        /// Optional all-or-nothing client certificate/key pair.
        client_identity: Option<MqttClientIdentity>,
    },
}

/// User-selected MQTT broker configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkMqttConfig {
    /// Hostname or IP only; URI schemes and paths are rejected.
    pub broker_host: String,
    /// TCP port.
    pub broker_port: u16,
    /// Stable broker client identity.
    pub client_id: String,
    /// Optional broker username.
    pub username: Option<String>,
    /// Optional broker password, always redacted from diagnostics.
    pub password: Option<SecretString>,
    /// TLS trust/client identity configuration.
    pub tls: CloudLinkTlsConfig,
    /// MQTT keepalive seconds.
    pub keep_alive_secs: u64,
    /// Delay before reconnecting after transport failure.
    pub reconnect_delay_secs: u64,
    /// Bounded rumqttc/outbound request channel.
    pub request_capacity: usize,
    /// Incoming and outgoing MQTT packet bound.
    pub maximum_packet_bytes: usize,
}

impl CloudLinkMqttConfig {
    /// Creates explicit plaintext development settings for local testing.
    #[must_use]
    pub fn development(
        broker_host: impl Into<String>,
        broker_port: u16,
        client_id: impl Into<String>,
    ) -> Self {
        Self {
            broker_host: broker_host.into(),
            broker_port,
            client_id: client_id.into(),
            username: None,
            password: None,
            tls: CloudLinkTlsConfig::Disabled,
            keep_alive_secs: 30,
            reconnect_delay_secs: 1,
            request_capacity: 64,
            maximum_packet_bytes: aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES,
        }
    }

    /// Validates all bounds and TLS file relationships without exposing secrets.
    pub fn validate(&self, security: DeploymentSecurity) -> Result<(), CloudLinkMqttError> {
        validate_host(&self.broker_host)?;
        if self.broker_port == 0 {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT broker port must be greater than zero",
            ));
        }
        validate_mqtt_text(&self.client_id, "MQTT client ID", 128)?;
        if !(5..=3_600).contains(&self.keep_alive_secs) {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT keepalive must be between 5 and 3600 seconds",
            ));
        }
        if self.reconnect_delay_secs == 0 || self.reconnect_delay_secs > 3_600 {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT reconnect delay must be between 1 and 3600 seconds",
            ));
        }
        if self.request_capacity == 0 || self.request_capacity > 4_096 {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT request capacity must be between 1 and 4096",
            ));
        }
        if self.maximum_packet_bytes == 0
            || self.maximum_packet_bytes > aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES
        {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT packet bound must be within the CloudLink 256 KiB limit",
            ));
        }
        if let Some(username) = &self.username {
            validate_mqtt_text(username, "MQTT username", 256)?;
        }
        if self.password.is_some() && self.username.is_none() {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT password requires a username",
            ));
        }
        if let Some(password) = &self.password
            && (password.expose().is_empty() || password.expose().len() > MAX_CREDENTIAL_BYTES)
        {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "MQTT password length is outside the accepted bound",
            ));
        }
        match &self.tls {
            CloudLinkTlsConfig::Disabled if security == DeploymentSecurity::Production => {
                return Err(CloudLinkMqttError::InvalidConfiguration(
                    "production CloudLink MQTT requires TLS",
                ));
            },
            CloudLinkTlsConfig::Custom {
                ca_path,
                client_identity,
            } => {
                validate_tls_file(ca_path, "CA certificate")?;
                if let Some(identity) = client_identity {
                    if identity.certificate_path.as_os_str().is_empty()
                        || identity.private_key_path.as_os_str().is_empty()
                    {
                        return Err(CloudLinkMqttError::InvalidConfiguration(
                            "MQTT client certificate and private key must be configured together",
                        ));
                    }
                    validate_tls_file(&identity.certificate_path, "client certificate")?;
                    validate_tls_file(&identity.private_key_path, "client private key")?;
                }
            },
            CloudLinkTlsConfig::Disabled | CloudLinkTlsConfig::SystemRoots => {},
        }
        Ok(())
    }

    /// Validates that legacy and CloudLink topic namespaces cannot overlap.
    pub fn validate_namespace(
        &self,
        topics: &TopicNamespace,
        legacy_topics: &[&str],
    ) -> Result<(), CloudLinkMqttError> {
        for topic in topics
            .publish_topics()
            .into_iter()
            .chain(topics.subscribe_topics())
        {
            if legacy_topics.iter().any(|legacy| *legacy == topic) {
                return Err(CloudLinkMqttError::InvalidConfiguration(
                    "legacy and CloudLink MQTT namespaces must be isolated",
                ));
            }
        }
        Ok(())
    }
}

/// Safe configuration/TLS construction error.
#[derive(Debug, Error)]
pub enum CloudLinkMqttError {
    /// Static validation failed; no secret value is interpolated.
    #[error("invalid CloudLink MQTT configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// A configured TLS file cannot be safely read.
    #[error("cannot use configured MQTT {role} file {path}: {message}")]
    TlsFile {
        /// Non-secret certificate/key role.
        role: &'static str,
        /// Configured local path.
        path: PathBuf,
        /// Sanitized failure class.
        message: &'static str,
    },
    /// Certificate or key bytes cannot be parsed.
    #[error("configured MQTT TLS material is invalid: {0}")]
    InvalidTlsMaterial(&'static str),
}

fn validate_host(host: &str) -> Result<(), CloudLinkMqttError> {
    let valid = !host.is_empty()
        && host.len() <= 253
        && !host.contains("://")
        && !host.contains('/')
        && !host.chars().any(char::is_whitespace)
        && !host.chars().any(char::is_control);
    if valid {
        Ok(())
    } else {
        Err(CloudLinkMqttError::InvalidConfiguration(
            "MQTT broker host must be a bounded hostname or IP without scheme or path",
        ))
    }
}

fn validate_mqtt_text(
    value: &str,
    _field: &'static str,
    maximum: usize,
) -> Result<(), CloudLinkMqttError> {
    if !value.is_empty()
        && value.len() <= maximum
        && !value.contains('\0')
        && !value.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err(CloudLinkMqttError::InvalidConfiguration(
            "MQTT text field is empty, oversized, or contains control characters",
        ))
    }
}

fn validate_tls_file(path: &Path, role: &'static str) -> Result<(), CloudLinkMqttError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| CloudLinkMqttError::TlsFile {
        role,
        path: path.to_path_buf(),
        message: "file is missing or unreadable",
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CloudLinkMqttError::TlsFile {
            role,
            path: path.to_path_buf(),
            message: "a regular non-symlink file is required",
        });
    }
    if metadata.len() == 0 || metadata.len() > MAX_CERTIFICATE_BYTES {
        return Err(CloudLinkMqttError::TlsFile {
            role,
            path: path.to_path_buf(),
            message: "file is empty or exceeds 1 MiB",
        });
    }
    Ok(())
}
