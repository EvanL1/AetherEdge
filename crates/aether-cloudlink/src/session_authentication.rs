//! Exact RFC 8785 session authentication projections and Ed25519 operations.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::session::{MessageAuthentication, SessionChallenge, SessionChallengeRequest};
use crate::validation::{canonical_u64, digest, identifier, positive_u64, uuid};
use crate::{CloudLinkCodecError, SessionHello};

const CHALLENGE_SIGNING_SCHEMA: &str = "aether.cloudlink.session-challenge-signing.v1alpha1";
const ESTABLISHMENT_SIGNING_SCHEMA: &str =
    "aether.cloudlink.session-establishment-signing.v1alpha1";
const UPLINK_SIGNING_SCHEMA: &str = "aether.cloudlink.uplink-signing.v1alpha1";

#[derive(Clone)]
struct GatewayUplinkSigner {
    key_id: String,
    key: Arc<SigningKey>,
}

impl GatewayUplinkSigner {
    fn sign(
        &self,
        projection: &UplinkSigningProjection,
    ) -> Result<MessageAuthentication, CloudLinkCodecError> {
        let signing_bytes = projection.canonical_bytes()?;
        MessageAuthentication::new(
            self.key_id.clone(),
            URL_SAFE_NO_PAD.encode(self.key.sign(&signing_bytes).to_bytes()),
        )
    }
}

/// Selected origin-evidence mechanism for one accepted session's uplinks.
///
/// Gateway-signed values can only be derived from a
/// [`GatewaySessionAuthenticator`], so hello and per-message signatures share
/// the same key instance and key identity. Trusted-connector evidence remains
/// outside the payload and never creates a placeholder signature.
#[derive(Clone)]
pub struct UplinkAuthentication {
    mode: UplinkAuthenticationMode,
}

#[derive(Clone)]
enum UplinkAuthenticationMode {
    GatewaySigned(GatewayUplinkSigner),
    TrustedConnectorBrokerAttestation,
}

impl UplinkAuthentication {
    /// Selects external trusted-connector evidence without payload authentication.
    #[must_use]
    pub const fn trusted_connector_broker_attestation() -> Self {
        Self {
            mode: UplinkAuthenticationMode::TrustedConnectorBrokerAttestation,
        }
    }

    /// Signs one exact projection or returns no payload field for a trusted connector.
    pub fn authenticate(
        &self,
        projection: &UplinkSigningProjection,
    ) -> Result<Option<MessageAuthentication>, CloudLinkCodecError> {
        match &self.mode {
            UplinkAuthenticationMode::GatewaySigned(signer) => signer.sign(projection).map(Some),
            UplinkAuthenticationMode::TrustedConnectorBrokerAttestation => Ok(None),
        }
    }

    /// Returns the session Gateway key identity when this is a signed origin.
    #[must_use]
    pub fn gateway_key_id(&self) -> Option<&str> {
        match &self.mode {
            UplinkAuthenticationMode::GatewaySigned(signer) => Some(&signer.key_id),
            UplinkAuthenticationMode::TrustedConnectorBrokerAttestation => None,
        }
    }
}

impl core::fmt::Debug for UplinkAuthentication {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("UplinkAuthentication([REDACTED])")
    }
}

/// Exact 13-field, language-neutral per-uplink signing object.
///
/// All protocol integers remain canonical decimal strings and absent delivery
/// values are encoded as JSON `null`, exactly as frozen by AetherContracts.
#[derive(Clone, Serialize)]
pub struct UplinkSigningProjection {
    schema: &'static str,
    gateway_id: String,
    credential_generation: String,
    session_id: String,
    session_epoch: String,
    message_kind: String,
    sent_at_ms: String,
    expires_at_ms: Option<String>,
    stream_id: Option<String>,
    stream_epoch: Option<String>,
    position: Option<String>,
    batch_id: Option<String>,
    business_digest: Option<String>,
}

