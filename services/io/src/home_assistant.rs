//! Composition root for the optional Home Assistant delegated-device integration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aether_domain::{
    GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationSnapshot, TopologyGeneration,
};
use aether_ports::{
    IntegrationProjectionQuery, IntegrationProjectionReceipt, IntegrationProjectionSink, PortError,
    PortErrorKind, PortResult, SecretRef,
};
use async_trait::async_trait;
#[cfg(feature = "home-assistant")]
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "home-assistant-integration-control")]
use crate::home_assistant_control::{ActiveHomeAssistantExecutor, PreparedIntegrationControl};
#[cfg(feature = "home-assistant")]
use aether_application::{IntegrationSynchronizationError, IntegrationSynchronizer};
#[cfg(feature = "home-assistant-cloudlink")]
use aether_cloudlink::{
    CLOUDLINK_INTEGRATION_EXTENSION, CLOUDLINK_PROTOCOL_VERSION, CandidateMessage, CloudLinkCodec,
    CloudLinkIntegrationExtension, CloudLinkIntegrationProjectionOutbox,
    CloudLinkIntegrationPublisher, GatewaySessionAuthenticator, ResumeCursor, SessionAccepted,
    SessionBinding, SessionChallenge, SessionChallengeRequest, SessionHello, UplinkAuthentication,
};
#[cfg(feature = "home-assistant-cloudlink")]
use aether_cloudlink_mqtt::{
    CloudLinkMqttConfig, CloudLinkTlsConfig, DeploymentSecurity, MqttCloudLinkTransport,
    SecretString, TopicNamespace,
};
#[cfg(feature = "home-assistant")]
use aether_home_assistant_bridge::{
    EnvironmentSecretResolver, HomeAssistantBridge, HomeAssistantConnectionConfig,
    WebSocketHomeAssistantTransport,
};
#[cfg(feature = "home-assistant")]
use aether_ports::IntegrationTopologyGenerationStore;
#[cfg(feature = "home-assistant-cloudlink")]
use aether_ports::{
    CloudLinkSpool, CloudLinkTransport, CloudLinkTransportEvent, CloudLinkTransportMessage,
    CloudLinkTransportRoute,
};
#[cfg(feature = "home-assistant")]
use aether_store_local::FileIntegrationTopologyGenerationStore;
#[cfg(feature = "home-assistant-cloudlink")]
use aether_store_local::{
    CloudLinkChallengeLedgerError, CloudLinkChallengeReservation, FileCloudLinkChallengeLedger,
    FileCloudLinkSpool,
};

const ENABLED_SETTING: &str = "AETHER_HOME_ASSISTANT_ENABLED";
const ORIGIN_SETTING: &str = "AETHER_HOME_ASSISTANT_ORIGIN";
const SECRET_REF_SETTING: &str = "AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF";
const PLAINTEXT_TOKEN_SETTING: &str = "AETHER_HOME_ASSISTANT_ACCESS_TOKEN";
const GATEWAY_ID_SETTING: &str = "AETHER_GATEWAY_ID";
const INTEGRATION_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_INTEGRATION_ID";
const GENERATION_STORE_PATH_SETTING: &str = "AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH";
const CLOUDLINK_ENABLED_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED";
const CLOUDLINK_ORIGIN_MODEL_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL";
const CLOUDLINK_CLOUD_KEY_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID";
const CLOUDLINK_CLOUD_PUBLIC_KEY_REF_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF";
const CLOUDLINK_GATEWAY_KEY_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID";
const CLOUDLINK_GATEWAY_SIGNING_KEY_REF_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF";
const CLOUDLINK_CHALLENGE_LEDGER_PATH_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH";
const CLOUDLINK_RUNTIME_CONFIG_DIR_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR";
const CLOUDLINK_CLOUD_EXTENSION_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION";
const CLOUDLINK_TOPOLOGY_SPOOL_PATH_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH";
const CLOUDLINK_OBSERVATION_SPOOL_PATH_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH";
const CLOUDLINK_SPOOL_CAPACITY_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_SPOOL_CAPACITY";
const CLOUDLINK_MQTT_HOST_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST";
const CLOUDLINK_MQTT_PORT_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT";
const CLOUDLINK_MQTT_CLIENT_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID";
const CLOUDLINK_MQTT_TOPIC_PREFIX_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX";
const CLOUDLINK_MQTT_USERNAME_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME";
const CLOUDLINK_MQTT_PASSWORD_REF_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF";
const CLOUDLINK_MQTT_PLAINTEXT_PASSWORD_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD";
const CLOUDLINK_CREDENTIAL_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID";
const CLOUDLINK_CREDENTIAL_GENERATION_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION";
const CLOUDLINK_SESSION_EPOCH_PATH_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH";
const CONTROL_ENABLED_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_ENABLED";
const CONTROL_CLOUD_EXTENSION_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION";
const CONTROL_LEDGER_PATH_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH";
const CONTROL_POLICY_PATH_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH";
const CONTROL_AUDIT_PATH_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH";
const CONTROL_CLOUD_KEY_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID";
const CONTROL_CLOUD_PUBLIC_KEY_REF_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF";
const CONTROL_EDGE_KEY_ID_SETTING: &str = "AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID";
const CONTROL_EDGE_SIGNING_KEY_REF_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF";
const CONTROL_PROVIDER_TIMEOUT_MS_SETTING: &str =
    "AETHER_HOME_ASSISTANT_CONTROL_PROVIDER_TIMEOUT_MS";
const DEFAULT_INTEGRATION_ID: &str = "home-assistant";
const CLOUDLINK_INTEGRATION_EXTENSION_ID: &str = "aether.cloudlink.integration.v1alpha1";
const CLOUDLINK_INTEGRATION_CONTROL_EXTENSION_ID: &str =
    "aether.cloudlink.integration-control.v1alpha1";
const DEFAULT_CLOUDLINK_SPOOL_CAPACITY: usize = 4_096;
const DEFAULT_CLOUDLINK_CHALLENGE_LEDGER_CAPACITY: usize = 256;
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CHALLENGE_REQUEST_LIFETIME_MS: u64 = 60_000;
#[cfg(feature = "home-assistant-cloudlink")]
const INITIAL_CLOUDLINK_SESSION_RETRY_DELAY: Duration = Duration::from_millis(250);
#[cfg(feature = "home-assistant-cloudlink")]
const MAX_CLOUDLINK_SESSION_RETRY_DELAY: Duration = Duration::from_secs(2);
const DEFAULT_CONTROL_PROVIDER_TIMEOUT_MS: u64 = 5_000;
#[cfg(feature = "home-assistant")]
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
#[cfg(feature = "home-assistant")]
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Validated settings for one enabled Home Assistant connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeAssistantEnabledConfig {
    origin: String,
    access_token_ref: SecretRef,
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    generation_store_path: PathBuf,
    cloudlink: Option<Box<HomeAssistantCloudLinkConfig>>,
    integration_control: Option<Box<HomeAssistantIntegrationControlConfig>>,
}

impl HomeAssistantEnabledConfig {
    /// Returns the Home Assistant HTTP(S) origin.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Returns the external credential reference, never credential material.
    #[must_use]
    pub const fn access_token_ref(&self) -> &SecretRef {
        &self.access_token_ref
    }

    /// Returns the owning AetherEdge gateway identity.
    #[must_use]
    pub const fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    /// Returns the stable identity of this Home Assistant connection.
    #[must_use]
    pub const fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    /// Returns the absolute path of the restart-stable topology generation store.
    #[must_use]
    pub fn generation_store_path(&self) -> &Path {
        &self.generation_store_path
    }

    /// Returns the explicit Cloud-first uplink configuration, when enabled.
    #[must_use]
    pub fn cloudlink(&self) -> Option<&HomeAssistantCloudLinkConfig> {
        self.cloudlink.as_deref()
    }

    /// Returns the explicit governed control configuration, when enabled.
    #[must_use]
    pub fn integration_control(&self) -> Option<&HomeAssistantIntegrationControlConfig> {
        self.integration_control.as_deref()
    }
}

/// Validated, non-secret settings for default-off governed Integration control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeAssistantIntegrationControlConfig {
    ledger_path: PathBuf,
    policy_path: PathBuf,
    audit_path: PathBuf,
    cloud_key_id: String,
    cloud_public_key_ref: SecretRef,
    legacy_receipt_signing: Option<LegacyControlReceiptSigningConfig>,
    provider_timeout: Duration,
}

/// Test-only receipt signer retained for the externally attested connector harness.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyControlReceiptSigningConfig {
    key_id: String,
    signing_key_ref: SecretRef,
}

impl HomeAssistantIntegrationControlConfig {
    fn from_lookup(
        lookup: &mut impl FnMut(&str) -> Option<String>,
        cloudlink: Option<&HomeAssistantCloudLinkConfig>,
        enabled: bool,
    ) -> Result<Option<Self>, HomeAssistantStartupError> {
        if !enabled {
            return Ok(None);
        }
        let Some(cloudlink) = cloudlink else {
            return Err(HomeAssistantStartupError::IntegrationControlRequiresCloudLink);
        };
        if required_setting(lookup, CONTROL_CLOUD_EXTENSION_SETTING)?
            != CLOUDLINK_INTEGRATION_CONTROL_EXTENSION_ID
        {
            return Err(HomeAssistantStartupError::InvalidIntegrationControlCloudExtension);
        }
        let ledger_path = absolute_file(lookup, CONTROL_LEDGER_PATH_SETTING)?;
        let policy_path = absolute_file(lookup, CONTROL_POLICY_PATH_SETTING)?;
        let audit_path = absolute_file(lookup, CONTROL_AUDIT_PATH_SETTING)?;
        if ledger_path == policy_path || ledger_path == audit_path || policy_path == audit_path {
            return Err(HomeAssistantStartupError::IntegrationControlPathsOverlap);
        }
        let cloud_key_id = identifier_setting(lookup, CONTROL_CLOUD_KEY_ID_SETTING)?;
        let cloud_public_key_ref =
            environment_secret_reference(lookup, CONTROL_CLOUD_PUBLIC_KEY_REF_SETTING)?;
        let legacy_receipt_signing = legacy_control_receipt_signing_config(cloudlink, lookup)?;
        let provider_timeout_ms = match lookup(CONTROL_PROVIDER_TIMEOUT_MS_SETTING) {
            None => DEFAULT_CONTROL_PROVIDER_TIMEOUT_MS,
            Some(value) => value.parse::<u64>().map_err(|_| {
                HomeAssistantStartupError::InvalidPositiveInteger {
                    name: CONTROL_PROVIDER_TIMEOUT_MS_SETTING,
                }
            })?,
        };
        if !(1..=30_000).contains(&provider_timeout_ms) {
            return Err(HomeAssistantStartupError::InvalidPositiveInteger {
                name: CONTROL_PROVIDER_TIMEOUT_MS_SETTING,
            });
        }
        Ok(Some(Self {
            ledger_path,
            policy_path,
            audit_path,
            cloud_key_id,
            cloud_public_key_ref,
            legacy_receipt_signing,
            provider_timeout: Duration::from_millis(provider_timeout_ms),
        }))
    }

    /// Returns the process-exclusive durable job and receipt ledger path.
    #[must_use]
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// Returns the required deny-by-default local authorization policy path.
    #[must_use]
    pub fn policy_path(&self) -> &Path {
        &self.policy_path
    }

    /// Returns the required durable local audit journal path.
    #[must_use]
    pub fn audit_path(&self) -> &Path {
        &self.audit_path
    }

    #[cfg(feature = "home-assistant-integration-control")]
    pub(crate) fn cloud_key_id(&self) -> &str {
        &self.cloud_key_id
    }

    #[cfg(feature = "home-assistant-integration-control")]
    pub(crate) const fn cloud_public_key_ref(&self) -> &SecretRef {
        &self.cloud_public_key_ref
    }

    #[cfg(feature = "home-assistant-integration-control")]
    pub(crate) fn legacy_receipt_signing(&self) -> Option<(&str, &SecretRef)> {
        self.legacy_receipt_signing
            .as_ref()
            .map(|config| (config.key_id.as_str(), &config.signing_key_ref))
    }

    #[cfg(feature = "home-assistant-integration-control")]
    pub(crate) const fn provider_timeout(&self) -> Duration {
        self.provider_timeout
    }
}

/// Explicit session-origin model for the optional CloudLink uplink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeAssistantCloudLinkOriginModel {
    /// Cloud challenges and Gateway hellos are independently Ed25519 signed.
    GatewaySigned,
    /// Test-only external broker attestation; forbidden by production composition.
    TrustedConnectorBrokerAttestation,
}

#[derive(Clone, PartialEq, Eq)]
enum HomeAssistantCloudLinkAuthenticationConfig {
    GatewaySigned {
        cloud_key_id: String,
        cloud_public_key_ref: SecretRef,
        gateway_key_id: String,
        gateway_signing_key_ref: SecretRef,
        challenge_ledger_path: PathBuf,
    },
    TrustedConnectorBrokerAttestation,
}

