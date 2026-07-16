//! Strict JSON codec, canonical business digests, and delivery envelopes.

use aether_domain::{PointSample, TimestampMs};
use aether_ports::{
    CloudLinkDurableAck, CloudLinkEnqueue, CloudLinkMessageKind, CloudLinkRecord,
    CloudLinkSessionBinding,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::session::{SessionAccepted, SessionBinding, SessionChallenge, SessionHello};
use crate::telemetry::{TelemetryBatch, TopologyBinding};
use crate::validation::{
    canonical_u64, digest, identifier, positive_u64, protocol_version, schema, traceparent, uuid,
};
use crate::{
    CLOUDLINK_PROTOCOL, CLOUDLINK_PROTOCOL_VERSION, CloudLinkCodecError,
    MAX_CLOUDLINK_MESSAGE_BYTES,
};

const ENVELOPE_SCHEMA: &str = "aether.cloudlink.envelope.v1";
const HEARTBEAT_SCHEMA: &str = "aether.cloudlink.heartbeat.v1";
const DURABLE_ACK_SCHEMA: &str = "aether.cloudlink.durable-ack.v1";
const REPLAY_REQUEST_SCHEMA: &str = "aether.cloudlink.replay-request.v1";

/// Strict codec entry point.
pub struct CloudLinkCodec;

impl CloudLinkCodec {
    /// Strictly decodes one bounded candidate message.
    pub fn decode(bytes: &[u8]) -> Result<CandidateMessage, CloudLinkCodecError> {
        bound(bytes.len())?;
        let discriminator: Value = serde_json::from_slice(bytes)?;
        let schema_value = discriminator.get("schema").and_then(Value::as_str).ok_or(
            CloudLinkCodecError::InvalidField {
                field: "schema",
                message: "is required and must be a string",
            },
        )?;
        match schema_value {
            "aether.cloudlink.session-challenge.v1" => {
                let value: SessionChallenge = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::SessionChallenge(value))
            },
            "aether.cloudlink.session-hello.v1" => {
                let value: SessionHello = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::SessionHello(value))
            },
            "aether.cloudlink.session-accepted.v1" => {
                let value: SessionAccepted = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::SessionAccepted(value))
            },
            HEARTBEAT_SCHEMA => {
                let value: HeartbeatMessage = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::Heartbeat(value))
            },
            ENVELOPE_SCHEMA => {
                let value: DeliveryEnvelope = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::Delivery(value))
            },
            DURABLE_ACK_SCHEMA => {
                let value: DurableAckMessage = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::DurableAck(value))
            },
            REPLAY_REQUEST_SCHEMA => {
                let value: ReplayRequest = serde_json::from_slice(bytes)?;
                value.validate()?;
                Ok(CandidateMessage::ReplayRequest(value))
            },
            other => Err(CloudLinkCodecError::UnsupportedMessage {
                found: other.to_string(),
            }),
        }
    }

    /// Encodes any validated candidate value and enforces the transport bound.
    pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CloudLinkCodecError> {
        let bytes = serde_json_canonicalizer::to_vec(value)
            .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
        bound(bytes.len())?;
        Ok(bytes)
    }

    /// Maps real acquisition-owned edge samples without inventing a model revision.
    pub fn telemetry_batch(
        topology: TopologyBinding,
        samples: &[PointSample],
    ) -> Result<TelemetryBatch, CloudLinkCodecError> {
        TelemetryBatch::from_samples(topology, samples)
    }

    /// Embeds and re-verifies the existing closed Runtime Manifest v1 checksum.
    pub fn runtime_manifest_report(
        manifest_json: &[u8],
        observed_at: TimestampMs,
    ) -> Result<RuntimeManifestReport, CloudLinkCodecError> {
        if manifest_json.len() > MAX_CLOUDLINK_MESSAGE_BYTES {
            return Err(CloudLinkCodecError::MessageTooLarge {
                found: manifest_json.len(),
                maximum: MAX_CLOUDLINK_MESSAGE_BYTES,
            });
        }
        let manifest: Value = serde_json::from_slice(manifest_json)?;
        let value = RuntimeManifestReport {
            observed_at_ms: observed_at.get().to_string(),
            manifest,
        };
        value.validate()?;
        Ok(value)
    }

    /// Seals typed business content before it receives a stream position.
    pub fn prepare<T: Serialize>(
        message_kind: CloudLinkMessageKind,
        batch_id: impl Into<String>,
        payload: &T,
        created_at: TimestampMs,
        expires_at: Option<TimestampMs>,
    ) -> Result<CloudLinkEnqueue, CloudLinkCodecError> {
        let value = serde_json::to_value(payload)
            .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
        Self::prepare_value(message_kind, batch_id, value, created_at, expires_at)
    }

    /// Seals already-decoded JSON after exact kind-specific validation.
    pub fn prepare_value(
        message_kind: CloudLinkMessageKind,
        batch_id: impl Into<String>,
        payload: Value,
        created_at: TimestampMs,
        expires_at: Option<TimestampMs>,
    ) -> Result<CloudLinkEnqueue, CloudLinkCodecError> {
        validate_business_payload(message_kind, &payload)?;
        let batch_id = batch_id.into();
        identifier(&batch_id, "batch_id", 128)?;
        if expires_at.is_some_and(|expires| expires.get() < created_at.get()) {
            return Err(CloudLinkCodecError::InvalidField {
                field: "expires_at_ms",
                message: "must be after created_at_ms",
            });
        }
        let payload_bytes = serde_json_canonicalizer::to_vec(&payload)
            .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
        bound(payload_bytes.len())?;
        let digest = business_digest(message_kind, &payload)?;
        Ok(CloudLinkEnqueue::new(
            message_kind,
            batch_id,
            digest,
            payload_bytes,
            created_at,
            expires_at,
        ))
    }

    /// Wraps a retained record for one current session without changing identity.
    pub fn delivery_envelope(
        session: &SessionBinding,
        record: &CloudLinkRecord,
        sent_at: TimestampMs,
        trace: Option<&str>,
    ) -> Result<DeliveryEnvelope, CloudLinkCodecError> {
        if let Some(value) = trace {
            traceparent(value)?;
        }
        if record
            .expires_at()
            .is_some_and(|expires| expires.get() < sent_at.get())
        {
            return Err(CloudLinkCodecError::InvalidField {
                field: "expires_at_ms",
                message: "expired business content cannot be offered",
            });
        }
        let payload: Value = serde_json::from_slice(record.payload())?;
        validate_business_payload(record.message_kind(), &payload)?;
        if business_digest(record.message_kind(), &payload)? != record.digest() {
            return Err(CloudLinkCodecError::DigestMismatch);
        }
        let value = DeliveryEnvelope {
            schema: ENVELOPE_SCHEMA.to_string(),
            protocol: CLOUDLINK_PROTOCOL.to_string(),
            protocol_version: session.protocol_version().to_string(),
            message_kind: record.message_kind().as_str().to_string(),
            gateway_id: session.gateway_id().to_string(),
            session_id: session.session_id().to_string(),
            session_epoch: session.session_epoch().to_string(),
            credential_generation: session.credential_generation().to_string(),
            sent_at_ms: sent_at.get().to_string(),
            expires_at_ms: record.expires_at().map(|value| value.get().to_string()),
            delivery: DeliveryDescriptor {
                stream_id: record.identity().stream_id().to_string(),
                stream_epoch: record.identity().stream_epoch().to_string(),
                position: record.identity().position().to_string(),
                batch_id: record.batch_id().to_string(),
                digest: record.digest().to_string(),
            },
            traceparent: trace.map(str::to_string),
            payload,
        };
        value.validate()?;
        Ok(value)
    }
}

