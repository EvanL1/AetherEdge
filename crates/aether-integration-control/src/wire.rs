//! Closed AetherContracts `integration-control` v1alpha1 wire values.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ControlResult;
use crate::validation::{canonical_u64, digest, exact, failure_code, identifier, signature, uuid};
use crate::{
    ACTION_INTENT_SCHEMA, ACTION_OFFER_SCHEMA, ACTION_RECEIPT_SCHEMA, CAPABILITY_ID,
    CLOUDLINK_ENVELOPE_SCHEMA, CLOUDLINK_PROTOCOL, CLOUDLINK_PROTOCOL_VERSION,
    INTEGRATION_CONTROL_EXTENSION, MESSAGE_KIND_OFFER, MESSAGE_KIND_RECEIPT, PERMISSION,
};

/// The one stable target shape permitted by the alpha.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTarget {
    integration_id: String,
    snapshot_generation: String,
    entity_id: String,
    point_key: String,
}

impl ActionTarget {
    /// Creates an exact generation-fenced semantic `is_on` target.
    pub fn new(
        integration_id: impl Into<String>,
        snapshot_generation: u64,
        entity_id: impl Into<String>,
    ) -> ControlResult<Self> {
        let value = Self {
            integration_id: integration_id.into(),
            snapshot_generation: snapshot_generation.to_string(),
            entity_id: entity_id.into(),
            point_key: "is_on".to_string(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> ControlResult<()> {
        identifier(&self.integration_id)?;
        canonical_u64(&self.snapshot_generation, false)?;
        identifier(&self.entity_id)?;
        exact(
            &self.point_key,
            "is_on",
            "only the is_on point can be controlled",
        )
    }

    /// Returns the edge-local Integration identity.
    #[must_use]
    pub fn integration_id(&self) -> &str {
        &self.integration_id
    }

    /// Returns the exact topology generation fence.
    #[must_use]
    pub fn snapshot_generation(&self) -> u64 {
        self.snapshot_generation.parse().unwrap_or_default()
    }

    /// Returns the provider-stable entity registry identity.
    #[must_use]
    pub fn entity_id(&self) -> &str {
        &self.entity_id
    }

    /// Returns the only controllable semantic point, `is_on`.
    #[must_use]
    pub fn point_key(&self) -> &str {
        &self.point_key
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PowerArguments {
    value: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Governance {
    execution: String,
    default_authorization: String,
    permission: String,
    risk: String,
    confirmation: String,
    idempotency: String,
    expiry: String,
    audit: String,
    edge_final_decision: bool,
}

impl Governance {
    fn validate(&self) -> ControlResult<()> {
        exact(
            &self.execution,
            "governed-job",
            "execution must be a governed job",
        )?;
        exact(
            &self.default_authorization,
            "deny",
            "authorization must default to deny",
        )?;
        exact(&self.permission, PERMISSION, "permission is not supported")?;
        exact(&self.risk, "high", "risk must be high")?;
        exact(
            &self.confirmation,
            "required",
            "confirmation must be required",
        )?;
        exact(
            &self.idempotency,
            "required",
            "idempotency must be required",
        )?;
        exact(&self.expiry, "required", "expiry must be required")?;
        exact(&self.audit, "required", "audit must be required")?;
        if !self.edge_final_decision {
            return Err(crate::validation::invalid(
                "the edge must retain the final decision",
            ));
        }
        Ok(())
    }
}

/// Cloud-side authorization evidence carried for independent edge evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudAuthorization {
    policy_decision_id: String,
    subject_id: String,
    permission: String,
    authorized_at_ms: String,
}

impl CloudAuthorization {
    fn validate(&self) -> ControlResult<()> {
        identifier(&self.policy_decision_id)?;
        identifier(&self.subject_id)?;
        exact(&self.permission, PERMISSION, "permission is not supported")?;
        canonical_u64(&self.authorized_at_ms, false)?;
        Ok(())
    }

    /// Returns the cloud policy evidence identity.
    #[must_use]
    pub fn policy_decision_id(&self) -> &str {
        &self.policy_decision_id
    }

    /// Returns the requesting subject.
    #[must_use]
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    /// Returns when the cloud policy decision was made.
    #[must_use]
    pub fn authorized_at_ms(&self) -> u64 {
        self.authorized_at_ms.parse().unwrap_or_default()
    }
}

/// Explicit user confirmation evidence carried for independent edge evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudConfirmation {
    confirmation_id: String,
    subject_id: String,
    confirmed_at_ms: String,
}

impl CloudConfirmation {
    fn validate(&self) -> ControlResult<()> {
        uuid(&self.confirmation_id)?;
        identifier(&self.subject_id)?;
        canonical_u64(&self.confirmed_at_ms, false)?;
        Ok(())
    }

    /// Returns the unique confirmation identity.
    #[must_use]
    pub fn confirmation_id(&self) -> &str {
        &self.confirmation_id
    }

    /// Returns the confirming subject.
    #[must_use]
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    /// Returns when the explicit confirmation was recorded.
    #[must_use]
    pub fn confirmed_at_ms(&self) -> u64 {
        self.confirmed_at_ms.parse().unwrap_or_default()
    }
}

/// Complete closed semantic intent. It contains no provider operation or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionIntent {
    schema: String,
    capability_id: String,
    target: ActionTarget,
    arguments: PowerArguments,
    governance: Governance,
    authorization: CloudAuthorization,
    confirmation: CloudConfirmation,
}

impl ActionIntent {
    pub(crate) fn validate(&self) -> ControlResult<()> {
        exact(
            &self.schema,
            ACTION_INTENT_SCHEMA,
            "intent schema is not supported",
        )?;
        exact(
            &self.capability_id,
            CAPABILITY_ID,
            "capability is not supported",
        )?;
        self.target.validate()?;
        self.governance.validate()?;
        self.authorization.validate()?;
        self.confirmation.validate()
    }

    /// Returns the only supported capability identity.
    #[must_use]
    pub fn capability_id(&self) -> &str {
        &self.capability_id
    }

    /// Returns the exact generation-fenced target.
    #[must_use]
    pub const fn target(&self) -> &ActionTarget {
        &self.target
    }

    /// Returns the desired semantic power state.
    #[must_use]
    pub const fn value(&self) -> bool {
        self.arguments.value
    }

    /// Returns cloud authorization evidence.
    #[must_use]
    pub const fn authorization(&self) -> &CloudAuthorization {
        &self.authorization
    }

    /// Returns explicit confirmation evidence.
    #[must_use]
    pub const fn confirmation(&self) -> &CloudConfirmation {
        &self.confirmation
    }
}

/// Structurally validated Ed25519 authentication material.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageAuthentication {
    key_id: String,
    algorithm: String,
    signature: String,
}

impl MessageAuthentication {
    /// Creates validated gateway or cloud signature material.
    pub fn new(
        key_id: impl Into<String>,
        signature_value: impl Into<String>,
    ) -> ControlResult<Self> {
        let value = Self {
            key_id: key_id.into(),
            algorithm: "Ed25519".to_string(),
            signature: signature_value.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> ControlResult<()> {
        identifier(&self.key_id)?;
        exact(
            &self.algorithm,
            "Ed25519",
            "signature algorithm is not supported",
        )?;
        signature(&self.signature)
    }

    /// Returns the public key identity.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Returns the unpadded Base64url signature for a verifier or encoder.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }
}

impl fmt::Debug for MessageAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MessageAuthentication([REDACTED])")
    }
}

/// Signed Cloud-to-edge governed action offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOffer {
    schema: String,
    protocol: String,
    protocol_version: String,
    extension: String,
    message_kind: String,
    gateway_id: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    job_id: String,
    issued_at_ms: String,
    expires_at_ms: String,
    intent_digest: String,
    intent: ActionIntent,
    cloud_authentication: MessageAuthentication,
}

#[derive(Serialize)]
struct OfferSigningProjection<'a> {
    schema: &'a str,
    protocol: &'a str,
    protocol_version: &'a str,
    extension: &'a str,
    message_kind: &'a str,
    gateway_id: &'a str,
    session_id: &'a str,
    session_epoch: &'a str,
    credential_generation: &'a str,
    job_id: &'a str,
    issued_at_ms: &'a str,
    expires_at_ms: &'a str,
    intent_digest: &'a str,
    intent: &'a ActionIntent,
}