impl core::fmt::Debug for HomeAssistantCloudLinkAuthenticationConfig {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("HomeAssistantCloudLinkAuthenticationConfig([REDACTED])")
    }
}

/// Validated, non-secret settings for the optional Integration CloudLink uplink.
#[derive(Clone, PartialEq, Eq)]
pub struct HomeAssistantCloudLinkConfig {
    runtime_config_directory: PathBuf,
    topology_spool_path: PathBuf,
    observation_spool_path: PathBuf,
    spool_capacity: usize,
    mqtt_broker_host: String,
    mqtt_broker_port: u16,
    mqtt_client_id: String,
    mqtt_topic_prefix: String,
    mqtt_username: Option<String>,
    mqtt_password_ref: Option<SecretRef>,
    credential_id: String,
    credential_generation: u64,
    session_epoch_path: PathBuf,
    authentication: HomeAssistantCloudLinkAuthenticationConfig,
}

impl core::fmt::Debug for HomeAssistantCloudLinkConfig {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HomeAssistantCloudLinkConfig")
            .field("authentication", &"[REDACTED]")
            .field("spool_capacity", &self.spool_capacity)
            .finish_non_exhaustive()
    }
}

impl HomeAssistantCloudLinkConfig {
    fn from_lookup(
        lookup: &mut impl FnMut(&str) -> Option<String>,
    ) -> Result<Option<Self>, HomeAssistantStartupError> {
        if !boolean_setting(lookup, CLOUDLINK_ENABLED_SETTING, false)? {
            return Ok(None);
        }
        if lookup(CLOUDLINK_MQTT_PLAINTEXT_PASSWORD_SETTING).is_some() {
            return Err(HomeAssistantStartupError::CloudLinkPlaintextPasswordForbidden);
        }
        let origin_model = required_setting(lookup, CLOUDLINK_ORIGIN_MODEL_SETTING)?;
        let authentication = match origin_model.as_str() {
            "gateway-signed" => {
                let cloud_key_id =
                    cloudlink_identifier_setting(lookup, CLOUDLINK_CLOUD_KEY_ID_SETTING)?;
                let cloud_public_key_ref = cloudlink_environment_secret_reference(
                    lookup,
                    CLOUDLINK_CLOUD_PUBLIC_KEY_REF_SETTING,
                )?;
                let gateway_key_id =
                    cloudlink_identifier_setting(lookup, CLOUDLINK_GATEWAY_KEY_ID_SETTING)?;
                let gateway_signing_key_ref = cloudlink_environment_secret_reference(
                    lookup,
                    CLOUDLINK_GATEWAY_SIGNING_KEY_REF_SETTING,
                )?;
                let challenge_ledger_path =
                    absolute_file(lookup, CLOUDLINK_CHALLENGE_LEDGER_PATH_SETTING)?;
                HomeAssistantCloudLinkAuthenticationConfig::GatewaySigned {
                    cloud_key_id,
                    cloud_public_key_ref,
                    gateway_key_id,
                    gateway_signing_key_ref,
                    challenge_ledger_path,
                }
            },
            "trusted-connector-broker-attestation" => {
                HomeAssistantCloudLinkAuthenticationConfig::TrustedConnectorBrokerAttestation
            },
            _ => return Err(HomeAssistantStartupError::InvalidCloudLinkOriginModel),
        };
        let runtime_config_directory =
            absolute_directory(lookup, CLOUDLINK_RUNTIME_CONFIG_DIR_SETTING)?;
        if required_setting(lookup, CLOUDLINK_CLOUD_EXTENSION_SETTING)?
            != CLOUDLINK_INTEGRATION_EXTENSION_ID
        {
            return Err(HomeAssistantStartupError::InvalidCloudLinkCloudExtension);
        }
        let topology_spool_path = absolute_file(lookup, CLOUDLINK_TOPOLOGY_SPOOL_PATH_SETTING)?;
        let observation_spool_path =
            absolute_file(lookup, CLOUDLINK_OBSERVATION_SPOOL_PATH_SETTING)?;
        if topology_spool_path == observation_spool_path {
            return Err(HomeAssistantStartupError::CloudLinkSpoolPathsOverlap);
        }
        let spool_capacity = match lookup(CLOUDLINK_SPOOL_CAPACITY_SETTING) {
            None => DEFAULT_CLOUDLINK_SPOOL_CAPACITY,
            Some(value) => value.parse::<usize>().map_err(|_| {
                HomeAssistantStartupError::InvalidPositiveInteger {
                    name: CLOUDLINK_SPOOL_CAPACITY_SETTING,
                }
            })?,
        };
        if !(1..=65_536).contains(&spool_capacity) {
            return Err(HomeAssistantStartupError::InvalidPositiveInteger {
                name: CLOUDLINK_SPOOL_CAPACITY_SETTING,
            });
        }
        let mqtt_broker_host = required_setting(lookup, CLOUDLINK_MQTT_HOST_SETTING)?;
        let mqtt_broker_port = positive_integer::<u16>(lookup, CLOUDLINK_MQTT_PORT_SETTING)?;
        let mqtt_client_id = required_setting(lookup, CLOUDLINK_MQTT_CLIENT_ID_SETTING)?;
        let mqtt_topic_prefix = required_setting(lookup, CLOUDLINK_MQTT_TOPIC_PREFIX_SETTING)?;
        let mqtt_username = Some(required_setting(lookup, CLOUDLINK_MQTT_USERNAME_SETTING)?);
        let reference = required_setting(lookup, CLOUDLINK_MQTT_PASSWORD_REF_SETTING)?;
        if !is_environment_secret_reference(&reference) {
            return Err(HomeAssistantStartupError::InvalidCloudLinkPasswordReference);
        }
        let mqtt_password_ref = Some(
            SecretRef::new(reference)
                .map_err(|_| HomeAssistantStartupError::InvalidCloudLinkPasswordReference)?,
        );
        let credential_id = required_setting(lookup, CLOUDLINK_CREDENTIAL_ID_SETTING)?;
        let credential_generation =
            positive_integer::<u64>(lookup, CLOUDLINK_CREDENTIAL_GENERATION_SETTING)?;
        let session_epoch_path = absolute_file(lookup, CLOUDLINK_SESSION_EPOCH_PATH_SETTING)?;
        if let HomeAssistantCloudLinkAuthenticationConfig::GatewaySigned {
            challenge_ledger_path,
            ..
        } = &authentication
            && (challenge_ledger_path == &topology_spool_path
                || challenge_ledger_path == &observation_spool_path
                || challenge_ledger_path == &session_epoch_path)
        {
            return Err(HomeAssistantStartupError::CloudLinkAuthenticationPathsOverlap);
        }
        Ok(Some(Self {
            runtime_config_directory,
            topology_spool_path,
            observation_spool_path,
            spool_capacity,
            mqtt_broker_host,
            mqtt_broker_port,
            mqtt_client_id,
            mqtt_topic_prefix,
            mqtt_username,
            mqtt_password_ref,
            credential_id,
            credential_generation,
            session_epoch_path,
            authentication,
        }))
    }

    /// Returns the verified Runtime Manifest directory.
    #[must_use]
    pub fn runtime_config_directory(&self) -> &Path {
        &self.runtime_config_directory
    }

    /// Returns the topology journal path.
    #[must_use]
    pub fn topology_spool_path(&self) -> &Path {
        &self.topology_spool_path
    }

    /// Returns the observation journal path.
    #[must_use]
    pub fn observation_spool_path(&self) -> &Path {
        &self.observation_spool_path
    }

    /// Returns the explicit session origin model.
    #[must_use]
    pub const fn origin_model(&self) -> HomeAssistantCloudLinkOriginModel {
        match &self.authentication {
            HomeAssistantCloudLinkAuthenticationConfig::GatewaySigned { .. } => {
                HomeAssistantCloudLinkOriginModel::GatewaySigned
            },
            HomeAssistantCloudLinkAuthenticationConfig::TrustedConnectorBrokerAttestation => {
                HomeAssistantCloudLinkOriginModel::TrustedConnectorBrokerAttestation
            },
        }
    }

    /// Returns the owner-only challenge ledger path for Gateway-signed sessions.
    #[must_use]
    pub fn challenge_ledger_path(&self) -> Option<&Path> {
        match &self.authentication {
            HomeAssistantCloudLinkAuthenticationConfig::GatewaySigned {
                challenge_ledger_path,
                ..
            } => Some(challenge_ledger_path),
            HomeAssistantCloudLinkAuthenticationConfig::TrustedConnectorBrokerAttestation => None,
        }
    }

    fn matches_gateway_signing_key(&self, key_id: &str, reference: &SecretRef) -> bool {
        matches!(
            &self.authentication,
            HomeAssistantCloudLinkAuthenticationConfig::GatewaySigned {
                gateway_key_id,
                gateway_signing_key_ref,
                ..
            } if gateway_key_id == key_id && gateway_signing_key_ref == reference
        )
    }

    #[cfg(feature = "home-assistant-integration-control")]
    pub(crate) fn mqtt_topic_prefix(&self) -> &str {
        &self.mqtt_topic_prefix
    }
}

/// Runtime configuration for the optional Home Assistant integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeAssistantRuntimeConfig {
    /// Home Assistant has no effect on the existing IO startup path.
    Disabled,
    /// Home Assistant is explicitly enabled with validated non-secret settings.
    Enabled(HomeAssistantEnabledConfig),
}

impl HomeAssistantRuntimeConfig {
    /// Reads the process environment. Absence defaults to disabled.
    pub fn from_environment() -> Result<Self, HomeAssistantStartupError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Parses settings through an injected lookup.
    ///
    /// This is public so composition tests and alternate process launchers can
    /// validate settings without mutating the process environment.
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, HomeAssistantStartupError> {
        let integration_control_requested =
            boolean_setting(&mut lookup, CONTROL_ENABLED_SETTING, false)?;
        let enabled = boolean_setting(&mut lookup, ENABLED_SETTING, false)
            .map_err(|_| HomeAssistantStartupError::InvalidEnabledValue)?;
        if !enabled {
            if integration_control_requested {
                return Err(HomeAssistantStartupError::IntegrationControlRequiresHomeAssistant);
            }
            return Ok(Self::Disabled);
        }
        if lookup(PLAINTEXT_TOKEN_SETTING).is_some() {
            return Err(HomeAssistantStartupError::PlaintextCredentialForbidden);
        }

        let origin = required_setting(&mut lookup, ORIGIN_SETTING)?;
        let reference = required_setting(&mut lookup, SECRET_REF_SETTING)?;
        if !is_environment_secret_reference(&reference) {
            return Err(HomeAssistantStartupError::UnsupportedSecretReference);
        }
        let access_token_ref = SecretRef::new(reference)
            .map_err(|_| HomeAssistantStartupError::InvalidSecretReference)?;
        let gateway_id = GatewayIdentity::new(required_setting(&mut lookup, GATEWAY_ID_SETTING)?)
            .map_err(|_| HomeAssistantStartupError::InvalidGatewayIdentity)?;
        let integration_id = IntegrationId::new(
            lookup(INTEGRATION_ID_SETTING).unwrap_or_else(|| DEFAULT_INTEGRATION_ID.to_owned()),
        )
        .map_err(|_| HomeAssistantStartupError::InvalidIntegrationIdentity)?;
        let generation_store_path = PathBuf::from(required_setting(
            &mut lookup,
            GENERATION_STORE_PATH_SETTING,
        )?);
        if !generation_store_path.is_absolute() || generation_store_path.file_name().is_none() {
            return Err(HomeAssistantStartupError::InvalidGenerationStorePath);
        }
        let cloudlink = HomeAssistantCloudLinkConfig::from_lookup(&mut lookup)?.map(Box::new);
        let integration_control = HomeAssistantIntegrationControlConfig::from_lookup(
            &mut lookup,
            cloudlink.as_deref(),
            integration_control_requested,
        )?
        .map(Box::new);

        Ok(Self::Enabled(HomeAssistantEnabledConfig {
            origin,
            access_token_ref,
            gateway_id,
            integration_id,
            generation_store_path,
            cloudlink,
            integration_control,
        }))
    }

    /// Returns enabled settings, if explicitly commissioned.
    #[must_use]
    pub const fn enabled(&self) -> Option<&HomeAssistantEnabledConfig> {
        match self {
            Self::Disabled => None,
            Self::Enabled(config) => Some(config),
        }
    }
}

