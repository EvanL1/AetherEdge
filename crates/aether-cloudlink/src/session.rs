//! Session negotiation and monotonic epoch binding.

use std::collections::HashSet;

use aether_ports::CloudLinkSessionBinding;
use serde::{Deserialize, Serialize};

use crate::validation::{canonical_u64, identifier, positive_u64, protocol_version, schema, uuid};
use crate::{CLOUDLINK_PROTOCOL, CLOUDLINK_PROTOCOL_VERSION, CloudLinkCodecError};

const HELLO_SCHEMA: &str = "aether.cloudlink.session-hello.v1";
const ACCEPTED_SCHEMA: &str = "aether.cloudlink.session-accepted.v1";
const CHALLENGE_SCHEMA: &str = "aether.cloudlink.session-challenge.v1";
const CHALLENGE_REQUEST_SCHEMA: &str = "aether.cloudlink.session-challenge-request.v1";

/// One client/server cursor claim used during resume negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeCursor {
    stream_id: String,
    stream_epoch: String,
    acknowledged_position: String,
}

impl ResumeCursor {
    /// Creates a canonical resume cursor.
    pub fn new(
        stream_id: impl Into<String>,
        stream_epoch: u64,
        acknowledged_position: u64,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            stream_id: stream_id.into(),
            stream_epoch: stream_epoch.to_string(),
            acknowledged_position: acknowledged_position.to_string(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        identifier(&self.stream_id, "resume.stream_id", 128)?;
        positive_u64(&self.stream_epoch, "resume.stream_epoch")?;
        canonical_u64(&self.acknowledged_position, "resume.acknowledged_position")?;
        Ok(())
    }

    /// Returns the logical stream ID.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the parsed stream epoch.
    #[must_use]
    pub fn stream_epoch(&self) -> u64 {
        self.stream_epoch.parse().unwrap_or_default()
    }

    /// Returns the parsed durable server cursor.
    #[must_use]
    pub fn acknowledged_position(&self) -> u64 {
        self.acknowledged_position.parse().unwrap_or_default()
    }
}

/// Experimental origin-evidence mode selected for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialOriginModel {
    /// The Gateway signs the frozen session and per-uplink objects.
    GatewaySigned,
    /// A configured trusted connector supplies evidence outside the payload.
    TrustedConnectorBrokerAttestation,
}

/// Structurally validated Ed25519 signature material.
///
/// This alpha type validates encoding and redacts diagnostics. It does not
/// claim production key provisioning, rotation, revocation, or verification.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageAuthentication {
    key_id: String,
    algorithm: String,
    signature: String,
}

impl MessageAuthentication {
    /// Creates alpha signature material using the one frozen algorithm.
    pub fn new(
        key_id: impl Into<String>,
        signature: impl Into<String>,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            key_id: key_id.into(),
            algorithm: "Ed25519".to_string(),
            signature: signature.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        identifier(&self.key_id, "message_authentication.key_id", 128)?;
        if self.algorithm != "Ed25519"
            || self.signature.len() != 86
            || !self
                .signature
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(CloudLinkCodecError::InvalidField {
                field: "message_authentication",
                message: "must be an unpadded-base64url Ed25519 signature",
            });
        }
        Ok(())
    }

    /// Returns the public key identity without exposing key material.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Returns the canonical signature encoding.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }
}

impl core::fmt::Debug for MessageAuthentication {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("MessageAuthentication([REDACTED])")
    }
}

/// Cloud-to-edge one-time challenge for the experimental signed origin model.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionChallenge {
    schema: String,
    protocol: String,
    message_kind: String,
    gateway_id: String,
    challenge_id: String,
    cloud_nonce: String,
    issued_at_ms: String,
    expires_at_ms: String,
    cloud_signature: MessageAuthentication,
}

impl core::fmt::Debug for SessionChallenge {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionChallenge")
            .field("gateway_id", &"[REDACTED]")
            .field("challenge_id", &"[REDACTED]")
            .field("cloud_signature", &self.cloud_signature)
            .finish_non_exhaustive()
    }
}