impl UplinkSigningProjection {
    /// Creates the exact signing projection for a durable delivery envelope.
    #[allow(clippy::too_many_arguments)]
    pub fn delivery(
        gateway_id: &str,
        credential_generation: &str,
        session_id: &str,
        session_epoch: &str,
        message_kind: &str,
        sent_at_ms: &str,
        expires_at_ms: Option<&str>,
        stream_id: &str,
        stream_epoch: &str,
        position: &str,
        batch_id: &str,
        business_digest: &str,
    ) -> Result<Self, CloudLinkCodecError> {
        uuid(gateway_id, "gateway_id")?;
        positive_u64(credential_generation, "credential_generation")?;
        uuid(session_id, "session_id")?;
        positive_u64(session_epoch, "session_epoch")?;
        identifier(message_kind, "message_kind", 128)?;
        let sent_at = canonical_u64(sent_at_ms, "sent_at_ms")?;
        if let Some(expires_at_ms) = expires_at_ms
            && canonical_u64(expires_at_ms, "expires_at_ms")? < sent_at
        {
            return Err(CloudLinkCodecError::InvalidField {
                field: "expires_at_ms",
                message: "must be after sent_at_ms",
            });
        }
        identifier(stream_id, "stream_id", 128)?;
        positive_u64(stream_epoch, "stream_epoch")?;
        positive_u64(position, "position")?;
        identifier(batch_id, "batch_id", 128)?;
        digest(business_digest, "business_digest")?;
        Ok(Self {
            schema: UPLINK_SIGNING_SCHEMA,
            gateway_id: gateway_id.to_owned(),
            credential_generation: credential_generation.to_owned(),
            session_id: session_id.to_owned(),
            session_epoch: session_epoch.to_owned(),
            message_kind: message_kind.to_owned(),
            sent_at_ms: sent_at_ms.to_owned(),
            expires_at_ms: expires_at_ms.map(str::to_owned),
            stream_id: Some(stream_id.to_owned()),
            stream_epoch: Some(stream_epoch.to_owned()),
            position: Some(position.to_owned()),
            batch_id: Some(batch_id.to_owned()),
            business_digest: Some(business_digest.to_owned()),
        })
    }

    /// Creates the exact signing projection for an edge heartbeat.
    pub fn heartbeat(
        gateway_id: &str,
        credential_generation: &str,
        session_id: &str,
        session_epoch: &str,
        message_kind: &str,
        observed_at_ms: &str,
    ) -> Result<Self, CloudLinkCodecError> {
        uuid(gateway_id, "gateway_id")?;
        positive_u64(credential_generation, "credential_generation")?;
        uuid(session_id, "session_id")?;
        positive_u64(session_epoch, "session_epoch")?;
        if message_kind != "heartbeat" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: message_kind.to_owned(),
            });
        }
        canonical_u64(observed_at_ms, "observed_at_ms")?;
        Ok(Self {
            schema: UPLINK_SIGNING_SCHEMA,
            gateway_id: gateway_id.to_owned(),
            credential_generation: credential_generation.to_owned(),
            session_id: session_id.to_owned(),
            session_epoch: session_epoch.to_owned(),
            message_kind: message_kind.to_owned(),
            sent_at_ms: observed_at_ms.to_owned(),
            expires_at_ms: None,
            stream_id: None,
            stream_epoch: None,
            position: None,
            batch_id: None,
            business_digest: None,
        })
    }

    /// Returns the exact RFC 8785 JCS UTF-8 signing bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CloudLinkCodecError> {
        serde_json_canonicalizer::to_vec(self)
            .map_err(|source| CloudLinkCodecError::CanonicalJson { source })
    }
}

impl core::fmt::Debug for UplinkSigningProjection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("UplinkSigningProjection([REDACTED])")
    }
}

/// Verified Cloud challenge whose sensitive transcript is redacted from diagnostics.
#[derive(Clone)]
pub struct VerifiedSessionChallenge {
    challenge: SessionChallenge,
}

impl core::fmt::Debug for VerifiedSessionChallenge {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VerifiedSessionChallenge")
            .field("authentication_transcript", &"[REDACTED]")
            .finish()
    }
}

impl VerifiedSessionChallenge {
    /// Returns the verified one-time challenge identity.
    #[must_use]
    pub fn challenge_id(&self) -> &str {
        self.challenge.challenge_id()
    }
}

/// Separate Cloud verifier and Gateway signer for experimental session establishment.
///
/// This verifies the frozen challenge and hello signing projections, but the
/// current unsigned `session-accepted` wire message is not transcript-bound.
/// Therefore this type is not proof of production session authentication.
pub struct GatewaySessionAuthenticator {
    cloud_key_id: String,
    cloud_key: VerifyingKey,
    gateway_signer: GatewayUplinkSigner,
}

impl core::fmt::Debug for GatewaySessionAuthenticator {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("GatewaySessionAuthenticator([REDACTED])")
    }
}