fn required_setting(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, HomeAssistantStartupError> {
    lookup(name)
        .filter(|value| !value.trim().is_empty() && value.trim() == value)
        .ok_or(HomeAssistantStartupError::MissingSetting { name })
}

fn boolean_setting(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
    default: bool,
) -> Result<bool, HomeAssistantStartupError> {
    match lookup(name) {
        None => Ok(default),
        Some(value) if value == "true" || value == "1" => Ok(true),
        Some(value) if value == "false" || value == "0" => Ok(false),
        Some(_) => Err(HomeAssistantStartupError::InvalidBooleanSetting { name }),
    }
}

fn absolute_file(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<PathBuf, HomeAssistantStartupError> {
    let path = PathBuf::from(required_setting(lookup, name)?);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(HomeAssistantStartupError::InvalidAbsolutePath { name });
    }
    Ok(path)
}

fn absolute_directory(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<PathBuf, HomeAssistantStartupError> {
    let path = PathBuf::from(required_setting(lookup, name)?);
    if !path.is_absolute() {
        return Err(HomeAssistantStartupError::InvalidAbsolutePath { name });
    }
    Ok(path)
}

fn positive_integer<T>(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<T, HomeAssistantStartupError>
where
    T: std::str::FromStr + PartialEq + Default,
{
    let value = required_setting(lookup, name)?
        .parse::<T>()
        .map_err(|_| HomeAssistantStartupError::InvalidPositiveInteger { name })?;
    if value == T::default() {
        return Err(HomeAssistantStartupError::InvalidPositiveInteger { name });
    }
    Ok(value)
}

fn is_environment_secret_reference(reference: &str) -> bool {
    let Some(variable) = reference.strip_prefix("env:") else {
        return false;
    };
    let mut bytes = variable.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

fn environment_secret_reference(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<SecretRef, HomeAssistantStartupError> {
    let reference = required_setting(lookup, name)?;
    if !is_environment_secret_reference(&reference) {
        return Err(HomeAssistantStartupError::InvalidIntegrationControlSecretReference { name });
    }
    SecretRef::new(reference)
        .map_err(|_| HomeAssistantStartupError::InvalidIntegrationControlSecretReference { name })
}

fn cloudlink_environment_secret_reference(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<SecretRef, HomeAssistantStartupError> {
    let reference = required_setting(lookup, name)?;
    if !is_environment_secret_reference(&reference) {
        return Err(
            HomeAssistantStartupError::InvalidCloudLinkAuthenticationSecretReference { name },
        );
    }
    SecretRef::new(reference).map_err(|_| {
        HomeAssistantStartupError::InvalidCloudLinkAuthenticationSecretReference { name }
    })
}

fn cloudlink_identifier_setting(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, HomeAssistantStartupError> {
    let value = required_setting(lookup, name)?;
    let mut bytes = value.bytes();
    if value.len() > 128
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(HomeAssistantStartupError::InvalidCloudLinkAuthenticationKeyId { name });
    }
    Ok(value)
}

fn identifier_setting(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, HomeAssistantStartupError> {
    let value = required_setting(lookup, name)?;
    let mut bytes = value.bytes();
    if value.len() > 128
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(HomeAssistantStartupError::InvalidIntegrationControlKeyId { name });
    }
    Ok(value)
}

fn legacy_control_receipt_signing_config(
    cloudlink: &HomeAssistantCloudLinkConfig,
    lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Result<Option<LegacyControlReceiptSigningConfig>, HomeAssistantStartupError> {
    let key_id = lookup(CONTROL_EDGE_KEY_ID_SETTING)
        .map(|value| validate_control_key_id(value, CONTROL_EDGE_KEY_ID_SETTING))
        .transpose()?;
    let key_reference = lookup(CONTROL_EDGE_SIGNING_KEY_REF_SETTING)
        .map(|value| validate_control_secret_reference(value, CONTROL_EDGE_SIGNING_KEY_REF_SETTING))
        .transpose()?;
    match (key_id.as_deref(), key_reference.as_ref()) {
        (None, None) => Ok(None),
        (Some(key_id), Some(key_reference))
            if cloudlink.matches_gateway_signing_key(key_id, key_reference) =>
        {
            Ok(None)
        },
        (Some(key_id), Some(key_reference))
            if cloudlink.origin_model()
                == HomeAssistantCloudLinkOriginModel::TrustedConnectorBrokerAttestation =>
        {
            Ok(Some(LegacyControlReceiptSigningConfig {
                key_id: key_id.to_owned(),
                signing_key_ref: key_reference.clone(),
            }))
        },
        (Some(_), Some(_)) | (Some(_), None) | (None, Some(_)) => {
            Err(HomeAssistantStartupError::IntegrationControlReceiptSigningKeyConflict)
        },
    }
}

fn validate_control_key_id(
    value: String,
    name: &'static str,
) -> Result<String, HomeAssistantStartupError> {
    let mut bytes = value.bytes();
    if value.len() > 128
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(HomeAssistantStartupError::InvalidIntegrationControlKeyId { name });
    }
    Ok(value)
}

fn validate_control_secret_reference(
    value: String,
    name: &'static str,
) -> Result<SecretRef, HomeAssistantStartupError> {
    if !is_environment_secret_reference(&value) {
        return Err(HomeAssistantStartupError::InvalidIntegrationControlSecretReference { name });
    }
    SecretRef::new(value)
        .map_err(|_| HomeAssistantStartupError::InvalidIntegrationControlSecretReference { name })
}

/// Typed failure raised while composing or stopping the optional integration.
#[derive(Debug, Error)]
pub enum HomeAssistantStartupError {
    /// The explicit enable switch was not a supported boolean spelling.
    #[error("{ENABLED_SETTING} must be true, false, 1, or 0")]
    InvalidEnabledValue,
    /// An optional boolean setting used an unsupported spelling.
    #[error("{name} must be true, false, 1, or 0")]
    InvalidBooleanSetting {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// Plaintext credential configuration is forbidden.
    #[error(
        "{PLAINTEXT_TOKEN_SETTING} is forbidden; configure {SECRET_REF_SETTING} with a secret reference"
    )]
    PlaintextCredentialForbidden,
    /// Broker password material may only be resolved through a reference.
    #[error(
        "{CLOUDLINK_MQTT_PLAINTEXT_PASSWORD_SETTING} is forbidden; use {CLOUDLINK_MQTT_PASSWORD_REF_SETTING}"
    )]
    CloudLinkPlaintextPasswordForbidden,
    /// CloudLink session origin must be selected; there is no trusted default.
    #[error(
        "{CLOUDLINK_ORIGIN_MODEL_SETTING} must be gateway-signed or trusted-connector-broker-attestation"
    )]
    InvalidCloudLinkOriginModel,
    /// A Gateway-signed key reference must use the closed environment form.
    #[error("{name} must use env:VARIABLE_NAME")]
    InvalidCloudLinkAuthenticationSecretReference {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// A configured session key identity is malformed.
    #[error("{name} must be a bounded CloudLink identifier")]
    InvalidCloudLinkAuthenticationKeyId {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// Session auth persistence cannot alias another CloudLink state file.
    #[error("Home Assistant CloudLink authentication and stream state paths must be distinct")]
    CloudLinkAuthenticationPathsOverlap,
    /// One required setting was absent or malformed.
    #[error("required Home Assistant setting {name} is missing")]
    MissingSetting {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// The secret reference itself violated the bounded reference contract.
    #[error("{SECRET_REF_SETTING} is invalid")]
    InvalidSecretReference,
    /// This composition root currently supports the environment resolver only.
    #[error("{SECRET_REF_SETTING} must use env:VARIABLE_NAME")]
    UnsupportedSecretReference,
    /// The gateway identity violated the domain identity contract.
    #[error("{GATEWAY_ID_SETTING} is invalid")]
    InvalidGatewayIdentity,
    /// The integration identity violated the domain identity contract.
    #[error("{INTEGRATION_ID_SETTING} is invalid")]
    InvalidIntegrationIdentity,
    /// Persistent topology generations require an explicit unambiguous location.
    #[error("{GENERATION_STORE_PATH_SETTING} must be an absolute file path")]
    InvalidGenerationStorePath,
    /// A CloudLink path must be explicit and absolute.
    #[error("{name} must be an absolute path")]
    InvalidAbsolutePath {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// A bounded positive integer setting was malformed.
    #[error("{name} must be a positive integer within its supported bound")]
    InvalidPositiveInteger {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// Cloud-first activation requires the exact public extension identifier.
    #[error("{CLOUDLINK_CLOUD_EXTENSION_SETTING} must equal {CLOUDLINK_INTEGRATION_EXTENSION_ID}")]
    InvalidCloudLinkCloudExtension,
    /// Topology and observation facts must never share one journal.
    #[error("Home Assistant CloudLink topology and observation spool paths must be distinct")]
    CloudLinkSpoolPathsOverlap,
    /// Broker password references are closed environment references.
    #[error("{CLOUDLINK_MQTT_PASSWORD_REF_SETTING} must use env:VARIABLE_NAME")]
    InvalidCloudLinkPasswordReference,
    /// Runtime configuration requested an adapter omitted from this binary.
    #[error("Home Assistant is enabled but aether-io was built without the home-assistant feature")]
    FeatureNotCompiled,
    /// Runtime configuration requested CloudLink from a binary that omitted it.
    #[error(
        "Home Assistant CloudLink is enabled but aether-io was built without the home-assistant-cloudlink feature"
    )]
    CloudLinkFeatureNotCompiled,
    /// The extension rejected connection settings before its task started.
    #[error("Home Assistant connection configuration is invalid: {0}")]
    InvalidConnectionConfiguration(PortError),
    /// The restart-stable generation store could not be opened safely.
    #[error("Home Assistant topology generation store is unavailable: {0}")]
    GenerationStoreUnavailable(PortError),
    /// Runtime Manifest loading or release verification failed.
    #[error("Home Assistant CloudLink Runtime Manifest is unavailable: {0}")]
    CloudLinkRuntimeManifestUnavailable(String),
    /// The built artifact did not explicitly declare the alpha extension.
    #[error(
        "Runtime Manifest does not declare {CLOUDLINK_INTEGRATION_EXTENSION_ID}; refuse CloudLink startup"
    )]
    CloudLinkRuntimeManifestMissingExtension,
    /// One of the two crash-recoverable stream journals could not be opened.
    #[error("Home Assistant CloudLink durable spool is unavailable: {0}")]
    CloudLinkSpoolUnavailable(String),
    /// MQTT/TLS/topic settings failed closed before a task was spawned.
    #[error("Home Assistant CloudLink MQTT configuration is invalid: {0}")]
    CloudLinkMqttConfiguration(String),
    /// A referenced broker secret could not be resolved without exposing it.
    #[error("Home Assistant CloudLink MQTT credential reference cannot be resolved")]
    CloudLinkSecretUnavailable,
    /// Test-only trusted connector mode cannot enter the production composition root.
    #[error("trusted-connector-broker-attestation is forbidden for production CloudLink sessions")]
    CloudLinkTrustedConnectorForbiddenInProduction,
    /// A referenced session key is absent.
    #[error("Home Assistant CloudLink authentication key reference cannot be resolved")]
    CloudLinkAuthenticationSecretUnavailable,
    /// Configured session Ed25519 material is not canonical or usable.
    #[error("Home Assistant CloudLink Ed25519 authentication key material is invalid")]
    InvalidCloudLinkAuthenticationKeyMaterial,
    /// Owner-only replay state could not be opened or validated.
    #[error("Home Assistant CloudLink challenge replay ledger is unavailable")]
    CloudLinkChallengeLedgerUnavailable,
    /// The monotonic session epoch checkpoint is invalid or unavailable.
    #[error("Home Assistant CloudLink session epoch store is unavailable: {0}")]
    CloudLinkSessionEpochUnavailable(String),
    /// Governed control can only bind to the authenticated CloudLink session.
    #[error(
        "{CONTROL_ENABLED_SETTING}=true requires {CLOUDLINK_ENABLED_SETTING}=true and complete CloudLink settings"
    )]
    IntegrationControlRequiresCloudLink,
    /// Governed control cannot implicitly enable the read-only provider integration.
    #[error("{CONTROL_ENABLED_SETTING}=true requires {ENABLED_SETTING}=true")]
    IntegrationControlRequiresHomeAssistant,
    /// Activation requires the exact experimental runtime protocol token.
    #[error(
        "{CONTROL_CLOUD_EXTENSION_SETTING} must equal {CLOUDLINK_INTEGRATION_CONTROL_EXTENSION_ID}"
    )]
    InvalidIntegrationControlCloudExtension,
    /// Durable control state, local policy, and audit evidence need distinct files.
    #[error("Home Assistant Integration-control ledger, policy, and audit paths must be distinct")]
    IntegrationControlPathsOverlap,
    /// Deprecated receipt-key aliases must identify the session Gateway signer exactly.
    #[error(
        "deprecated Integration-control receipt key settings must both be absent or exactly match the CloudLink session Gateway key"
    )]
    IntegrationControlReceiptSigningKeyConflict,
    /// Control key material may only be resolved through an environment secret reference.
    #[error("{name} must use env:VARIABLE_NAME")]
    InvalidIntegrationControlSecretReference {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// A configured Ed25519 key identity is malformed.
    #[error("{name} must be a bounded CloudLink identifier")]
    InvalidIntegrationControlKeyId {
        /// Stable environment setting name.
        name: &'static str,
    },
    /// Runtime control was configured but omitted from this binary.
    #[error(
        "Home Assistant Integration-control is enabled but aether-io was built without the home-assistant-integration-control feature"
    )]
    IntegrationControlFeatureNotCompiled,
    /// The verified Runtime Manifest omitted the explicit control protocol token.
    #[error(
        "Runtime Manifest does not declare {CLOUDLINK_INTEGRATION_CONTROL_EXTENSION_ID}; refuse Integration-control startup"
    )]
    IntegrationControlRuntimeManifestMissingExtension,
    /// Persistent governed job and receipt state could not be opened exclusively.
    #[error("Home Assistant Integration-control durable ledger is unavailable")]
    IntegrationControlLedgerUnavailable,
    /// Required local authorization policy was absent, malformed, or out of scope.
    #[error("Home Assistant Integration-control local authorization policy is unavailable")]
    IntegrationControlPolicyUnavailable,
    /// Required append-only local audit evidence could not be opened.
    #[error("Home Assistant Integration-control audit journal is unavailable")]
    IntegrationControlAuditUnavailable,
    /// A configured key reference could not be resolved.
    #[error("Home Assistant Integration-control key reference cannot be resolved")]
    IntegrationControlSecretUnavailable,
    /// Configured Ed25519 key material was not canonical unpadded Base64url.
    #[error("Home Assistant Integration-control Ed25519 key material is invalid")]
    InvalidIntegrationControlKeyMaterial,
    /// The fixed control MQTT namespace was invalid.
    #[error("Home Assistant Integration-control MQTT topic configuration is invalid")]
    IntegrationControlTopicConfiguration,
    /// The bounded provider deadline or processor activation was invalid.
    #[error("Home Assistant Integration-control runtime configuration is invalid")]
    InvalidIntegrationControlConfiguration,
    /// A spawned integration task panicked or was cancelled unexpectedly.
    #[error("Home Assistant integration task failed: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
    /// The integration task did not stop within the bounded shutdown window.
    #[error("Home Assistant integration task shutdown timed out")]
    TaskShutdownTimeout,
}