impl SessionChallenge {
    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, CHALLENGE_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL || self.message_kind != "session-challenge" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        uuid(&self.gateway_id, "gateway_id")?;
        uuid(&self.challenge_id, "challenge_id")?;
        if self.cloud_nonce.len() != 43
            || !self
                .cloud_nonce
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(CloudLinkCodecError::InvalidField {
                field: "cloud_nonce",
                message: "must be 32 bytes encoded as unpadded base64url",
            });
        }
        let issued = canonical_u64(&self.issued_at_ms, "issued_at_ms")?;
        let expires = canonical_u64(&self.expires_at_ms, "expires_at_ms")?;
        if expires < issued {
            return Err(CloudLinkCodecError::InvalidField {
                field: "expires_at_ms",
                message: "must be after issued_at_ms",
            });
        }
        self.cloud_signature.validate()
    }

    /// Returns the gateway routing claim carried by the trusted broker channel.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    /// Returns the one-time challenge identity.
    #[must_use]
    pub fn challenge_id(&self) -> &str {
        &self.challenge_id
    }

    /// Returns the challenge issue time.
    #[must_use]
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms.parse().unwrap_or_default()
    }

    /// Returns the challenge expiry time.
    #[must_use]
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms.parse().unwrap_or_default()
    }

    pub(crate) fn cloud_nonce(&self) -> &str {
        &self.cloud_nonce
    }

    pub(crate) fn issued_at_ms_wire(&self) -> &str {
        &self.issued_at_ms
    }

    pub(crate) fn expires_at_ms_wire(&self) -> &str {
        &self.expires_at_ms
    }

    pub(crate) const fn cloud_signature(&self) -> &MessageAuthentication {
        &self.cloud_signature
    }
}

/// Credential reference used before a Cloud challenge has been issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeCredentialBinding {
    credential_id: String,
    generation: String,
}

/// Gateway-to-Cloud request for one signed session challenge.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionChallengeRequest {
    schema: String,
    protocol: String,
    message_kind: String,
    gateway_id: String,
    credential_binding: ChallengeCredentialBinding,
    offered_protocol_versions: Vec<String>,
    client_nonce: String,
    resume: Vec<ResumeCursor>,
}

impl core::fmt::Debug for SessionChallengeRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionChallengeRequest")
            .field("authentication_transcript", &"[REDACTED]")
            .field(
                "offered_version_count",
                &self.offered_protocol_versions.len(),
            )
            .field("resume_cursor_count", &self.resume.len())
            .finish()
    }
}

impl SessionChallengeRequest {
    /// Creates the exact closed request that precedes a Gateway-signed hello.
    pub fn new(
        gateway_id: impl Into<String>,
        credential_id: impl Into<String>,
        credential_generation: u64,
        offered_protocol_versions: Vec<String>,
        client_nonce: impl Into<String>,
        resume: Vec<ResumeCursor>,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            schema: CHALLENGE_REQUEST_SCHEMA.to_owned(),
            protocol: CLOUDLINK_PROTOCOL.to_owned(),
            message_kind: "session-challenge-request".to_owned(),
            gateway_id: gateway_id.into(),
            credential_binding: ChallengeCredentialBinding {
                credential_id: credential_id.into(),
                generation: credential_generation.to_string(),
            },
            offered_protocol_versions,
            client_nonce: client_nonce.into(),
            resume,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, CHALLENGE_REQUEST_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL || self.message_kind != "session-challenge-request" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        uuid(&self.gateway_id, "gateway_id")?;
        identifier(
            &self.credential_binding.credential_id,
            "credential_binding.credential_id",
            256,
        )?;
        positive_u64(
            &self.credential_binding.generation,
            "credential_binding.generation",
        )?;
        validate_offered_versions(&self.offered_protocol_versions)?;
        validate_nonce(&self.client_nonce, "client_nonce")?;
        validate_cursors(&self.resume)
    }

    /// Returns the Gateway identity bound to the request.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    pub(crate) fn credential_id(&self) -> &str {
        &self.credential_binding.credential_id
    }

    pub(crate) fn credential_generation(&self) -> u64 {
        self.credential_binding
            .generation
            .parse()
            .unwrap_or_default()
    }

    pub(crate) fn credential_generation_wire(&self) -> &str {
        &self.credential_binding.generation
    }

    pub(crate) fn offered_protocol_versions(&self) -> &[String] {
        &self.offered_protocol_versions
    }

    pub(crate) fn client_nonce(&self) -> &str {
        &self.client_nonce
    }

    pub(crate) fn resume(&self) -> &[ResumeCursor] {
        &self.resume
    }
}

/// Credential reference and declared origin model used during establishment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialBinding {
    credential_id: String,
    generation: String,
    origin_model: CredentialOriginModel,
}

/// Edge-to-cloud session hello.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHello {
    schema: String,
    protocol: String,
    message_kind: String,
    gateway_id: String,
    credential_binding: CredentialBinding,
    challenge_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_signature: Option<MessageAuthentication>,
    offered_protocol_versions: Vec<String>,
    client_nonce: String,
    resume: Vec<ResumeCursor>,
}

impl core::fmt::Debug for SessionHello {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionHello")
            .field("authentication_transcript", &"[REDACTED]")
            .field("resume_cursor_count", &self.resume.len())
            .finish_non_exhaustive()
    }
}