/// Every strictly decoded candidate message.
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateMessage {
    /// Cloud-issued one-time session challenge.
    SessionChallenge(SessionChallenge),
    /// Session negotiation request.
    SessionHello(SessionHello),
    /// Session negotiation response.
    SessionAccepted(SessionAccepted),
    /// Heartbeat or heartbeat ACK.
    Heartbeat(HeartbeatMessage),
    /// Manifest, point batch, or data-loss delivery envelope.
    Delivery(DeliveryEnvelope),
    /// Durable cloud application acknowledgement.
    DurableAck(DurableAckMessage),
    /// Server-authoritative replay request.
    ReplayRequest(ReplayRequest),
}

impl CandidateMessage {
    /// Validates a post-establishment response against the current session.
    pub fn validate_session(&self, session: &SessionBinding) -> Result<(), CloudLinkCodecError> {
        match self {
            Self::Heartbeat(value) => value.validate_session(session),
            Self::Delivery(value) => value.validate_session(session),
            Self::DurableAck(value) => value.validate_session(session),
            Self::ReplayRequest(value) => value.validate_session(session),
            Self::SessionChallenge(_) | Self::SessionHello(_) | Self::SessionAccepted(_) => {
                Err(CloudLinkCodecError::SessionMismatch)
            },
        }
    }
}