/// Atomic, read-only, process-local delegated integration projection.
///
/// This projection is intentionally separate from SHM: Home Assistant remains
/// authoritative for delegated state, while SHM remains authoritative for
/// commissioned Aether live numeric points.
#[derive(Debug, Default)]
pub struct InMemoryIntegrationProjection {
    snapshot: RwLock<Option<IntegrationSnapshot>>,
}

impl InMemoryIntegrationProjection {
    fn last_sequence(snapshot: &IntegrationSnapshot) -> Option<u64> {
        snapshot
            .observations()
            .iter()
            .map(IntegrationObservation::sequence)
            .max()
    }
}

#[async_trait]
impl IntegrationProjectionSink for InMemoryIntegrationProjection {
    async fn replace_snapshot(
        &self,
        snapshot: IntegrationSnapshot,
    ) -> PortResult<IntegrationProjectionReceipt> {
        let topology = snapshot.topology();
        let gateway_id = topology.gateway_id().clone();
        let integration_id = topology.integration_id().clone();
        let generation = topology.generation();
        let observation_count = snapshot.observations().len();
        let last_sequence = Self::last_sequence(&snapshot);

        let mut current = self.snapshot.write().await;
        if let Some(existing) = current.as_ref() {
            if existing.topology().gateway_id() != &gateway_id
                || existing.topology().integration_id() != &integration_id
            {
                return Err(conflict("integration projection scope changed"));
            }
            if generation < existing.topology().generation() {
                return Err(conflict("integration topology generation did not advance"));
            }
            if generation == existing.topology().generation()
                && topology.digest() != existing.topology().digest()
            {
                return Err(conflict(
                    "integration topology digest changed without a new generation",
                ));
            }
        }
        *current = Some(snapshot);

        Ok(IntegrationProjectionReceipt::snapshot_replaced(
            gateway_id,
            integration_id,
            generation,
            observation_count,
            last_sequence,
        ))
    }

    async fn apply_observation(
        &self,
        expected_generation: TopologyGeneration,
        observation: IntegrationObservation,
    ) -> PortResult<IntegrationProjectionReceipt> {
        let mut current = self.snapshot.write().await;
        let projected = current
            .as_ref()
            .ok_or_else(|| conflict("integration projection is not initialized"))?;
        if projected.topology().generation() != expected_generation {
            return Err(conflict("integration topology generation changed"));
        }
        if observation.gateway_id() != projected.topology().gateway_id()
            || observation.integration_id() != projected.topology().integration_id()
        {
            return Err(conflict("integration observation scope changed"));
        }
        let expected_sequence = Self::last_sequence(projected)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| conflict("integration observation sequence exhausted"))?;
        if observation.sequence() != expected_sequence {
            return Err(conflict("integration observation sequence has a gap"));
        }

        let (topology, mut observations) = projected.clone().into_parts();
        if let Some(existing) = observations.iter_mut().find(|existing| {
            existing.entity_id() == observation.entity_id()
                && existing.point_key() == observation.point_key()
        }) {
            *existing = observation.clone();
        } else {
            observations.push(observation.clone());
        }
        let next = IntegrationSnapshot::new(topology, observations)
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let receipt = IntegrationProjectionReceipt::observation_applied(
            observation.gateway_id().clone(),
            observation.integration_id().clone(),
            expected_generation,
            observation.sequence(),
        );
        *current = Some(next);
        Ok(receipt)
    }
}