impl SessionHello {
    /// Creates a Gateway-signed alpha hello without a password or private key.
    #[allow(clippy::too_many_arguments)]
    pub fn new_gateway_signed(
        gateway_id: impl Into<String>,
        credential_id: impl Into<String>,
        credential_generation: u64,
        challenge_id: impl Into<String>,
        gateway_key_id: impl Into<String>,
        gateway_signature: MessageAuthentication,
        offered_protocol_versions: Vec<String>,
        client_nonce: impl Into<String>,
        resume: Vec<ResumeCursor>,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            schema: HELLO_SCHEMA.to_string(),
            protocol: CLOUDLINK_PROTOCOL.to_string(),
            message_kind: "session-hello".to_string(),
            gateway_id: gateway_id.into(),
            credential_binding: CredentialBinding {
                credential_id: credential_id.into(),
                generation: credential_generation.to_string(),
                origin_model: CredentialOriginModel::GatewaySigned,
            },
            challenge_id: challenge_id.into(),
            gateway_key_id: Some(gateway_key_id.into()),
            gateway_signature: Some(gateway_signature),
            offered_protocol_versions,
            client_nonce: client_nonce.into(),
            resume,
        };
        value.validate()?;
        Ok(value)
    }

    /// Creates a hello whose identity evidence is bound to the trusted MQTT connector.
    ///
    /// The caller must use an authenticated TLS broker connection and exact
    /// per-gateway ACLs. No signature placeholder is accepted in the payload.
    #[allow(clippy::too_many_arguments)]
    pub fn new_trusted_connector_broker_attested(
        gateway_id: impl Into<String>,
        credential_id: impl Into<String>,
        credential_generation: u64,
        challenge_id: impl Into<String>,
        offered_protocol_versions: Vec<String>,
        client_nonce: impl Into<String>,
        resume: Vec<ResumeCursor>,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            schema: HELLO_SCHEMA.to_string(),
            protocol: CLOUDLINK_PROTOCOL.to_string(),
            message_kind: "session-hello".to_string(),
            gateway_id: gateway_id.into(),
            credential_binding: CredentialBinding {
                credential_id: credential_id.into(),
                generation: credential_generation.to_string(),
                origin_model: CredentialOriginModel::TrustedConnectorBrokerAttestation,
            },
            challenge_id: challenge_id.into(),
            gateway_key_id: None,
            gateway_signature: None,
            offered_protocol_versions,
            client_nonce: client_nonce.into(),
            resume,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, HELLO_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL || self.message_kind != "session-hello" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        uuid(&self.gateway_id, "gateway_id")?;
        identifier(
            &self.credential_binding.credential_id,
            "credential_binding.credential_id",
            256,
        )?;
        positive_u64(
            &self.credential_binding.generation,
            "credential_binding.generation",
        )?;
        uuid(&self.challenge_id, "challenge_id")?;
        match self.credential_binding.origin_model {
            CredentialOriginModel::GatewaySigned => {
                let key_id =
                    self.gateway_key_id
                        .as_deref()
                        .ok_or(CloudLinkCodecError::InvalidField {
                            field: "gateway_key_id",
                            message: "is required for gateway-signed origin",
                        })?;
                identifier(key_id, "gateway_key_id", 128)?;
                let signature =
                    self.gateway_signature
                        .as_ref()
                        .ok_or(CloudLinkCodecError::InvalidField {
                            field: "gateway_signature",
                            message: "is required for gateway-signed origin",
                        })?;
                signature.validate()?;
                if signature.key_id() != key_id {
                    return Err(CloudLinkCodecError::InvalidField {
                        field: "gateway_signature.key_id",
                        message: "must match gateway_key_id",
                    });
                }
            },
            CredentialOriginModel::TrustedConnectorBrokerAttestation => {
                if self.gateway_key_id.is_some() || self.gateway_signature.is_some() {
                    return Err(CloudLinkCodecError::InvalidField {
                        field: "credential_binding.origin_model",
                        message: "trusted connector evidence is external to the payload",
                    });
                }
            },
        }
        validate_offered_versions(&self.offered_protocol_versions)?;
        validate_nonce(&self.client_nonce, "client_nonce")?;
        validate_cursors(&self.resume)?;
        Ok(())
    }

    /// Returns the gateway routing claim.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }
}

/// Cloud-to-edge accepted session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAccepted {
    schema: String,
    protocol: String,
    message_kind: String,
    gateway_id: String,
    selected_protocol_version: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    server_time_ms: String,
    heartbeat_interval_ms: String,
    resume: Vec<ResumeCursor>,
}