/// Heartbeat and heartbeat ACK share one closed state/cursor shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatMessage {
    schema: String,
    protocol: String,
    protocol_version: String,
    message_kind: String,
    gateway_id: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    observed_at_ms: String,
    cursors: Vec<crate::ResumeCursor>,
}

impl HeartbeatMessage {
    /// Creates an edge heartbeat or cloud heartbeat ACK.
    pub fn new(
        session: &SessionBinding,
        acknowledged: bool,
        observed_at: TimestampMs,
        cursors: Vec<crate::ResumeCursor>,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            schema: HEARTBEAT_SCHEMA.to_string(),
            protocol: CLOUDLINK_PROTOCOL.to_string(),
            protocol_version: session.protocol_version().to_string(),
            message_kind: if acknowledged {
                "heartbeat-ack".to_string()
            } else {
                "heartbeat".to_string()
            },
            gateway_id: session.gateway_id().to_string(),
            session_id: session.session_id().to_string(),
            session_epoch: session.session_epoch().to_string(),
            credential_generation: session.credential_generation().to_string(),
            observed_at_ms: observed_at.get().to_string(),
            cursors,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, HEARTBEAT_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL
            || !matches!(self.message_kind.as_str(), "heartbeat" | "heartbeat-ack")
        {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        validate_session_fields(
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )?;
        canonical_u64(&self.observed_at_ms, "observed_at_ms")?;
        if self.cursors.len() > 32 {
            return Err(CloudLinkCodecError::InvalidField {
                field: "cursors",
                message: "contains more than 32 stream cursors",
            });
        }
        for cursor in &self.cursors {
            cursor.validate()?;
        }
        Ok(())
    }

    /// Validates that this request belongs to the current verified session.
    pub fn validate_session(&self, session: &SessionBinding) -> Result<(), CloudLinkCodecError> {
        self.validate()?;
        session_match(
            session,
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )
    }
}

/// Stream identity and digest carried by a delivery envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryDescriptor {
    stream_id: String,
    stream_epoch: String,
    position: String,
    batch_id: String,
    digest: String,
}

impl DeliveryDescriptor {
    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        identifier(&self.stream_id, "delivery.stream_id", 128)?;
        positive_u64(&self.stream_epoch, "delivery.stream_epoch")?;
        positive_u64(&self.position, "delivery.position")?;
        identifier(&self.batch_id, "delivery.batch_id", 128)?;
        digest(&self.digest, "delivery.digest")
    }

    /// Returns the stream ID.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the parsed stream epoch.
    #[must_use]
    pub fn stream_epoch(&self) -> u64 {
        self.stream_epoch.parse().unwrap_or_default()
    }

    /// Returns the parsed position.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position.parse().unwrap_or_default()
    }

    /// Returns the stable batch ID.
    #[must_use]
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// Returns the canonical business digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Stable envelope for application-durable business facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEnvelope {
    schema: String,
    protocol: String,
    protocol_version: String,
    message_kind: String,
    gateway_id: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    sent_at_ms: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<String>,
    delivery: DeliveryDescriptor,
    #[serde(skip_serializing_if = "Option::is_none")]
    traceparent: Option<String>,
    payload: Value,
}