#[async_trait]
impl IntegrationProjectionQuery for InMemoryIntegrationProjection {
    async fn snapshot(
        &self,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>> {
        Ok(self.snapshot.read().await.clone().filter(|snapshot| {
            snapshot.topology().gateway_id() == gateway_id
                && snapshot.topology().integration_id() == integration_id
        }))
    }
}

fn conflict(message: &str) -> PortError {
    PortError::new(PortErrorKind::Conflict, message)
}

/// Owned lifecycle handle for the optional integration synchronization task.
#[derive(Debug)]
pub struct HomeAssistantIntegrationRuntime {
    task: JoinHandle<()>,
}

impl HomeAssistantIntegrationRuntime {
    /// Waits for the already-cancelled task and aborts it on bounded timeout.
    pub async fn shutdown(mut self) -> Result<(), HomeAssistantStartupError> {
        match tokio::time::timeout(Duration::from_secs(2), &mut self.task).await {
            Ok(result) => result.map_err(HomeAssistantStartupError::TaskJoin),
            Err(_) => {
                self.task.abort();
                let _ = self.task.await;
                Err(HomeAssistantStartupError::TaskShutdownTimeout)
            },
        }
    }
}

/// Composes the optional Home Assistant bridge from the process environment.
pub fn start_home_assistant_integration(
    shutdown: CancellationToken,
) -> Result<Option<HomeAssistantIntegrationRuntime>, HomeAssistantStartupError> {
    start_home_assistant_integration_with_config(
        HomeAssistantRuntimeConfig::from_environment()?,
        shutdown,
    )
}

/// Composes the optional bridge from already validated configuration.
pub fn start_home_assistant_integration_with_config(
    config: HomeAssistantRuntimeConfig,
    shutdown: CancellationToken,
) -> Result<Option<HomeAssistantIntegrationRuntime>, HomeAssistantStartupError> {
    let HomeAssistantRuntimeConfig::Enabled(config) = config else {
        return Ok(None);
    };
    start_enabled_integration(config, shutdown).map(Some)
}

#[cfg(not(feature = "home-assistant"))]
fn start_enabled_integration(
    _config: HomeAssistantEnabledConfig,
    _shutdown: CancellationToken,
) -> Result<HomeAssistantIntegrationRuntime, HomeAssistantStartupError> {
    Err(HomeAssistantStartupError::FeatureNotCompiled)
}

#[cfg(feature = "home-assistant")]
fn start_enabled_integration(
    config: HomeAssistantEnabledConfig,
    shutdown: CancellationToken,
) -> Result<HomeAssistantIntegrationRuntime, HomeAssistantStartupError> {
    #[cfg(not(feature = "home-assistant-integration-control"))]
    if config.integration_control().is_some() {
        return Err(HomeAssistantStartupError::IntegrationControlFeatureNotCompiled);
    }
    let connection =
        HomeAssistantConnectionConfig::new(config.origin(), config.access_token_ref().clone())
            .map_err(HomeAssistantStartupError::InvalidConnectionConfiguration)?;
    let generation_store: Arc<dyn IntegrationTopologyGenerationStore> = Arc::new(
        FileIntegrationTopologyGenerationStore::open(config.generation_store_path())
            .map_err(HomeAssistantStartupError::GenerationStoreUnavailable)?,
    );
    let cloudlink = match config
        .cloudlink()
        .map(|cloudlink| prepare_cloudlink_runtime(&config, cloudlink))
        .transpose()
    {
        Ok(cloudlink) => cloudlink,
        Err(error) => {
            tracing::warn!(
                reason = %error,
                "Home Assistant CloudLink extension was rejected; local Home Assistant synchronization remains active"
            );
            None
        },
    };
    let task = tokio::spawn(run_enabled_integration(
        config,
        connection,
        generation_store,
        cloudlink,
        shutdown,
    ));
    Ok(HomeAssistantIntegrationRuntime { task })
}

#[cfg(all(feature = "home-assistant", not(feature = "home-assistant-cloudlink")))]
struct PreparedCloudLinkRuntime;

#[cfg(all(feature = "home-assistant", not(feature = "home-assistant-cloudlink")))]
fn prepare_cloudlink_runtime(
    _home_assistant: &HomeAssistantEnabledConfig,
    _config: &HomeAssistantCloudLinkConfig,
) -> Result<PreparedCloudLinkRuntime, HomeAssistantStartupError> {
    Err(HomeAssistantStartupError::CloudLinkFeatureNotCompiled)
}

#[cfg(feature = "home-assistant-cloudlink")]
enum PreparedCloudLinkSessionAuthentication {
    GatewaySigned {
        authenticator: Box<GatewaySessionAuthenticator>,
        challenges: Arc<FileCloudLinkChallengeLedger>,
    },
    TrustedConnectorBrokerAttestation,
}

#[cfg(feature = "home-assistant-cloudlink")]
impl PreparedCloudLinkSessionAuthentication {
    fn uplink_authentication(&self) -> UplinkAuthentication {
        match self {
            Self::GatewaySigned { authenticator, .. } => authenticator.uplink_authentication(),
            Self::TrustedConnectorBrokerAttestation => {
                UplinkAuthentication::trusted_connector_broker_attestation()
            },
        }
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
struct PreparedCloudLinkRuntime {
    extension: CloudLinkIntegrationExtension,
    topology_spool: Arc<FileCloudLinkSpool>,
    observation_spool: Arc<FileCloudLinkSpool>,
    transport: Arc<MqttCloudLinkTransport>,
    gateway_id: String,
    credential_id: String,
    credential_generation: u64,
    session_epochs: Arc<FileSessionEpochStore>,
    session_authentication: PreparedCloudLinkSessionAuthentication,
    #[cfg(feature = "home-assistant-integration-control")]
    integration_control: Option<PreparedIntegrationControl>,
}

#[cfg(feature = "home-assistant-cloudlink")]
fn prepare_cloudlink_runtime(
    home_assistant: &HomeAssistantEnabledConfig,
    config: &HomeAssistantCloudLinkConfig,
) -> Result<PreparedCloudLinkRuntime, HomeAssistantStartupError> {
    prepare_cloudlink_runtime_with_security(
        home_assistant,
        config,
        CloudLinkTlsConfig::SystemRoots,
        DeploymentSecurity::Production,
    )
}

#[cfg(feature = "home-assistant-cloudlink")]
fn prepare_cloudlink_runtime_with_security(
    home_assistant: &HomeAssistantEnabledConfig,
    config: &HomeAssistantCloudLinkConfig,
    tls: CloudLinkTlsConfig,
    deployment_security: DeploymentSecurity,
) -> Result<PreparedCloudLinkRuntime, HomeAssistantStartupError> {
    debug_assert_eq!(
        CLOUDLINK_INTEGRATION_EXTENSION,
        CLOUDLINK_INTEGRATION_EXTENSION_ID
    );
    let manifest = aether_runtime_catalog::load_runtime_manifest(
        config.runtime_config_directory(),
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| {
        HomeAssistantStartupError::CloudLinkRuntimeManifestUnavailable(error.to_string())
    })?;
    if !manifest
        .protocols()
        .any(|protocol| protocol == CLOUDLINK_INTEGRATION_EXTENSION)
    {
        return Err(HomeAssistantStartupError::CloudLinkRuntimeManifestMissingExtension);
    }
    let session_authentication =
        prepare_cloudlink_session_authentication(config, deployment_security)?;
    #[cfg(feature = "home-assistant-integration-control")]
    if home_assistant.integration_control().is_some()
        && !manifest
            .protocols()
            .any(|protocol| protocol == CLOUDLINK_INTEGRATION_CONTROL_EXTENSION_ID)
    {
        return Err(HomeAssistantStartupError::IntegrationControlRuntimeManifestMissingExtension);
    }

    #[cfg(feature = "home-assistant-integration-control")]
    let integration_control = home_assistant
        .integration_control()
        .map(|control| {
            PreparedIntegrationControl::prepare(
                control,
                home_assistant.gateway_id(),
                home_assistant.integration_id(),
                config.mqtt_topic_prefix(),
            )
        })
        .transpose()?;

    let topology_spool = Arc::new(
        FileCloudLinkSpool::open(
            &config.topology_spool_path,
            &format!(
                "integration-topology-{}",
                home_assistant.integration_id().as_str()
            ),
            config.spool_capacity,
        )
        .map_err(|error| HomeAssistantStartupError::CloudLinkSpoolUnavailable(error.to_string()))?,
    );
    let observation_spool = Arc::new(
        FileCloudLinkSpool::open(
            &config.observation_spool_path,
            &format!(
                "integration-observations-{}",
                home_assistant.integration_id().as_str()
            ),
            config.spool_capacity,
        )
        .map_err(|error| HomeAssistantStartupError::CloudLinkSpoolUnavailable(error.to_string()))?,
    );
    let extension = CloudLinkIntegrationExtension::enable_cloud_first(
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        home_assistant.integration_id().as_str(),
        &topology_spool.current_status().map_err(|error| {
            HomeAssistantStartupError::CloudLinkSpoolUnavailable(error.to_string())
        })?,
        &observation_spool.current_status().map_err(|error| {
            HomeAssistantStartupError::CloudLinkSpoolUnavailable(error.to_string())
        })?,
    )
    .map_err(|error| HomeAssistantStartupError::CloudLinkMqttConfiguration(error.to_string()))?;

    let password = config
        .mqtt_password_ref
        .as_ref()
        .map(resolve_environment_secret)
        .transpose()?
        .map(SecretString::new);
    let mqtt = CloudLinkMqttConfig {
        broker_host: config.mqtt_broker_host.clone(),
        broker_port: config.mqtt_broker_port,
        client_id: config.mqtt_client_id.clone(),
        username: config.mqtt_username.clone(),
        password,
        tls,
        keep_alive_secs: 30,
        reconnect_delay_secs: 1,
        request_capacity: 256,
        maximum_packet_bytes: aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES,
    };
    let topics = TopicNamespace::new(
        &config.mqtt_topic_prefix,
        home_assistant.gateway_id().as_str(),
    )
    .map_err(|error| HomeAssistantStartupError::CloudLinkMqttConfiguration(error.to_string()))?;
    let transport =
        MqttCloudLinkTransport::connect(mqtt, topics, deployment_security).map_err(|error| {
            HomeAssistantStartupError::CloudLinkMqttConfiguration(error.to_string())
        })?;
    let session_epochs = Arc::new(FileSessionEpochStore::open(&config.session_epoch_path)?);

    Ok(PreparedCloudLinkRuntime {
        extension,
        topology_spool,
        observation_spool,
        transport,
        gateway_id: home_assistant.gateway_id().as_str().to_owned(),
        credential_id: config.credential_id.clone(),
        credential_generation: config.credential_generation,
        session_epochs,
        session_authentication,
        #[cfg(feature = "home-assistant-integration-control")]
        integration_control,
    })
}

#[cfg(feature = "home-assistant-cloudlink")]
fn prepare_cloudlink_session_authentication(
    config: &HomeAssistantCloudLinkConfig,
    deployment_security: DeploymentSecurity,
) -> Result<PreparedCloudLinkSessionAuthentication, HomeAssistantStartupError> {
    match &config.authentication {
        HomeAssistantCloudLinkAuthenticationConfig::GatewaySigned {
            cloud_key_id,
            cloud_public_key_ref,
            gateway_key_id,
            gateway_signing_key_ref,
            challenge_ledger_path,
        } => {
            let cloud_key = resolve_cloudlink_authentication_secret(cloud_public_key_ref)?;
            let gateway_key = resolve_cloudlink_authentication_secret(gateway_signing_key_ref)?;
            let authenticator = GatewaySessionAuthenticator::from_base64url(
                cloud_key_id.clone(),
                cloud_key,
                gateway_key_id.clone(),
                gateway_key,
            )
            .map_err(|_error| {
                HomeAssistantStartupError::InvalidCloudLinkAuthenticationKeyMaterial
            })?;
            let challenges = Arc::new(
                FileCloudLinkChallengeLedger::open(
                    challenge_ledger_path,
                    DEFAULT_CLOUDLINK_CHALLENGE_LEDGER_CAPACITY,
                )
                .map_err(|_error| HomeAssistantStartupError::CloudLinkChallengeLedgerUnavailable)?,
            );
            Ok(PreparedCloudLinkSessionAuthentication::GatewaySigned {
                authenticator: Box::new(authenticator),
                challenges,
            })
        },
        HomeAssistantCloudLinkAuthenticationConfig::TrustedConnectorBrokerAttestation => {
            if deployment_security == DeploymentSecurity::Production {
                return Err(
                    HomeAssistantStartupError::CloudLinkTrustedConnectorForbiddenInProduction,
                );
            }
            Ok(PreparedCloudLinkSessionAuthentication::TrustedConnectorBrokerAttestation)
        },
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
fn resolve_cloudlink_authentication_secret(
    reference: &SecretRef,
) -> Result<String, HomeAssistantStartupError> {
    let variable = reference
        .as_str()
        .strip_prefix("env:")
        .ok_or(HomeAssistantStartupError::CloudLinkAuthenticationSecretUnavailable)?;
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(HomeAssistantStartupError::CloudLinkAuthenticationSecretUnavailable)
}

#[cfg(feature = "home-assistant-cloudlink")]
fn resolve_environment_secret(reference: &SecretRef) -> Result<String, HomeAssistantStartupError> {
    let variable = reference
        .as_str()
        .strip_prefix("env:")
        .ok_or(HomeAssistantStartupError::CloudLinkSecretUnavailable)?;
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(HomeAssistantStartupError::CloudLinkSecretUnavailable)
}

#[cfg(all(feature = "home-assistant", not(feature = "home-assistant-cloudlink")))]
async fn run_enabled_integration(
    config: HomeAssistantEnabledConfig,
    connection: HomeAssistantConnectionConfig,
    generation_store: Arc<dyn IntegrationTopologyGenerationStore>,
    _cloudlink: Option<PreparedCloudLinkRuntime>,
    shutdown: CancellationToken,
) {
    let projection = Arc::new(InMemoryIntegrationProjection::default());
    run_home_assistant_synchronization(
        config,
        connection,
        generation_store,
        projection.clone(),
        projection,
        shutdown,
    )
    .await;
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn run_enabled_integration(
    config: HomeAssistantEnabledConfig,
    connection: HomeAssistantConnectionConfig,
    generation_store: Arc<dyn IntegrationTopologyGenerationStore>,
    cloudlink: Option<PreparedCloudLinkRuntime>,
    shutdown: CancellationToken,
) {
    let projection = Arc::new(InMemoryIntegrationProjection::default());
    let Some(cloudlink) = cloudlink else {
        run_home_assistant_synchronization(
            config,
            connection,
            generation_store,
            projection.clone(),
            projection,
            #[cfg(feature = "home-assistant-integration-control")]
            None,
            shutdown,
        )
        .await;
        return;
    };
    let outbox = Arc::new(CloudLinkIntegrationProjectionOutbox::new(
        projection.clone(),
        projection,
        cloudlink.extension.clone(),
        cloudlink.topology_spool.clone(),
        cloudlink.observation_spool.clone(),
    ));
    #[cfg(feature = "home-assistant-integration-control")]
    let control_executor = cloudlink
        .integration_control
        .as_ref()
        .map(PreparedIntegrationControl::executor);
    let cloud_shutdown = shutdown.clone();
    tokio::join!(
        run_home_assistant_synchronization(
            config,
            connection,
            generation_store,
            outbox.clone(),
            outbox.clone(),
            #[cfg(feature = "home-assistant-integration-control")]
            control_executor,
            shutdown,
        ),
        run_cloudlink_runtime(cloudlink, outbox, cloud_shutdown),
    );
}

#[cfg(feature = "home-assistant")]
async fn run_home_assistant_synchronization(
    config: HomeAssistantEnabledConfig,
    connection: HomeAssistantConnectionConfig,
    generation_store: Arc<dyn IntegrationTopologyGenerationStore>,
    projection_sink: Arc<dyn IntegrationProjectionSink>,
    projection_query: Arc<dyn IntegrationProjectionQuery>,
    #[cfg(feature = "home-assistant-integration-control")] control_executor: Option<
        Arc<ActiveHomeAssistantExecutor>,
    >,
    shutdown: CancellationToken,
) {
    let mut retry_delay = INITIAL_RETRY_DELAY;

    loop {
        let secrets = Arc::new(EnvironmentSecretResolver);
        let transport = tokio::select! {
            () = shutdown.cancelled() => break,
            result = WebSocketHomeAssistantTransport::connect(connection.clone(), secrets) => result,
        };
        let transport = match transport {
            Ok(transport) => transport,
            Err(error) => {
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "Home Assistant connection unavailable; retrying"
                );
                if wait_for_retry(&shutdown, retry_delay).await {
                    break;
                }
                retry_delay = next_retry_delay(retry_delay);
                continue;
            },
        };
        #[cfg(feature = "home-assistant-integration-control")]
        if let Some(executor) = &control_executor {
            executor.install(transport.clone()).await;
        }

        let provider = Arc::new(
            HomeAssistantBridge::new(
                config.gateway_id().clone(),
                config.integration_id().clone(),
                transport.clone(),
            )
            .with_generation_store(generation_store.clone()),
        );
        let synchronizer = IntegrationSynchronizer::new(
            provider,
            projection_sink.clone(),
            projection_query.clone(),
        );
        run_synchronizer(synchronizer, shutdown.clone()).await;
        #[cfg(feature = "home-assistant-integration-control")]
        if let Some(executor) = &control_executor {
            executor.clear().await;
        }
        if let Err(error) = transport.shutdown().await {
            tracing::warn!(
                error_kind = ?error.kind(),
                "Home Assistant transport shutdown failed"
            );
        }
        break;
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
struct FileSessionEpochStore {
    path: PathBuf,
    current: std::sync::atomic::AtomicU64,
}

#[cfg(feature = "home-assistant-cloudlink")]
impl FileSessionEpochStore {
    fn open(path: &Path) -> Result<Self, HomeAssistantStartupError> {
        let current = match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.len() > 64 {
                    return Err(HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                        "session epoch checkpoint is not a bounded regular file".to_owned(),
                    ));
                }
                let value = std::fs::read_to_string(path).map_err(|error| {
                    HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
                })?;
                value.trim().parse::<u64>().map_err(|_| {
                    HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                        "session epoch checkpoint is not a canonical uint64".to_owned(),
                    )
                })?
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                return Err(HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                    error.to_string(),
                ));
            },
        };
        let parent = path.parent().ok_or_else(|| {
            HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                "session epoch checkpoint has no parent directory".to_owned(),
            )
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            current: std::sync::atomic::AtomicU64::new(current),
        })
    }