impl ActionOffer {
    pub(crate) fn validate(&self) -> ControlResult<()> {
        exact(
            &self.schema,
            ACTION_OFFER_SCHEMA,
            "offer schema is not supported",
        )?;
        exact(
            &self.protocol,
            CLOUDLINK_PROTOCOL,
            "protocol is not supported",
        )?;
        exact(
            &self.protocol_version,
            CLOUDLINK_PROTOCOL_VERSION,
            "protocol version is not supported",
        )?;
        exact(
            &self.extension,
            INTEGRATION_CONTROL_EXTENSION,
            "extension is not supported",
        )?;
        exact(
            &self.message_kind,
            MESSAGE_KIND_OFFER,
            "message kind is not supported",
        )?;
        uuid(&self.gateway_id)?;
        uuid(&self.session_id)?;
        canonical_u64(&self.session_epoch, true)?;
        canonical_u64(&self.credential_generation, true)?;
        uuid(&self.job_id)?;
        let issued_at = canonical_u64(&self.issued_at_ms, false)?;
        let expires_at = canonical_u64(&self.expires_at_ms, false)?;
        if expires_at <= issued_at {
            return Err(crate::validation::invalid(
                "offer expiry must be after issue time",
            ));
        }
        digest(&self.intent_digest)?;
        self.intent.validate()?;
        if self.intent.authorization.authorized_at_ms() > issued_at
            || self.intent.confirmation.confirmed_at_ms() > issued_at
            || self.intent.confirmation.confirmed_at_ms()
                < self.intent.authorization.authorized_at_ms()
            || self.intent.confirmation.subject_id() != self.intent.authorization.subject_id()
        {
            return Err(crate::validation::invalid(
                "authorization and confirmation evidence is inconsistent",
            ));
        }
        self.cloud_authentication.validate()
    }