impl SessionAccepted {
    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, ACCEPTED_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL || self.message_kind != "session-accepted" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        uuid(&self.gateway_id, "gateway_id")?;
        protocol_version(&self.selected_protocol_version)?;
        uuid(&self.session_id, "session_id")?;
        positive_u64(&self.session_epoch, "session_epoch")?;
        positive_u64(&self.credential_generation, "credential_generation")?;
        canonical_u64(&self.server_time_ms, "server_time_ms")?;
        positive_u64(&self.heartbeat_interval_ms, "heartbeat_interval_ms")?;
        validate_cursors(&self.resume)?;
        Ok(())
    }

    /// Validates negotiation and creates a current verified session binding.
    pub fn bind(
        &self,
        expected_gateway_id: &str,
        expected_credential_generation: u64,
        offered_versions: &[&str],
        previous_session_epoch: u64,
    ) -> Result<SessionBinding, CloudLinkCodecError> {
        self.validate()?;
        if self.gateway_id != expected_gateway_id {
            return Err(CloudLinkCodecError::SessionMismatch);
        }
        if !offered_versions.contains(&self.selected_protocol_version.as_str()) {
            return Err(CloudLinkCodecError::VersionNegotiationFailed);
        }
        let session_epoch = positive_u64(&self.session_epoch, "session_epoch")?;
        let credential_generation =
            positive_u64(&self.credential_generation, "credential_generation")?;
        if session_epoch <= previous_session_epoch
            || credential_generation != expected_credential_generation
        {
            return Err(CloudLinkCodecError::SessionMismatch);
        }
        Ok(SessionBinding {
            gateway_id: self.gateway_id.clone(),
            protocol_version: self.selected_protocol_version.clone(),
            session_id: self.session_id.clone(),
            session_epoch,
            credential_generation,
        })
    }

    /// Returns the server-authoritative durable cursors for resume.
    #[must_use]
    pub fn resume_cursors(&self) -> &[ResumeCursor] {
        &self.resume
    }

    /// Returns the negotiated heartbeat interval in milliseconds.
    #[must_use]
    pub fn heartbeat_interval_ms(&self) -> u64 {
        self.heartbeat_interval_ms.parse().unwrap_or_default()
    }
}

/// Verified identity/session values carried by post-establishment messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    gateway_id: String,
    protocol_version: String,
    session_id: String,
    session_epoch: u64,
    credential_generation: u64,
}

impl SessionBinding {
    /// Creates a verified binding for deterministic adapters and tests.
    pub fn new(
        gateway_id: impl Into<String>,
        session_id: impl Into<String>,
        session_epoch: u64,
        credential_generation: u64,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            gateway_id: gateway_id.into(),
            protocol_version: CLOUDLINK_PROTOCOL_VERSION.to_string(),
            session_id: session_id.into(),
            session_epoch,
            credential_generation,
        };
        uuid(&value.gateway_id, "gateway_id")?;
        uuid(&value.session_id, "session_id")?;
        if session_epoch == 0 || credential_generation == 0 {
            return Err(CloudLinkCodecError::SessionMismatch);
        }
        Ok(value)
    }

    /// Returns the verified gateway identity.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    /// Returns the negotiated protocol version.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Returns the opaque session ID.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the monotonic session epoch.
    #[must_use]
    pub const fn session_epoch(&self) -> u64 {
        self.session_epoch
    }

    /// Returns the verified credential generation.
    #[must_use]
    pub const fn credential_generation(&self) -> u64 {
        self.credential_generation
    }

    /// Converts to the smaller spool validation binding.
    #[must_use]
    pub fn spool_binding(&self) -> CloudLinkSessionBinding {
        CloudLinkSessionBinding::new(self.session_id.clone(), self.session_epoch)
    }
}

fn validate_cursors(cursors: &[ResumeCursor]) -> Result<(), CloudLinkCodecError> {
    if cursors.len() > 32 {
        return Err(CloudLinkCodecError::InvalidField {
            field: "resume",
            message: "contains more than 32 stream cursors",
        });
    }
    let mut identities = HashSet::new();
    for cursor in cursors {
        cursor.validate()?;
        if !identities.insert((&cursor.stream_id, &cursor.stream_epoch)) {
            return Err(CloudLinkCodecError::InvalidField {
                field: "resume",
                message: "must contain unique stream and epoch identities",
            });
        }
    }
    Ok(())
}

fn validate_offered_versions(versions: &[String]) -> Result<(), CloudLinkCodecError> {
    if versions.is_empty() || versions.len() > 8 {
        return Err(CloudLinkCodecError::InvalidField {
            field: "offered_protocol_versions",
            message: "must contain between one and eight versions",
        });
    }
    let mut unique = HashSet::new();
    for version in versions {
        protocol_version(version)?;
        if !unique.insert(version) {
            return Err(CloudLinkCodecError::InvalidField {
                field: "offered_protocol_versions",
                message: "must contain unique versions",
            });
        }
    }
    Ok(())
}

fn validate_nonce(value: &str, field: &'static str) -> Result<(), CloudLinkCodecError> {
    if value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(CloudLinkCodecError::InvalidField {
            field,
            message: "must be 32 bytes encoded as unpadded base64url",
        })
    }
}