    fn current(&self) -> u64 {
        self.current.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn persist(&self, next: u64) -> Result<(), HomeAssistantStartupError> {
        if next <= self.current() {
            return Err(HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                "accepted session epoch did not advance".to_owned(),
            ));
        }
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                    "session epoch checkpoint filename is invalid".to_owned(),
                )
            })?;
        let temporary = self
            .path
            .with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
        match std::fs::remove_file(&temporary) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => {
                return Err(HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(
                    error.to_string(),
                ));
            },
        }
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|error| {
            HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
        })?;
        std::io::Write::write_all(&mut file, format!("{next}\n").as_bytes()).map_err(|error| {
            HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
        })?;
        file.sync_all().map_err(|error| {
            HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
        })?;
        std::fs::rename(&temporary, &self.path).map_err(|error| {
            HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
        })?;
        #[cfg(unix)]
        if let Some(parent) = self.path.parent() {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    HomeAssistantStartupError::CloudLinkSessionEpochUnavailable(error.to_string())
                })?;
        }
        self.current
            .store(next, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn run_cloudlink_runtime(
    runtime: PreparedCloudLinkRuntime,
    projection: Arc<dyn IntegrationProjectionQuery>,
    shutdown: CancellationToken,
) {
    #[cfg(not(feature = "home-assistant-integration-control"))]
    let _ = projection;

    loop {
        let event = tokio::select! {
            () = shutdown.cancelled() => break,
            event = runtime.transport.receive() => event,
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "Home Assistant CloudLink MQTT event unavailable"
                );
                continue;
            },
        };
        if event != CloudLinkTransportEvent::Connected {
            continue;
        }
        let session = match establish_cloudlink_session(&runtime, &shutdown).await {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "Home Assistant CloudLink session establishment failed closed"
                );
                continue;
            },
        };
        let uplink_authentication = runtime.session_authentication.uplink_authentication();
        let publisher = CloudLinkIntegrationPublisher::new(
            runtime.extension.clone(),
            runtime.topology_spool.clone(),
            runtime.observation_spool.clone(),
            runtime.transport.clone(),
            session.clone(),
            uplink_authentication.clone(),
        );
        if let Err(error) = publisher
            .process_event(CloudLinkTransportEvent::Connected)
            .await
        {
            tracing::warn!(
                error_kind = ?error.kind(),
                "Home Assistant CloudLink retained replay failed closed for the current session"
            );
            continue;
        }

        #[cfg(feature = "home-assistant-integration-control")]
        if let Some(control) = runtime.integration_control.as_ref() {
            let processor = match control.processor(&session, projection.clone()) {
                Ok(processor) => Some(processor),
                Err(error) => {
                    tracing::warn!(
                        error_kind = ?error.kind(),
                        "Home Assistant Integration-control session binding failed closed"
                    );
                    None
                },
            };
            let processor = match processor {
                Some(processor) => match control.recover_once(&processor).await {
                    Ok(()) => Some(processor),
                    Err(error) => {
                        tracing::warn!(
                            error_kind = ?error.kind(),
                            "Home Assistant Integration-control crash recovery failed closed"
                        );
                        None
                    },
                },
                None => None,
            };
            let processor = match processor {
                Some(processor) => match control.activate(runtime.transport.as_ref()).await {
                    Ok(()) => Some(processor),
                    Err(error) => {
                        tracing::warn!(
                            error_kind = ?error.kind(),
                            "Home Assistant Integration-control subscription activation failed closed"
                        );
                        None
                    },
                },
                None => None,
            };

            if let Some(processor) = processor {
                if let Err(error) = control
                    .flush_receipts(&session, &uplink_authentication, runtime.transport.as_ref())
                    .await
                {
                    tracing::warn!(
                        error_kind = ?error.kind(),
                        "Home Assistant Integration-control retained receipt replay failed"
                    );
                }

                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        offer = runtime.transport.receive_integration_control_offer() => {
                            match offer {
                                Ok(bytes) => {
                                    if let Err(error) = control
                                        .process_offer(
                                            &processor,
                                            &session,
                                            &uplink_authentication,
                                            runtime.transport.as_ref(),
                                            &bytes,
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            error_kind = ?error.kind(),
                                            "Home Assistant Integration-control offer rejected"
                                        );
                                    }
                                },
                                Err(error) => {
                                    tracing::warn!(
                                        error_kind = ?error.kind(),
                                        "Home Assistant Integration-control MQTT event unavailable"
                                    );
                                    if error.kind() == PortErrorKind::Unavailable {
                                        break;
                                    }
                                },
                            }
                        },
                        event = runtime.transport.receive() => {
                            let event = match event {
                                Ok(event) => event,
                                Err(error) => {
                                    tracing::warn!(
                                        error_kind = ?error.kind(),
                                        "Home Assistant CloudLink MQTT event unavailable"
                                    );
                                    break;
                                },
                            };
                            if matches!(
                                event,
                                CloudLinkTransportEvent::Disconnected
                                    | CloudLinkTransportEvent::Connected
                            ) {
                                break;
                            }
                            if let CloudLinkTransportEvent::Inbound(message) = &event
                                && message.route() == CloudLinkTransportRoute::AckDown
                            {
                                match control.acknowledge(&session, message.payload()).await {
                                    Ok(true) => {
                                        if let Err(error) = control
                                            .flush_receipts(
                                                &session,
                                                &uplink_authentication,
                                                runtime.transport.as_ref(),
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                error_kind = ?error.kind(),
                                                "Home Assistant Integration-control receipt continuation failed"
                                            );
                                        }
                                        continue;
                                    },
                                    Ok(false) => {},
                                    Err(error) => {
                                        tracing::warn!(
                                            error_kind = ?error.kind(),
                                            "Home Assistant Integration-control durable ACK rejected"
                                        );
                                        continue;
                                    },
                                }
                            }
                            if let Err(error) = publisher.process_event(event).await {
                                tracing::warn!(
                                    error_kind = ?error.kind(),
                                    "Home Assistant CloudLink publication event rejected"
                                );
                            }
                        },
                    }
                }
                continue;
            }
            // A failed control session remains fail-closed without stalling
            // the separately commissioned read-only Integration publisher.
        }

        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return,
                event = runtime.transport.receive() => event,
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!(
                        error_kind = ?error.kind(),
                        "Home Assistant CloudLink MQTT event unavailable"
                    );
                    break;
                },
            };
            if matches!(
                event,
                CloudLinkTransportEvent::Disconnected | CloudLinkTransportEvent::Connected
            ) {
                break;
            }
            if let Err(error) = publisher.process_event(event).await {
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "Home Assistant CloudLink publication event rejected"
                );
            }
        }
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn establish_cloudlink_session(
    runtime: &PreparedCloudLinkRuntime,
    shutdown: &CancellationToken,
) -> PortResult<SessionBinding> {
    match &runtime.session_authentication {
        PreparedCloudLinkSessionAuthentication::GatewaySigned {
            authenticator,
            challenges,
        } => {
            establish_gateway_signed_cloudlink_session(runtime, authenticator, challenges, shutdown)
                .await
        },
        PreparedCloudLinkSessionAuthentication::TrustedConnectorBrokerAttestation => {
            establish_trusted_connector_cloudlink_session(runtime, shutdown).await
        },
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn establish_gateway_signed_cloudlink_session(
    runtime: &PreparedCloudLinkRuntime,
    authenticator: &GatewaySessionAuthenticator,
    challenges: &FileCloudLinkChallengeLedger,
    shutdown: &CancellationToken,
) -> PortResult<SessionBinding> {
    let now = current_timestamp_ms()?.get();
    let request_deadline = now
        .checked_add(CLOUDLINK_CHALLENGE_REQUEST_LIFETIME_MS)
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::Rejected,
                "CloudLink challenge request deadline overflowed",
            )
        })?;
    let resume = cloudlink_resume_cursors(runtime).await?;
    let candidate_request = SessionChallengeRequest::new(
        runtime.gateway_id.clone(),
        runtime.credential_id.clone(),
        runtime.credential_generation,
        vec![CLOUDLINK_PROTOCOL_VERSION.to_owned()],
        random_nonce(),
        resume,
    )
    .map_err(cloudlink_codec_error)?;
    let candidate_bytes =
        CloudLinkCodec::encode(&candidate_request).map_err(cloudlink_codec_error)?;
    let pending_request = challenges
        .prepare_request(&candidate_bytes, request_deadline, now)
        .map_err(cloudlink_challenge_ledger_error)?;
    let persisted_request = decode_session_challenge_request(pending_request.payload())?;
    let challenge = receive_cloudlink_challenge_with_retry(
        runtime.transport.as_ref(),
        pending_request.payload(),
        pending_request.expires_at_ms(),
        shutdown,
    )
    .await?;
    let now = current_timestamp_ms()?.get();
    authenticator
        .verify_challenge(&challenge, &runtime.gateway_id, now)
        .map_err(cloudlink_codec_error)?;
    let challenge_bytes = CloudLinkCodec::encode(&challenge).map_err(cloudlink_codec_error)?;
    let hello_bytes = match challenges
        .reserve(
            challenge.challenge_id(),
            challenge.expires_at_ms(),
            &challenge_bytes,
            pending_request.payload(),
            now,
        )
        .map_err(cloudlink_challenge_ledger_error)?
    {
        CloudLinkChallengeReservation::Prepare { challenge, request } => {
            let persisted_challenge = decode_session_challenge(&challenge)?;
            let persisted_request = decode_session_challenge_request(&request)?;
            let verified = authenticator
                .verify_challenge(
                    &persisted_challenge,
                    &runtime.gateway_id,
                    current_timestamp_ms()?.get(),
                )
                .map_err(cloudlink_codec_error)?;
            let hello = authenticator
                .sign_hello(&verified, &persisted_request)
                .map_err(cloudlink_codec_error)?;
            let encoded = CloudLinkCodec::encode(&hello).map_err(cloudlink_codec_error)?;
            challenges
                .store_hello(persisted_challenge.challenge_id(), &encoded)
                .map_err(cloudlink_challenge_ledger_error)?
        },
        CloudLinkChallengeReservation::RetryHello(hello) => hello,
    };
    let accepted = receive_gateway_session_accepted_with_retry(
        runtime,
        authenticator,
        challenges,
        &persisted_request,
        &challenge,
        &hello_bytes,
        shutdown,
    )
    .await?;
    let session = bind_cloudlink_session(runtime, &accepted)?;
    challenges
        .complete(challenge.challenge_id())
        .map_err(cloudlink_challenge_ledger_error)?;
    persist_cloudlink_session_epoch(runtime, &session)?;
    Ok(session)
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn establish_trusted_connector_cloudlink_session(
    runtime: &PreparedCloudLinkRuntime,
    shutdown: &CancellationToken,
) -> PortResult<SessionBinding> {
    let challenge = loop {
        let event = tokio::select! {
            () = shutdown.cancelled() => {
                return Err(PortError::new(PortErrorKind::Unavailable, "CloudLink shutdown"));
            },
            event = runtime.transport.receive() => event?,
        };
        match event {
            CloudLinkTransportEvent::Inbound(message)
                if message.route() == CloudLinkTransportRoute::SessionDown =>
            {
                match CloudLinkCodec::decode(message.payload()).map_err(cloudlink_codec_error)? {
                    CandidateMessage::SessionChallenge(challenge) => break challenge,
                    _ => {
                        return Err(PortError::new(
                            PortErrorKind::Rejected,
                            "CloudLink session route carried an unexpected message",
                        ));
                    },
                }
            },
            CloudLinkTransportEvent::Disconnected => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "CloudLink disconnected during session challenge",
                ));
            },
            CloudLinkTransportEvent::Connected
            | CloudLinkTransportEvent::TransportPublished(_)
            | CloudLinkTransportEvent::Inbound(_) => {},
        }
    };
    let now = current_timestamp_ms()?.get();
    if challenge.gateway_id() != runtime.gateway_id
        || now < challenge.issued_at_ms()
        || now >= challenge.expires_at_ms()
    {
        return Err(PortError::new(
            PortErrorKind::Rejected,
            "CloudLink challenge scope or validity window is invalid",
        ));
    }
    let resume = cloudlink_resume_cursors(runtime).await?;
    let hello = SessionHello::new_trusted_connector_broker_attested(
        runtime.gateway_id.clone(),
        runtime.credential_id.clone(),
        runtime.credential_generation,
        challenge.challenge_id(),
        vec![CLOUDLINK_PROTOCOL_VERSION.to_owned()],
        random_nonce(),
        resume,
    )
    .map_err(cloudlink_codec_error)?;
    runtime
        .transport
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::SessionUp,
            CloudLinkCodec::encode(&hello).map_err(cloudlink_codec_error)?,
            None,
        ))
        .await?;

    let accepted = loop {
        let event = tokio::select! {
            () = shutdown.cancelled() => {
                return Err(PortError::new(PortErrorKind::Unavailable, "CloudLink shutdown"));
            },
            event = runtime.transport.receive() => event?,
        };
        match event {
            CloudLinkTransportEvent::Inbound(message)
                if message.route() == CloudLinkTransportRoute::SessionDown =>
            {
                match CloudLinkCodec::decode(message.payload()).map_err(cloudlink_codec_error)? {
                    CandidateMessage::SessionAccepted(accepted) => break accepted,
                    _ => {
                        return Err(PortError::new(
                            PortErrorKind::Rejected,
                            "CloudLink session route did not accept the hello",
                        ));
                    },
                }
            },
            CloudLinkTransportEvent::Disconnected => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "CloudLink disconnected during session acceptance",
                ));
            },
            CloudLinkTransportEvent::Connected
            | CloudLinkTransportEvent::TransportPublished(_)
            | CloudLinkTransportEvent::Inbound(_) => {},
        }
    };
    let session = bind_cloudlink_session(runtime, &accepted)?;
    persist_cloudlink_session_epoch(runtime, &session)?;
    Ok(session)
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn receive_cloudlink_challenge_with_retry(
    transport: &(impl CloudLinkTransport + ?Sized),
    request: &[u8],
    deadline_ms: u64,
    shutdown: &CancellationToken,
) -> PortResult<SessionChallenge> {
    let local_deadline = bounded_session_retry_deadline(deadline_ms)?;
    send_cloudlink_session_payload(transport, request).await?;
    let mut retry_delay = INITIAL_CLOUDLINK_SESSION_RETRY_DELAY;
    let mut next_retry_at = tokio::time::Instant::now() + retry_delay;
    loop {
        let now = current_timestamp_ms()?.get();
        let local_now = tokio::time::Instant::now();
        if now >= deadline_ms || local_now >= local_deadline {
            return Err(PortError::new(
                PortErrorKind::Timeout,
                "CloudLink challenge request deadline expired",
            ));
        }
        let remaining = Duration::from_millis(deadline_ms - now)
            .min(local_deadline.saturating_duration_since(local_now));
        let wait = next_retry_at
            .saturating_duration_since(local_now)
            .min(remaining);
        let event = tokio::select! {
            () = shutdown.cancelled() => {
                return Err(PortError::new(PortErrorKind::Unavailable, "CloudLink shutdown"));
            },
            event = transport.receive() => Some(event?),
            () = tokio::time::sleep(wait) => None,
        };
        let Some(event) = event else {
            if tokio::time::Instant::now() < next_retry_at {
                continue;
            }
            send_cloudlink_session_payload(transport, request).await?;
            retry_delay = retry_delay
                .saturating_mul(2)
                .min(MAX_CLOUDLINK_SESSION_RETRY_DELAY);
            next_retry_at = tokio::time::Instant::now() + retry_delay;
            continue;
        };
        match event {
            CloudLinkTransportEvent::Inbound(message)
                if message.route() == CloudLinkTransportRoute::SessionDown =>
            {
                return match CloudLinkCodec::decode(message.payload())
                    .map_err(cloudlink_codec_error)?
                {
                    CandidateMessage::SessionChallenge(challenge) => Ok(challenge),
                    _ => Err(PortError::new(
                        PortErrorKind::Rejected,
                        "CloudLink session route carried an unexpected message",
                    )),
                };
            },
            CloudLinkTransportEvent::Disconnected => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "CloudLink disconnected during session challenge",
                ));
            },
            CloudLinkTransportEvent::Connected
            | CloudLinkTransportEvent::TransportPublished(_)
            | CloudLinkTransportEvent::Inbound(_) => {},
        }
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn receive_gateway_session_accepted_with_retry(
    runtime: &PreparedCloudLinkRuntime,
    authenticator: &GatewaySessionAuthenticator,
    challenges: &FileCloudLinkChallengeLedger,
    request: &SessionChallengeRequest,
    challenge: &SessionChallenge,
    hello: &[u8],
    shutdown: &CancellationToken,
) -> PortResult<SessionAccepted> {
    // This receive loop starts synchronously after a challenge observed on the
    // current Connected transport and aborts on Disconnected. Binding below
    // also requires a strictly newer persisted session epoch. The alpha wire
    // still cannot cryptographically correlate SessionAccepted to this exact
    // challenge/client nonce, so this remains experimental until the public
    // contract adds authenticated transcript correlation.
    let local_deadline = bounded_session_retry_deadline(challenge.expires_at_ms())?;
    send_cloudlink_session_payload(runtime.transport.as_ref(), hello).await?;
    let request_bytes = CloudLinkCodec::encode(request).map_err(cloudlink_codec_error)?;
    let mut retry_delay = INITIAL_CLOUDLINK_SESSION_RETRY_DELAY;
    let mut next_retry_at = tokio::time::Instant::now() + retry_delay;
    loop {
        let now = current_timestamp_ms()?.get();
        let local_now = tokio::time::Instant::now();
        if now >= challenge.expires_at_ms() || local_now >= local_deadline {
            return Err(PortError::new(
                PortErrorKind::Timeout,
                "CloudLink signed hello deadline expired",
            ));
        }
        let remaining = Duration::from_millis(challenge.expires_at_ms() - now)
            .min(local_deadline.saturating_duration_since(local_now));
        let wait = next_retry_at
            .saturating_duration_since(local_now)
            .min(remaining);
        let event = tokio::select! {
            () = shutdown.cancelled() => {
                return Err(PortError::new(PortErrorKind::Unavailable, "CloudLink shutdown"));
            },
            event = runtime.transport.receive() => Some(event?),
            () = tokio::time::sleep(wait) => None,
        };
        let Some(event) = event else {
            if tokio::time::Instant::now() < next_retry_at {
                continue;
            }
            send_cloudlink_session_payload(runtime.transport.as_ref(), hello).await?;
            retry_delay = retry_delay
                .saturating_mul(2)
                .min(MAX_CLOUDLINK_SESSION_RETRY_DELAY);
            next_retry_at = tokio::time::Instant::now() + retry_delay;
            continue;
        };
        match event {
            CloudLinkTransportEvent::Inbound(message)
                if message.route() == CloudLinkTransportRoute::SessionDown =>
            {
                match CloudLinkCodec::decode(message.payload()).map_err(cloudlink_codec_error)? {
                    CandidateMessage::SessionAccepted(accepted) => return Ok(accepted),
                    CandidateMessage::SessionChallenge(repeated)
                        if repeated.challenge_id() == challenge.challenge_id() =>
                    {
                        let now = current_timestamp_ms()?.get();
                        authenticator
                            .verify_challenge(&repeated, &runtime.gateway_id, now)
                            .map_err(cloudlink_codec_error)?;
                        let repeated_bytes =
                            CloudLinkCodec::encode(&repeated).map_err(cloudlink_codec_error)?;
                        let retry = challenges
                            .reserve(
                                repeated.challenge_id(),
                                repeated.expires_at_ms(),
                                &repeated_bytes,
                                &request_bytes,
                                now,
                            )
                            .map_err(cloudlink_challenge_ledger_error)?;
                        let CloudLinkChallengeReservation::RetryHello(retry_hello) = retry else {
                            return Err(PortError::new(
                                PortErrorKind::Rejected,
                                "CloudLink repeated challenge has no persisted hello",
                            ));
                        };
                        if retry_hello != hello {
                            return Err(PortError::new(
                                PortErrorKind::Conflict,
                                "CloudLink repeated challenge conflicts with persisted hello",
                            ));
                        }
                        send_cloudlink_session_payload(runtime.transport.as_ref(), &retry_hello)
                            .await?;
                    },
                    _ => {
                        return Err(PortError::new(
                            PortErrorKind::Rejected,
                            "CloudLink session route did not accept the hello",
                        ));
                    },
                }
            },
            CloudLinkTransportEvent::Disconnected => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "CloudLink disconnected during session acceptance",
                ));
            },
            CloudLinkTransportEvent::Connected
            | CloudLinkTransportEvent::TransportPublished(_)
            | CloudLinkTransportEvent::Inbound(_) => {},
        }
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn send_cloudlink_session_payload(
    transport: &(impl CloudLinkTransport + ?Sized),
    payload: &[u8],
) -> PortResult<()> {
    transport
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::SessionUp,
            payload.to_vec(),
            None,
        ))
        .await
}