    pub(crate) fn signing_projection(&self) -> impl Serialize + '_ {
        OfferSigningProjection {
            schema: &self.schema,
            protocol: &self.protocol,
            protocol_version: &self.protocol_version,
            extension: &self.extension,
            message_kind: &self.message_kind,
            gateway_id: &self.gateway_id,
            session_id: &self.session_id,
            session_epoch: &self.session_epoch,
            credential_generation: &self.credential_generation,
            job_id: &self.job_id,
            issued_at_ms: &self.issued_at_ms,
            expires_at_ms: &self.expires_at_ms,
            intent_digest: &self.intent_digest,
            intent: &self.intent,
        }
    }

    /// Returns RFC 8785 bytes for exactly the frozen signed field projection.
    pub fn signing_bytes(&self) -> ControlResult<Vec<u8>> {
        self.validate()?;
        serde_json_canonicalizer::to_vec(&self.signing_projection())
            .map_err(|_source| crate::validation::invalid("offer signing projection failed"))
    }

    /// Returns the frozen extension activation token.
    #[must_use]
    pub fn extension(&self) -> &str {
        &self.extension
    }

    /// Returns the edge gateway scope.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    /// Returns the authenticated session identity.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the authenticated session epoch.
    #[must_use]
    pub fn session_epoch(&self) -> u64 {
        self.session_epoch.parse().unwrap_or_default()
    }

    /// Returns the session credential generation.
    #[must_use]
    pub fn credential_generation(&self) -> u64 {
        self.credential_generation.parse().unwrap_or_default()
    }

    /// Returns the stable governed job identity.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Returns the cloud issue time.
    #[must_use]
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms.parse().unwrap_or_default()
    }

    /// Returns the hard execution deadline.
    #[must_use]
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms.parse().unwrap_or_default()
    }

    /// Returns the stable canonical intent digest.
    #[must_use]
    pub fn intent_digest(&self) -> &str {
        &self.intent_digest
    }

    /// Returns the closed semantic intent.
    #[must_use]
    pub const fn intent(&self) -> &ActionIntent {
        &self.intent
    }

    /// Returns cloud signature metadata.
    #[must_use]
    pub const fn cloud_authentication(&self) -> &MessageAuthentication {
        &self.cloud_authentication
    }
}