impl GatewaySessionAuthenticator {
    /// Creates an authenticator from canonical unpadded-Base64url key material.
    ///
    /// Both encoded inputs and temporary decoded private-key bytes are cleared
    /// on every success or failure path.
    pub fn from_base64url(
        cloud_key_id: impl Into<String>,
        cloud_verifying_key: String,
        gateway_key_id: impl Into<String>,
        gateway_signing_key: String,
    ) -> Result<Self, CloudLinkCodecError> {
        let cloud_verifying_key = Zeroizing::new(cloud_verifying_key);
        let gateway_signing_key = Zeroizing::new(gateway_signing_key);
        let cloud_key = decode_key(&cloud_verifying_key)?;
        let gateway_key = decode_key(&gateway_signing_key)?;
        Self::new_with_zeroizing_key(cloud_key_id, *cloud_key, gateway_key_id, gateway_key)
    }

    /// Creates an authenticator from raw Ed25519 public-key and private-seed bytes.
    pub fn new(
        cloud_key_id: impl Into<String>,
        cloud_verifying_key: [u8; 32],
        gateway_key_id: impl Into<String>,
        gateway_signing_key: [u8; 32],
    ) -> Result<Self, CloudLinkCodecError> {
        let gateway_signing_key = Zeroizing::new(gateway_signing_key);
        Self::new_with_zeroizing_key(
            cloud_key_id,
            cloud_verifying_key,
            gateway_key_id,
            gateway_signing_key,
        )
    }

    fn new_with_zeroizing_key(
        cloud_key_id: impl Into<String>,
        cloud_verifying_key: [u8; 32],
        gateway_key_id: impl Into<String>,
        gateway_signing_key: Zeroizing<[u8; 32]>,
    ) -> Result<Self, CloudLinkCodecError> {
        let cloud_key_id = cloud_key_id.into();
        let gateway_key_id = gateway_key_id.into();
        identifier(&cloud_key_id, "cloud_signature.key_id", 128)?;
        identifier(&gateway_key_id, "gateway_key_id", 128)?;
        let gateway_key = SigningKey::from_bytes(&gateway_signing_key);
        let cloud_key = VerifyingKey::from_bytes(&cloud_verifying_key).map_err(|_source| {
            authentication_invalid(
                "cloud_signature",
                "configured Cloud Ed25519 public key is invalid",
            )
        })?;
        Ok(Self {
            cloud_key_id,
            cloud_key,
            gateway_signer: GatewayUplinkSigner {
                key_id: gateway_key_id,
                key: Arc::new(gateway_key),
            },
        })
    }

    /// Verifies challenge scope, issue time, strict expiry, key identity, and signature.
    pub fn verify_challenge(
        &self,
        challenge: &SessionChallenge,
        expected_gateway_id: &str,
        evaluation_time_ms: u64,
    ) -> Result<VerifiedSessionChallenge, CloudLinkCodecError> {
        challenge.validate()?;
        if evaluation_time_ms >= challenge.expires_at_ms() {
            return Err(CloudLinkCodecError::MessageExpired);
        }
        if challenge.gateway_id() != expected_gateway_id
            || evaluation_time_ms < challenge.issued_at_ms()
        {
            return Err(authentication_invalid(
                "cloud_signature",
                "challenge scope or validity window is invalid",
            ));
        }
        let authentication = challenge.cloud_signature();
        if authentication.key_id() != self.cloud_key_id {
            return Err(authentication_invalid(
                "cloud_signature",
                "challenge key identity does not match configured Cloud key",
            ));
        }
        let signature = decode_signature(authentication.signature())?;
        let signing_bytes = challenge_signing_bytes(challenge)?;
        self.cloud_key
            .verify_strict(&signing_bytes, &signature)
            .map_err(|_source| {
                authentication_invalid(
                    "cloud_signature",
                    "challenge Ed25519 signature verification failed",
                )
            })?;
        Ok(VerifiedSessionChallenge {
            challenge: challenge.clone(),
        })
    }