#[cfg(feature = "home-assistant-cloudlink")]
fn bounded_session_retry_deadline(protocol_deadline_ms: u64) -> PortResult<tokio::time::Instant> {
    let now = current_timestamp_ms()?.get();
    if now >= protocol_deadline_ms {
        return Err(PortError::new(
            PortErrorKind::Timeout,
            "CloudLink session authentication deadline expired",
        ));
    }
    let retry_window = Duration::from_millis(
        (protocol_deadline_ms - now).min(CLOUDLINK_CHALLENGE_REQUEST_LIFETIME_MS),
    );
    tokio::time::Instant::now()
        .checked_add(retry_window)
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::Permanent,
                "CloudLink session retry deadline cannot be represented",
            )
        })
}

#[cfg(feature = "home-assistant-cloudlink")]
fn decode_session_challenge_request(bytes: &[u8]) -> PortResult<SessionChallengeRequest> {
    match CloudLinkCodec::decode(bytes).map_err(cloudlink_codec_error)? {
        CandidateMessage::SessionChallengeRequest(request) => Ok(request),
        _ => Err(PortError::new(
            PortErrorKind::InvalidData,
            "persisted CloudLink challenge request has the wrong message kind",
        )),
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
fn decode_session_challenge(bytes: &[u8]) -> PortResult<SessionChallenge> {
    match CloudLinkCodec::decode(bytes).map_err(cloudlink_codec_error)? {
        CandidateMessage::SessionChallenge(challenge) => Ok(challenge),
        _ => Err(PortError::new(
            PortErrorKind::InvalidData,
            "persisted CloudLink challenge has the wrong message kind",
        )),
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
fn bind_cloudlink_session(
    runtime: &PreparedCloudLinkRuntime,
    accepted: &SessionAccepted,
) -> PortResult<SessionBinding> {
    accepted
        .bind(
            &runtime.gateway_id,
            runtime.credential_generation,
            &[CLOUDLINK_PROTOCOL_VERSION],
            runtime.session_epochs.current(),
        )
        .map_err(cloudlink_codec_error)
}

#[cfg(feature = "home-assistant-cloudlink")]
fn persist_cloudlink_session_epoch(
    runtime: &PreparedCloudLinkRuntime,
    session: &SessionBinding,
) -> PortResult<()> {
    runtime
        .session_epochs
        .persist(session.session_epoch())
        .map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "CloudLink session epoch could not be committed",
            )
        })
}

#[cfg(feature = "home-assistant-cloudlink")]
async fn cloudlink_resume_cursors(
    runtime: &PreparedCloudLinkRuntime,
) -> PortResult<Vec<ResumeCursor>> {
    let topology = runtime
        .topology_spool
        .status()
        .await
        .map_err(cloudlink_spool_error)?;
    let observations = runtime
        .observation_spool
        .status()
        .await
        .map_err(cloudlink_spool_error)?;
    [
        ResumeCursor::new(
            topology.stream_id(),
            topology.stream_epoch(),
            topology.last_acknowledged_position(),
        ),
        ResumeCursor::new(
            observations.stream_id(),
            observations.stream_epoch(),
            observations.last_acknowledged_position(),
        ),
    ]
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(cloudlink_codec_error)
}

#[cfg(feature = "home-assistant-cloudlink")]
fn random_nonce() -> String {
    use rand::RngCore;

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut encoded = String::with_capacity(43);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        accumulator = (accumulator << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            encoded.push(char::from(
                ALPHABET[((accumulator >> bits) & 0x3f) as usize],
            ));
        }
    }
    if bits > 0 {
        encoded.push(char::from(
            ALPHABET[((accumulator << (6 - bits)) & 0x3f) as usize],
        ));
    }
    encoded
}

#[cfg(feature = "home-assistant-cloudlink")]
fn current_timestamp_ms() -> PortResult<aether_domain::TimestampMs> {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_source| {
            PortError::new(
                PortErrorKind::Permanent,
                "system clock is before the Unix epoch",
            )
        })
        .and_then(|duration| {
            u64::try_from(duration.as_millis()).map_err(|_source| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "system clock cannot be represented as protocol milliseconds",
                )
            })
        })?;
    Ok(aether_domain::TimestampMs::new(value))
}

#[cfg(feature = "home-assistant-cloudlink")]
fn cloudlink_codec_error(error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        format!("CloudLink session message is invalid: {error}"),
    )
}

#[cfg(feature = "home-assistant-cloudlink")]
fn cloudlink_challenge_ledger_error(error: CloudLinkChallengeLedgerError) -> PortError {
    let kind = match error {
        CloudLinkChallengeLedgerError::CapacityExceeded
        | CloudLinkChallengeLedgerError::Storage => PortErrorKind::Unavailable,
        CloudLinkChallengeLedgerError::ConflictingReplay => PortErrorKind::Conflict,
        CloudLinkChallengeLedgerError::CompletedReplay
        | CloudLinkChallengeLedgerError::MessageExpired
        | CloudLinkChallengeLedgerError::MissingChallenge
        | CloudLinkChallengeLedgerError::InvalidTransition => PortErrorKind::Rejected,
        CloudLinkChallengeLedgerError::InvalidInput | CloudLinkChallengeLedgerError::Corrupt => {
            PortErrorKind::InvalidData
        },
        CloudLinkChallengeLedgerError::UnsupportedPlatform
        | CloudLinkChallengeLedgerError::InsecurePermissions => PortErrorKind::Permanent,
    };
    PortError::new(kind, error.to_string())
}

#[cfg(feature = "home-assistant-cloudlink")]
fn cloudlink_spool_error(error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("CloudLink durable stream is unavailable: {error}"),
    )
}

#[cfg(feature = "home-assistant")]
async fn run_synchronizer(synchronizer: IntegrationSynchronizer, shutdown: CancellationToken) {
    let mut needs_snapshot = true;
    let mut retry_delay = INITIAL_RETRY_DELAY;

    loop {
        let result = tokio::select! {
            () = shutdown.cancelled() => break,
            result = async {
                if needs_snapshot {
                    synchronizer.synchronize_snapshot().await
                } else {
                    synchronizer.synchronize_next().await
                }
            } => result,
        };
        match result {
            Ok(receipt) => {
                needs_snapshot = false;
                retry_delay = INITIAL_RETRY_DELAY;
                if receipt.sequence().is_none() {
                    tracing::info!(
                        gateway_id = receipt.gateway_id().as_str(),
                        integration_id = receipt.integration_id().as_str(),
                        generation = receipt.generation().get(),
                        "Home Assistant delegated-device snapshot synchronized"
                    );
                }
            },
            Err(IntegrationSynchronizationError::Provider(error))
                if !needs_snapshot && error.kind() == PortErrorKind::Timeout =>
            {
                // A subscribed Home Assistant stream may legitimately be
                // silent. A bounded wait expiring proves neither a disconnect
                // nor an event gap, so keep the current topology generation
                // and the existing subscription.
                retry_delay = INITIAL_RETRY_DELAY;
                tokio::task::yield_now().await;
            },
            Err(error) => {
                needs_snapshot = true;
                tracing::warn!(
                    error = %error,
                    "Home Assistant incremental projection stopped; complete resynchronization required"
                );
                if wait_for_retry(&shutdown, retry_delay).await {
                    break;
                }
                retry_delay = next_retry_delay(retry_delay);
            },
        }
    }
}