impl DeliveryEnvelope {
    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, ENVELOPE_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.protocol.clone(),
            });
        }
        validate_session_fields(
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )?;
        let sent_at = canonical_u64(&self.sent_at_ms, "sent_at_ms")?;
        if let Some(expires) = &self.expires_at_ms
            && canonical_u64(expires, "expires_at_ms")? < sent_at
        {
            return Err(CloudLinkCodecError::InvalidField {
                field: "expires_at_ms",
                message: "must be after sent_at_ms",
            });
        }
        self.delivery.validate()?;
        if let Some(value) = &self.traceparent {
            traceparent(value)?;
        }
        let kind = message_kind(&self.message_kind)?;
        validate_business_payload(kind, &self.payload)?;
        if business_digest(kind, &self.payload)? != self.delivery.digest {
            return Err(CloudLinkCodecError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_session(&self, session: &SessionBinding) -> Result<(), CloudLinkCodecError> {
        self.validate()?;
        session_match(
            session,
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )
    }

    /// Returns durable delivery identity.
    #[must_use]
    pub const fn delivery(&self) -> &DeliveryDescriptor {
        &self.delivery
    }

    /// Returns the typed business kind.
    pub fn message_kind(&self) -> Result<CloudLinkMessageKind, CloudLinkCodecError> {
        message_kind(&self.message_kind)
    }

    /// Returns the strict business payload.
    #[must_use]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }
}

/// Existing Runtime Manifest v1 embedded without reinterpretation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeManifestWire {
    schema_version: u32,
    composition: String,
    aether_version: String,
    target_triple: String,
    target_os: String,
    services: Vec<String>,
    cargo_features: Vec<String>,
    capabilities: Vec<String>,
    protocols: Vec<String>,
    checksum: RuntimeManifestChecksum,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeManifestChecksum {
    algorithm: String,
    digest: String,
}

/// Runtime Manifest business payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeManifestReport {
    observed_at_ms: String,
    manifest: Value,
}

impl RuntimeManifestReport {
    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        canonical_u64(&self.observed_at_ms, "payload.observed_at_ms")?;
        let wire: RuntimeManifestWire = serde_json::from_value(self.manifest.clone())?;
        if semver::Version::parse(&wire.aether_version)
            .ok()
            .is_none_or(|version| version.to_string() != wire.aether_version)
        {
            return Err(CloudLinkCodecError::InvalidField {
                field: "payload.manifest.aether_version",
                message: "must be strict SemVer 2.0.0",
            });
        }
        if wire.schema_version != 1
            || wire.checksum.algorithm != "sha256"
            || wire.checksum.digest.len() != 64
            || !wire
                .checksum
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(CloudLinkCodecError::RuntimeManifestChecksum);
        }
        let mut unsigned = self.manifest.clone();
        unsigned
            .as_object_mut()
            .ok_or(CloudLinkCodecError::RuntimeManifestChecksum)?
            .remove("checksum");
        let canonical = serde_json_canonicalizer::to_vec(&unsigned)
            .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
        let expected = format!("{:x}", Sha256::digest(canonical));
        if expected != wire.checksum.digest {
            return Err(CloudLinkCodecError::RuntimeManifestChecksum);
        }
        Ok(())
    }

    /// Returns the exact verified manifest value.
    #[must_use]
    pub const fn manifest(&self) -> &Value {
        &self.manifest
    }
}

/// Explicit unavailable retained range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataLossPayload {
    stream_id: String,
    stream_epoch: String,
    first_lost_position: String,
    last_lost_position: String,
    earliest_retained_position: String,
    reason: String,
    recorded_at_ms: String,
}