/// Permitted terminal and intermediate receipt stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionReceiptStage {
    /// The edge accepted the governed job but has not called a provider.
    EdgeAccepted,
    /// The edge rejected the job under local authority.
    EdgeRejected,
    /// The provider accepted the request, without proving a physical outcome.
    ProviderAccepted,
    /// The provider synchronously rejected the request.
    ProviderRejected,
    /// Execution may have crossed the provider boundary, so retry is unsafe.
    Unknown,
}

/// Decision represented by one receipt stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionDecision {
    /// Accepted by the stated boundary.
    Accepted,
    /// Rejected by the stated boundary.
    Rejected,
    /// No safe terminal conclusion exists.
    Unknown,
}

/// Physical-device evidence. Alpha receipts can only state `unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PhysicalOutcome {
    /// Provider acceptance is not physical completion.
    Unknown,
}

/// Audit persistence represented by a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditStatus {
    /// Required audit evidence was committed.
    Complete,
    /// The provider boundary may have been crossed but final audit failed.
    Incomplete,
}

/// Receipt audit reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAudit {
    audit_record_id: String,
    status: AuditStatus,
}

impl ReceiptAudit {
    pub(crate) fn new(audit_record_id: String, status: AuditStatus) -> ControlResult<Self> {
        identifier(&audit_record_id)?;
        Ok(Self {
            audit_record_id,
            status,
        })
    }

    fn validate(&self) -> ControlResult<()> {
        identifier(&self.audit_record_id)
    }

    /// Returns the durable local audit identity.
    #[must_use]
    pub fn audit_record_id(&self) -> &str {
        &self.audit_record_id
    }

    /// Returns whether final audit persistence completed.
    #[must_use]
    pub const fn status(&self) -> AuditStatus {
        self.status
    }
}

/// Closed business payload persisted and replayed by the edge receipt ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReceiptPayload {
    schema: String,
    job_id: String,
    receipt_id: String,
    receipt_sequence: String,
    capability_id: String,
    target: ActionTarget,
    intent_digest: String,
    stage: ActionReceiptStage,
    decision: ActionDecision,
    physical_outcome: PhysicalOutcome,
    observed_at_ms: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<String>,
    audit: ReceiptAudit,
}