#[cfg(feature = "home-assistant")]
async fn wait_for_retry(shutdown: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        () = shutdown.cancelled() => true,
        () = tokio::time::sleep(delay) => false,
    }
}

#[cfg(feature = "home-assistant")]
fn next_retry_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_RETRY_DELAY)
}

#[cfg(all(
    test,
    feature = "home-assistant-integration-control",
    target_family = "unix"
))]
#[path = "home_assistant_real_broker_tests.rs"]
mod real_broker_tests;

#[cfg(all(test, feature = "home-assistant"))]
mod tests {
    #[cfg(feature = "home-assistant-cloudlink")]
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aether_domain::{
        EntityId, EntityPointDescriptor, EntityRecord, IntegrationPointKey, IntegrationPointKind,
        IntegrationTopologySnapshot, ObservedValue, ObservedValueType, SnapshotDigest, TimestampMs,
    };
    use aether_ports::{DelegatedDeviceProvider, PortResult};
    #[cfg(feature = "home-assistant-cloudlink")]
    use serde_json::json;
    #[cfg(feature = "home-assistant-cloudlink")]
    use tokio::sync::Notify;

    use super::*;

    #[cfg(feature = "home-assistant-cloudlink")]
    struct RetryChallengeTransport {
        sent: std::sync::Mutex<Vec<Vec<u8>>>,
        inbound: std::sync::Mutex<VecDeque<CloudLinkTransportEvent>>,
        inbound_ready: Notify,
        challenge: Option<Vec<u8>>,
    }

    #[cfg(feature = "home-assistant-cloudlink")]
    impl RetryChallengeTransport {
        fn new(challenge: Vec<u8>) -> Self {
            Self {
                sent: std::sync::Mutex::new(Vec::new()),
                inbound: std::sync::Mutex::new(VecDeque::new()),
                inbound_ready: Notify::new(),
                challenge: Some(challenge),
            }
        }

        fn silent() -> Self {
            Self {
                sent: std::sync::Mutex::new(Vec::new()),
                inbound: std::sync::Mutex::new(VecDeque::new()),
                inbound_ready: Notify::new(),
                challenge: None,
            }
        }

        fn sent_payloads(&self) -> Vec<Vec<u8>> {
            self.sent.lock().expect("sent payload lock").clone()
        }

        fn push_event(&self, event: CloudLinkTransportEvent) {
            self.inbound
                .lock()
                .expect("inbound event lock")
                .push_back(event);
            self.inbound_ready.notify_one();
        }
    }

    #[cfg(feature = "home-assistant-cloudlink")]
    #[async_trait]
    impl CloudLinkTransport for RetryChallengeTransport {
        async fn send(&self, message: CloudLinkTransportMessage) -> PortResult<()> {
            let send_count = {
                let mut sent = self.sent.lock().expect("sent payload lock");
                sent.push(message.payload().to_vec());
                sent.len()
            };
            if send_count == 2
                && let Some(challenge) = &self.challenge
            {
                self.inbound.lock().expect("inbound event lock").push_back(
                    CloudLinkTransportEvent::Inbound(CloudLinkTransportMessage::new(
                        CloudLinkTransportRoute::SessionDown,
                        challenge.clone(),
                        None,
                    )),
                );
                self.inbound_ready.notify_one();
            }
            Ok(())
        }

        async fn receive(&self) -> PortResult<CloudLinkTransportEvent> {
            loop {
                let notified = self.inbound_ready.notified();
                if let Some(event) = self.inbound.lock().expect("inbound event lock").pop_front() {
                    return Ok(event);
                }
                notified.await;
            }
        }
    }

    struct GapOnceProvider {
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        snapshot_count: AtomicUsize,
        observation_count: AtomicUsize,
    }

    impl GapOnceProvider {
        fn new() -> Self {
            Self {
                gateway_id: GatewayIdentity::new("home-edge").expect("gateway identity"),
                integration_id: IntegrationId::new("home-assistant-main")
                    .expect("integration identity"),
                snapshot_count: AtomicUsize::new(0),
                observation_count: AtomicUsize::new(0),
            }
        }

        fn make_snapshot(&self, generation: u64) -> IntegrationSnapshot {
            let entity_id = EntityId::new("switch-kitchen").expect("entity identity");
            let point_key = IntegrationPointKey::new("is_on").expect("point key");
            let entity = EntityRecord::new(
                entity_id.clone(),
                "Kitchen switch",
                "switch",
                vec![
                    EntityPointDescriptor::new(
                        point_key.clone(),
                        "Is on",
                        IntegrationPointKind::State,
                        ObservedValueType::Boolean,
                        None,
                    )
                    .expect("point descriptor"),
                ],
                None,
                None,
                vec![],
            )
            .expect("entity");
            let topology = IntegrationTopologySnapshot::new(
                self.gateway_id.clone(),
                self.integration_id.clone(),
                TopologyGeneration::new(generation).expect("generation"),
                TimestampMs::new(generation),
                SnapshotDigest::new(format!("sha256:{generation:064x}")).expect("digest"),
                vec![],
                vec![],
                vec![entity],
            )
            .expect("topology");
            let observation = self.make_observation(entity_id, point_key, 1, generation, false);
            IntegrationSnapshot::new(topology, vec![observation]).expect("snapshot")
        }

        fn make_observation(
            &self,
            entity_id: EntityId,
            point_key: IntegrationPointKey,
            sequence: u64,
            observed_at_ms: u64,
            value: bool,
        ) -> IntegrationObservation {
            IntegrationObservation::available(
                self.gateway_id.clone(),
                self.integration_id.clone(),
                entity_id,
                point_key,
                ObservedValue::boolean(value),
                TimestampMs::new(observed_at_ms),
                sequence,
                None,
            )
            .expect("observation")
        }
    }

    #[async_trait]
    impl DelegatedDeviceProvider for GapOnceProvider {
        fn gateway_id(&self) -> &GatewayIdentity {
            &self.gateway_id
        }

        fn integration_id(&self) -> &IntegrationId {
            &self.integration_id
        }

        async fn snapshot(&self) -> PortResult<IntegrationSnapshot> {
            let generation = self.snapshot_count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(self.make_snapshot(u64::try_from(generation).expect("snapshot count fits u64")))
        }

        async fn next_observation(&self) -> PortResult<IntegrationObservation> {
            if self.observation_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(PortError::new(
                    PortErrorKind::Conflict,
                    "scripted stream gap",
                ));
            }
            std::future::pending().await
        }
    }

    struct IdleOnceProvider {
        inner: GapOnceProvider,
    }

    impl IdleOnceProvider {
        fn new() -> Self {
            Self {
                inner: GapOnceProvider::new(),
            }
        }
    }

    #[async_trait]
    impl DelegatedDeviceProvider for IdleOnceProvider {
        fn gateway_id(&self) -> &GatewayIdentity {
            &self.inner.gateway_id
        }

        fn integration_id(&self) -> &IntegrationId {
            &self.inner.integration_id
        }

        async fn snapshot(&self) -> PortResult<IntegrationSnapshot> {
            let generation = self.inner.snapshot_count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(self
                .inner
                .make_snapshot(u64::try_from(generation).expect("snapshot count fits u64")))
        }

        async fn next_observation(&self) -> PortResult<IntegrationObservation> {
            match self.inner.observation_count.fetch_add(1, Ordering::SeqCst) {
                0 => Err(PortError::new(
                    PortErrorKind::Timeout,
                    "scripted normal idle interval",
                )),
                1 => Ok(self.inner.make_observation(
                    EntityId::new("switch-kitchen").expect("entity identity"),
                    IntegrationPointKey::new("is_on").expect("point key"),
                    2,
                    2,
                    true,
                )),
                _ => std::future::pending().await,
            }
        }
    }

    async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
        for _ in 0..100 {
            if counter.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("counter did not reach {expected}");
    }

    #[tokio::test(start_paused = true)]
    async fn synchronization_loop_resnapshots_after_a_provider_stream_gap_and_stops() {
        let provider = Arc::new(GapOnceProvider::new());
        let projection = Arc::new(InMemoryIntegrationProjection::default());
        let synchronizer =
            IntegrationSynchronizer::new(provider.clone(), projection.clone(), projection);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run_synchronizer(synchronizer, shutdown.clone()));

        wait_for_count(&provider.observation_count, 1).await;
        tokio::time::advance(INITIAL_RETRY_DELAY).await;
        wait_for_count(&provider.snapshot_count, 2).await;

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("synchronizer stops within the deadline")
            .expect("synchronizer task does not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn synchronization_loop_keeps_the_projection_generation_during_normal_idle_time() {
        let provider = Arc::new(IdleOnceProvider::new());
        let projection = Arc::new(InMemoryIntegrationProjection::default());
        let synchronizer =
            IntegrationSynchronizer::new(provider.clone(), projection.clone(), projection.clone());
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run_synchronizer(synchronizer, shutdown.clone()));

        wait_for_count(&provider.inner.observation_count, 3).await;
        assert_eq!(
            provider.inner.snapshot_count.load(Ordering::SeqCst),
            1,
            "normal idle time must not request another complete snapshot"
        );
        let projected = projection
            .snapshot(provider.gateway_id(), provider.integration_id())
            .await
            .expect("projection query")
            .expect("projected snapshot");
        assert_eq!(projected.topology().generation().get(), 1);
        assert_eq!(
            projected
                .observations()
                .iter()
                .map(IntegrationObservation::sequence)
                .max(),
            Some(2)
        );

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("synchronizer stops within the deadline")
            .expect("synchronizer task does not panic");
    }

    #[cfg(feature = "home-assistant-cloudlink")]
    #[tokio::test(start_paused = true)]
    async fn challenge_request_retry_reuses_exact_bytes_until_cloud_replies() {
        let now = current_timestamp_ms()
            .expect("representable system clock")
            .get();
        let challenge = serde_json::to_vec(&json!({
            "schema": "aether.cloudlink.session-challenge.v1",
            "protocol": "aether.cloudlink",
            "message_kind": "session-challenge",
            "gateway_id": "33333333-3333-4333-8333-333333333333",
            "challenge_id": "22222222-2222-4222-8222-222222222222",
            "cloud_nonce": "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
            "issued_at_ms": now.to_string(),
            "expires_at_ms": (now + 60_000).to_string(),
            "cloud_signature": {
                "key_id": "cloud-key-1",
                "algorithm": "Ed25519",
                "signature": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            }
        }))
        .expect("challenge JSON");
        let transport = Arc::new(RetryChallengeTransport::new(challenge));
        let shutdown = CancellationToken::new();
        let request = br#"{"session":"request","client_nonce":"sensitive"}"#.to_vec();
        let task_transport = transport.clone();
        let task_shutdown = shutdown.clone();
        let request_for_task = request.clone();
        let task = tokio::spawn(async move {
            receive_cloudlink_challenge_with_retry(
                task_transport.as_ref(),
                &request_for_task,
                now + 60_000,
                &task_shutdown,
            )
            .await
        });

        for _ in 0..100 {
            if !transport.sent_payloads().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(transport.sent_payloads().len(), 1);
        for _ in 0..8 {
            transport.push_event(CloudLinkTransportEvent::Connected);
        }
        tokio::time::advance(INITIAL_CLOUDLINK_SESSION_RETRY_DELAY).await;
        for _ in 0..100 {
            if transport.sent_payloads().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        task.await
            .expect("retry task")
            .expect("second request receives challenge");
        assert_eq!(
            transport.sent_payloads(),
            vec![request.clone(), request],
            "transport acceptance is not an application response; retry bytes stay exact"
        );
    }

    #[cfg(feature = "home-assistant-cloudlink")]
    #[tokio::test(start_paused = true)]
    async fn challenge_request_retry_has_a_local_monotonic_upper_bound() {
        let now = current_timestamp_ms()
            .expect("representable system clock")
            .get();
        let transport = Arc::new(RetryChallengeTransport::silent());
        let shutdown = CancellationToken::new();
        let task_transport = transport.clone();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            receive_cloudlink_challenge_with_retry(
                task_transport.as_ref(),
                br#"{"session":"request"}"#,
                now + (24 * 60 * 60 * 1_000),
                &task_shutdown,
            )
            .await
        });

        for _ in 0..100 {
            if !transport.sent_payloads().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(
            CLOUDLINK_CHALLENGE_REQUEST_LIFETIME_MS,
        ))
        .await;
        let error = task
            .await
            .expect("bounded retry task")
            .expect_err("silent Cloud must time out locally");
        assert_eq!(error.kind(), PortErrorKind::Timeout);
        assert!(
            transport.sent_payloads().len() <= 2,
            "advancing to the cap cannot trigger an unbounded retry burst"
        );
    }
}