impl DataLossPayload {
    /// Creates business content from durable spool evidence.
    pub fn from_evidence(evidence: &aether_ports::CloudLinkDataLossEvidence) -> Self {
        Self {
            stream_id: evidence.stream_id().to_string(),
            stream_epoch: evidence.stream_epoch().to_string(),
            first_lost_position: evidence.first_lost_position().to_string(),
            last_lost_position: evidence.last_lost_position().to_string(),
            earliest_retained_position: evidence.earliest_retained_position().to_string(),
            reason: evidence.reason().to_string(),
            recorded_at_ms: evidence.recorded_at().get().to_string(),
        }
    }

    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        identifier(&self.stream_id, "payload.stream_id", 128)?;
        positive_u64(&self.stream_epoch, "payload.stream_epoch")?;
        let first = positive_u64(&self.first_lost_position, "payload.first_lost_position")?;
        let last = positive_u64(&self.last_lost_position, "payload.last_lost_position")?;
        let earliest = positive_u64(
            &self.earliest_retained_position,
            "payload.earliest_retained_position",
        )?;
        if first > last || earliest <= last {
            return Err(CloudLinkCodecError::InvalidField {
                field: "payload.data_loss_range",
                message: "must be ordered and precede earliest retained position",
            });
        }
        identifier(&self.reason, "payload.reason", 64)?;
        canonical_u64(&self.recorded_at_ms, "payload.recorded_at_ms")?;
        Ok(())
    }
}

/// Cloud application ACK. MQTT PUBACK never constructs this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableAckMessage {
    schema: String,
    protocol: String,
    protocol_version: String,
    message_kind: String,
    gateway_id: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    stream_id: String,
    stream_epoch: String,
    acknowledged_position: String,
    batch_id: String,
    digest: String,
    receipt_id: String,
    acknowledged_at_ms: String,
}

impl DurableAckMessage {
    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, DURABLE_ACK_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL || self.message_kind != "durable-ack" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        validate_session_fields(
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )?;
        identifier(&self.stream_id, "stream_id", 128)?;
        positive_u64(&self.stream_epoch, "stream_epoch")?;
        positive_u64(&self.acknowledged_position, "acknowledged_position")?;
        identifier(&self.batch_id, "batch_id", 128)?;
        digest(&self.digest, "digest")?;
        identifier(&self.receipt_id, "receipt_id", 128)?;
        canonical_u64(&self.acknowledged_at_ms, "acknowledged_at_ms")?;
        Ok(())
    }

    fn validate_session(&self, session: &SessionBinding) -> Result<(), CloudLinkCodecError> {
        self.validate()?;
        session_match(
            session,
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )
    }

    /// Converts a verified wire receipt into the dedicated spool capability value.
    pub fn to_spool_ack(
        &self,
        session: &SessionBinding,
    ) -> Result<CloudLinkDurableAck, CloudLinkCodecError> {
        self.validate_session(session)?;
        Ok(CloudLinkDurableAck::new(
            CloudLinkSessionBinding::new(self.session_id.clone(), session.session_epoch()),
            self.stream_id.clone(),
            positive_u64(&self.stream_epoch, "stream_epoch")?,
            positive_u64(&self.acknowledged_position, "acknowledged_position")?,
            self.batch_id.clone(),
            self.digest.clone(),
            self.receipt_id.clone(),
        ))
    }
}

/// Server-authoritative request to resume from one first-needed position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {
    schema: String,
    protocol: String,
    protocol_version: String,
    message_kind: String,
    gateway_id: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    stream_id: String,
    stream_epoch: String,
    from_position: String,
    requested_at_ms: String,
}