    /// Signs the exact establishment projection using the verified persisted challenge.
    pub fn sign_hello(
        &self,
        verified: &VerifiedSessionChallenge,
        request: &SessionChallengeRequest,
    ) -> Result<SessionHello, CloudLinkCodecError> {
        request.validate()?;
        if request.gateway_id() != verified.challenge.gateway_id() {
            return Err(authentication_invalid(
                "gateway_signature",
                "challenge and request Gateway identities do not match",
            ));
        }
        let signing_bytes =
            hello_signing_bytes(&verified.challenge, request, &self.gateway_signer.key_id)?;
        let authentication = MessageAuthentication::new(
            self.gateway_signer.key_id.clone(),
            URL_SAFE_NO_PAD.encode(self.gateway_signer.key.sign(&signing_bytes).to_bytes()),
        )?;
        SessionHello::new_gateway_signed(
            request.gateway_id(),
            request.credential_id(),
            request.credential_generation(),
            verified.challenge.challenge_id(),
            self.gateway_signer.key_id.clone(),
            authentication,
            request.offered_protocol_versions().to_vec(),
            request.client_nonce(),
            request.resume().to_vec(),
        )
    }

    /// Reuses the exact session Gateway key for every per-uplink signature.
    #[must_use]
    pub fn uplink_authentication(&self) -> UplinkAuthentication {
        UplinkAuthentication {
            mode: UplinkAuthenticationMode::GatewaySigned(self.gateway_signer.clone()),
        }
    }
}

#[derive(Serialize)]
struct ChallengeSigningProjection<'a> {
    schema: &'static str,
    gateway_id: &'a str,
    challenge_id: &'a str,
    cloud_nonce: &'a str,
    issued_at_ms: &'a str,
    expires_at_ms: &'a str,
}

fn challenge_signing_bytes(challenge: &SessionChallenge) -> Result<Vec<u8>, CloudLinkCodecError> {
    serde_json_canonicalizer::to_vec(&ChallengeSigningProjection {
        schema: CHALLENGE_SIGNING_SCHEMA,
        gateway_id: challenge.gateway_id(),
        challenge_id: challenge.challenge_id(),
        cloud_nonce: challenge.cloud_nonce(),
        issued_at_ms: challenge.issued_at_ms_wire(),
        expires_at_ms: challenge.expires_at_ms_wire(),
    })
    .map_err(|source| CloudLinkCodecError::CanonicalJson { source })
}

#[derive(Serialize)]
struct EstablishmentSigningProjection<'a> {
    schema: &'static str,
    gateway_id: &'a str,
    credential_id: &'a str,
    credential_generation: &'a str,
    gateway_key_id: &'a str,
    challenge_id: &'a str,
    cloud_nonce: &'a str,
    client_nonce: &'a str,
    offered_protocol_versions: &'a [String],
    resume: &'a [crate::ResumeCursor],
}

fn hello_signing_bytes(
    challenge: &SessionChallenge,
    request: &SessionChallengeRequest,
    gateway_key_id: &str,
) -> Result<Vec<u8>, CloudLinkCodecError> {
    serde_json_canonicalizer::to_vec(&EstablishmentSigningProjection {
        schema: ESTABLISHMENT_SIGNING_SCHEMA,
        gateway_id: request.gateway_id(),
        credential_id: request.credential_id(),
        credential_generation: request.credential_generation_wire(),
        gateway_key_id,
        challenge_id: challenge.challenge_id(),
        cloud_nonce: challenge.cloud_nonce(),
        client_nonce: request.client_nonce(),
        offered_protocol_versions: request.offered_protocol_versions(),
        resume: request.resume(),
    })
    .map_err(|source| CloudLinkCodecError::CanonicalJson { source })
}

fn decode_signature(value: &str) -> Result<Signature, CloudLinkCodecError> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_source| {
        authentication_invalid(
            "cloud_signature",
            "challenge signature is not canonical unpadded Base64url",
        )
    })?;
    if decoded.len() != 64 || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(authentication_invalid(
            "cloud_signature",
            "challenge signature is not canonical unpadded Base64url",
        ));
    }
    Signature::from_slice(&decoded).map_err(|_source| {
        authentication_invalid(
            "cloud_signature",
            "challenge signature is not an Ed25519 signature",
        )
    })
}

fn decode_key(value: &str) -> Result<Zeroizing<[u8; 32]>, CloudLinkCodecError> {
    let decoded = Zeroizing::new(URL_SAFE_NO_PAD.decode(value).map_err(|_source| {
        authentication_invalid(
            "session_authentication_key_material",
            "Ed25519 key is not canonical unpadded Base64url",
        )
    })?);
    if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(&*decoded) != value {
        return Err(authentication_invalid(
            "session_authentication_key_material",
            "Ed25519 key is not canonical unpadded Base64url",
        ));
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn authentication_invalid(field: &'static str, message: &'static str) -> CloudLinkCodecError {
    CloudLinkCodecError::InvalidField { field, message }
}