impl ActionReceiptPayload {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn terminal(
        job_id: String,
        receipt_id: String,
        target: ActionTarget,
        intent_digest: String,
        stage: ActionReceiptStage,
        decision: ActionDecision,
        observed_at_ms: u64,
        evidence_digest: Option<String>,
        failure_code_value: Option<String>,
        audit: ReceiptAudit,
    ) -> ControlResult<Self> {
        let value = Self {
            schema: ACTION_RECEIPT_SCHEMA.to_string(),
            job_id,
            receipt_id,
            receipt_sequence: "1".to_string(),
            capability_id: CAPABILITY_ID.to_string(),
            target,
            intent_digest,
            stage,
            decision,
            physical_outcome: PhysicalOutcome::Unknown,
            observed_at_ms: observed_at_ms.to_string(),
            evidence_digest,
            failure_code: failure_code_value,
            audit,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> ControlResult<()> {
        exact(
            &self.schema,
            ACTION_RECEIPT_SCHEMA,
            "receipt schema is not supported",
        )?;
        uuid(&self.job_id)?;
        uuid(&self.receipt_id)?;
        canonical_u64(&self.receipt_sequence, true)?;
        exact(
            &self.capability_id,
            CAPABILITY_ID,
            "receipt capability is not supported",
        )?;
        self.target.validate()?;
        digest(&self.intent_digest)?;
        canonical_u64(&self.observed_at_ms, false)?;
        if let Some(value) = &self.evidence_digest {
            digest(value)?;
        }
        if let Some(value) = &self.failure_code {
            failure_code(value)?;
        }
        self.audit.validate()?;

        match self.stage {
            ActionReceiptStage::EdgeAccepted => {
                if self.decision != ActionDecision::Accepted
                    || self.failure_code.is_some()
                    || self.evidence_digest.is_some()
                {
                    return Err(crate::validation::invalid(
                        "edge-accepted receipt fields are inconsistent",
                    ));
                }
            },
            ActionReceiptStage::ProviderAccepted => {
                if self.decision != ActionDecision::Accepted
                    || self.failure_code.is_some()
                    || self.evidence_digest.is_none()
                {
                    return Err(crate::validation::invalid(
                        "provider-accepted receipt fields are inconsistent",
                    ));
                }
            },
            ActionReceiptStage::EdgeRejected => {
                if self.decision != ActionDecision::Rejected || self.failure_code.is_none() {
                    return Err(crate::validation::invalid(
                        "edge-rejected receipt fields are inconsistent",
                    ));
                }
            },
            ActionReceiptStage::ProviderRejected => {
                if self.decision != ActionDecision::Rejected
                    || self.failure_code.is_none()
                    || self.evidence_digest.is_none()
                {
                    return Err(crate::validation::invalid(
                        "provider-rejected receipt fields are inconsistent",
                    ));
                }
            },
            ActionReceiptStage::Unknown => {
                if self.decision != ActionDecision::Unknown || self.failure_code.is_none() {
                    return Err(crate::validation::invalid(
                        "unknown receipt fields are inconsistent",
                    ));
                }
            },
        }
        Ok(())
    }

    /// Revalidates a deserialized or persisted closed receipt.
    pub fn validate_contract(&self) -> ControlResult<()> {
        self.validate()
    }

    /// Returns the governed job identity.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Returns the stable receipt identity.
    #[must_use]
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    /// Returns the stable offer digest.
    #[must_use]
    pub fn intent_digest(&self) -> &str {
        &self.intent_digest
    }

    /// Returns the exact controlled target.
    #[must_use]
    pub const fn target(&self) -> &ActionTarget {
        &self.target
    }

    /// Returns the boundary stage represented by the receipt.
    #[must_use]
    pub const fn stage(&self) -> ActionReceiptStage {
        self.stage
    }

    /// Returns the decision represented by the receipt.
    #[must_use]
    pub const fn decision(&self) -> ActionDecision {
        self.decision
    }

    /// Returns the deliberately unproven physical outcome.
    #[must_use]
    pub const fn physical_outcome(&self) -> PhysicalOutcome {
        self.physical_outcome
    }

    /// Returns the immutable time at which the edge observed the terminal outcome.
    #[must_use]
    pub fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms.parse().unwrap_or_default()
    }

    /// Always returns false for the alpha contract.
    #[must_use]
    pub const fn physical_completed(&self) -> bool {
        false
    }

    /// Always returns false because provider acceptance is not job success.
    #[must_use]
    pub const fn job_succeeded(&self) -> bool {
        false
    }

    /// Returns the safe terminal failure code, when present.
    #[must_use]
    pub fn failure_code(&self) -> Option<&str> {
        self.failure_code.as_deref()
    }
}

/// Existing CloudLink durable delivery descriptor used for receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptDelivery {
    stream_id: String,
    stream_epoch: String,
    position: String,
    batch_id: String,
    digest: String,
}