impl ReplayRequest {
    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        schema(&self.schema, REPLAY_REQUEST_SCHEMA)?;
        if self.protocol != CLOUDLINK_PROTOCOL || self.message_kind != "replay-request" {
            return Err(CloudLinkCodecError::UnsupportedMessage {
                found: self.message_kind.clone(),
            });
        }
        validate_session_fields(
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )?;
        identifier(&self.stream_id, "stream_id", 128)?;
        positive_u64(&self.stream_epoch, "stream_epoch")?;
        positive_u64(&self.from_position, "from_position")?;
        canonical_u64(&self.requested_at_ms, "requested_at_ms")?;
        Ok(())
    }

    /// Validates that this request belongs to the current verified session.
    pub fn validate_session(&self, session: &SessionBinding) -> Result<(), CloudLinkCodecError> {
        self.validate()?;
        session_match(
            session,
            &self.protocol_version,
            &self.gateway_id,
            &self.session_id,
            &self.session_epoch,
            &self.credential_generation,
        )
    }

    /// Returns the server-authoritative first position needed.
    #[must_use]
    pub fn from_position(&self) -> u64 {
        self.from_position.parse().unwrap_or_default()
    }

    /// Returns the logical stream ID.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the requested stream epoch.
    #[must_use]
    pub fn stream_epoch(&self) -> u64 {
        self.stream_epoch.parse().unwrap_or_default()
    }
}

fn validate_business_payload(
    kind: CloudLinkMessageKind,
    payload: &Value,
) -> Result<(), CloudLinkCodecError> {
    match kind {
        CloudLinkMessageKind::RuntimeManifestReport => {
            serde_json::from_value::<RuntimeManifestReport>(payload.clone())?.validate()
        },
        CloudLinkMessageKind::TelemetryBatch => {
            serde_json::from_value::<TelemetryBatch>(payload.clone())?.validate()
        },
        CloudLinkMessageKind::DataLoss => {
            serde_json::from_value::<DataLossPayload>(payload.clone())?.validate()
        },
    }
}

fn business_digest(
    kind: CloudLinkMessageKind,
    payload: &Value,
) -> Result<String, CloudLinkCodecError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        protocol_version: &'static str,
        message_kind: &'static str,
        payload: &'a Value,
    }
    let canonical = serde_json_canonicalizer::to_vec(&DigestInput {
        protocol_version: CLOUDLINK_PROTOCOL_VERSION,
        message_kind: kind.as_str(),
        payload,
    })
    .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn message_kind(value: &str) -> Result<CloudLinkMessageKind, CloudLinkCodecError> {
    match value {
        "runtime-manifest-report" => Ok(CloudLinkMessageKind::RuntimeManifestReport),
        "telemetry-batch" => Ok(CloudLinkMessageKind::TelemetryBatch),
        "data-loss" => Ok(CloudLinkMessageKind::DataLoss),
        other => Err(CloudLinkCodecError::UnsupportedMessage {
            found: other.to_string(),
        }),
    }
}

fn validate_session_fields(
    version: &str,
    gateway_id: &str,
    session_id: &str,
    session_epoch: &str,
    credential_generation: &str,
) -> Result<(), CloudLinkCodecError> {
    protocol_version(version)?;
    uuid(gateway_id, "gateway_id")?;
    uuid(session_id, "session_id")?;
    positive_u64(session_epoch, "session_epoch")?;
    positive_u64(credential_generation, "credential_generation")?;
    Ok(())
}

fn session_match(
    session: &SessionBinding,
    version: &str,
    gateway_id: &str,
    session_id: &str,
    session_epoch: &str,
    credential_generation: &str,
) -> Result<(), CloudLinkCodecError> {
    let matches = version == session.protocol_version()
        && gateway_id == session.gateway_id()
        && session_id == session.session_id()
        && positive_u64(session_epoch, "session_epoch")? == session.session_epoch()
        && positive_u64(credential_generation, "credential_generation")?
            == session.credential_generation();
    if matches {
        Ok(())
    } else {
        Err(CloudLinkCodecError::SessionMismatch)
    }
}

fn bound(found: usize) -> Result<(), CloudLinkCodecError> {
    if found <= MAX_CLOUDLINK_MESSAGE_BYTES {
        Ok(())
    } else {
        Err(CloudLinkCodecError::MessageTooLarge {
            found,
            maximum: MAX_CLOUDLINK_MESSAGE_BYTES,
        })
    }
}