impl ReceiptDelivery {
    /// Creates a validated durable receipt delivery identity.
    pub fn new(
        stream_epoch: u64,
        position: u64,
        batch_id: impl Into<String>,
        digest_value: impl Into<String>,
    ) -> ControlResult<Self> {
        let value = Self {
            stream_id: "integration-control-receipts".to_string(),
            stream_epoch: stream_epoch.to_string(),
            position: position.to_string(),
            batch_id: batch_id.into(),
            digest: digest_value.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> ControlResult<()> {
        exact(
            &self.stream_id,
            "integration-control-receipts",
            "receipt stream is not supported",
        )?;
        canonical_u64(&self.stream_epoch, true)?;
        canonical_u64(&self.position, true)?;
        identifier(&self.batch_id)?;
        digest(&self.digest)
    }

    /// Returns the fixed durable receipt stream identity.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the durable stream epoch.
    #[must_use]
    pub fn stream_epoch(&self) -> u64 {
        self.stream_epoch.parse().unwrap_or_default()
    }

    /// Returns the durable stream position.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position.parse().unwrap_or_default()
    }

    /// Returns the stable delivery batch identity.
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

/// Authenticated existing CloudLink uplink envelope carrying one receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReceiptEnvelope {
    schema: String,
    protocol: String,
    protocol_version: String,
    message_kind: String,
    gateway_id: String,
    session_id: String,
    session_epoch: String,
    credential_generation: String,
    sent_at_ms: String,
    delivery: ReceiptDelivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_authentication: Option<MessageAuthentication>,
    payload: ActionReceiptPayload,
}

impl ActionReceiptEnvelope {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        gateway_id: String,
        session_id: String,
        session_epoch: u64,
        credential_generation: u64,
        sent_at_ms: u64,
        delivery: ReceiptDelivery,
        message_authentication: Option<MessageAuthentication>,
        payload: ActionReceiptPayload,
    ) -> ControlResult<Self> {
        let value = Self {
            schema: CLOUDLINK_ENVELOPE_SCHEMA.to_string(),
            protocol: CLOUDLINK_PROTOCOL.to_string(),
            protocol_version: CLOUDLINK_PROTOCOL_VERSION.to_string(),
            message_kind: MESSAGE_KIND_RECEIPT.to_string(),
            gateway_id,
            session_id,
            session_epoch: session_epoch.to_string(),
            credential_generation: credential_generation.to_string(),
            sent_at_ms: sent_at_ms.to_string(),
            delivery,
            message_authentication,
            payload,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> ControlResult<()> {
        exact(
            &self.schema,
            CLOUDLINK_ENVELOPE_SCHEMA,
            "envelope schema is not supported",
        )?;
        exact(
            &self.protocol,
            CLOUDLINK_PROTOCOL,
            "protocol is not supported",
        )?;
        exact(
            &self.protocol_version,
            CLOUDLINK_PROTOCOL_VERSION,
            "protocol version is not supported",
        )?;
        exact(
            &self.message_kind,
            MESSAGE_KIND_RECEIPT,
            "message kind is not supported",
        )?;
        uuid(&self.gateway_id)?;
        uuid(&self.session_id)?;
        canonical_u64(&self.session_epoch, true)?;
        canonical_u64(&self.credential_generation, true)?;
        canonical_u64(&self.sent_at_ms, false)?;
        self.delivery.validate()?;
        if let Some(authentication) = &self.message_authentication {
            authentication.validate()?;
        }
        self.payload.validate()
    }

    /// Returns durable delivery identity and business digest.
    #[must_use]
    pub const fn delivery(&self) -> &ReceiptDelivery {
        &self.delivery
    }

    /// Returns the strict Integration-control receipt payload.
    #[must_use]
    pub const fn payload(&self) -> &ActionReceiptPayload {
        &self.payload
    }

    /// Returns payload authentication for a Gateway-signed CloudLink session.
    #[must_use]
    pub const fn message_authentication(&self) -> Option<&MessageAuthentication> {
        self.message_authentication.as_ref()
    }
}
