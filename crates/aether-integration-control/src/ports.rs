//! Injectable trust, policy, provider, audit, and persistence boundaries.

use std::fmt;

use aether_ports::{CloudLinkDurableAck, DurableAckOutcome};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ControlResult;
use crate::validation::{identifier, uuid};
use crate::wire::{ActionOffer, ActionReceiptPayload, ActionTarget};
use crate::{IntegrationControlCodec, ReceiptDelivery};

/// Exact authenticated CloudLink session against which offers are checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSession {
    gateway_id: String,
    session_id: String,
    session_epoch: u64,
    credential_generation: u64,
}

impl ControlSession {
    /// Creates a validated established session fence.
    pub fn new(
        gateway_id: impl Into<String>,
        session_id: impl Into<String>,
        session_epoch: u64,
        credential_generation: u64,
    ) -> ControlResult<Self> {
        let value = Self {
            gateway_id: gateway_id.into(),
            session_id: session_id.into(),
            session_epoch,
            credential_generation,
        };
        uuid(&value.gateway_id)?;
        uuid(&value.session_id)?;
        if value.session_epoch == 0 || value.credential_generation == 0 {
            return Err(crate::validation::invalid(
                "session epoch and credential generation must be positive",
            ));
        }
        Ok(value)
    }

    /// Returns the current gateway identity.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    /// Returns the current session identity.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the current session epoch.
    #[must_use]
    pub const fn session_epoch(&self) -> u64 {
        self.session_epoch
    }

    /// Returns the current credential generation.
    #[must_use]
    pub const fn credential_generation(&self) -> u64 {
        self.credential_generation
    }
}

/// Stable local failure codes allowed in receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFailureCode {
    /// The target is absent from the current local projection.
    TargetNotFound,
    /// The requested generation is not the exact current topology generation.
    TopologyGenerationMismatch,
    /// The entity kind is not one of light, switch, or fan.
    EntityKindDenied,
    /// The exact entity does not expose a boolean `is_on` point.
    PointDenied,
    /// The target has not been commissioned for local control.
    NotCommissioned,
    /// The Integration is not delegated to this local provider.
    DelegationDenied,
    /// The final edge policy denied the permission.
    PolicyDenied,
    /// Local confirmation evidence did not match.
    ConfirmationInvalid,
    /// The target resolver could not safely read current topology.
    TargetUnavailable,
    /// The provider synchronously rejected the fixed operation.
    ProviderRejected,
    /// Provider completion is uncertain after an I/O failure.
    ProviderOutcomeUnknown,
    /// The provider deadline elapsed after execution began.
    ProviderTimeout,
    /// A prior process stopped after durably claiming the job.
    Interrupted,
    /// Required audit evidence could not be completed.
    AuditIncomplete,
}

impl ControlFailureCode {
    /// Returns the frozen receipt spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TargetNotFound => "TARGET_NOT_FOUND",
            Self::TopologyGenerationMismatch => "TOPOLOGY_GENERATION_MISMATCH",
            Self::EntityKindDenied => "ENTITY_KIND_DENIED",
            Self::PointDenied => "POINT_DENIED",
            Self::NotCommissioned => "NOT_COMMISSIONED",
            Self::DelegationDenied => "DELEGATION_DENIED",
            Self::PolicyDenied => "POLICY_DENIED",
            Self::ConfirmationInvalid => "CONFIRMATION_INVALID",
            Self::TargetUnavailable => "TARGET_UNAVAILABLE",
            Self::ProviderRejected => "PROVIDER_REJECTED",
            Self::ProviderOutcomeUnknown => "PROVIDER_OUTCOME_UNKNOWN",
            Self::ProviderTimeout => "PROVIDER_TIMEOUT",
            Self::Interrupted => "INTERRUPTED",
            Self::AuditIncomplete => "AUDIT_INCOMPLETE",
        }
    }
}

/// Failure returned by an injected component without leaking provider details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlDependencyError {
    failure_code: ControlFailureCode,
}

impl ControlDependencyError {
    /// Creates a redacted dependency failure.
    #[must_use]
    pub const fn new(failure_code: ControlFailureCode) -> Self {
        Self { failure_code }
    }

    /// Returns the safe receipt failure code.
    #[must_use]
    pub const fn failure_code(&self) -> ControlFailureCode {
        self.failure_code
    }
}

impl fmt::Display for ControlDependencyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Integration-control dependency rejected or unavailable")
    }
}

impl std::error::Error for ControlDependencyError {}

/// Verifies Ed25519 material using edge-local cloud trust configuration.
#[async_trait]
pub trait CloudOfferVerifier: Send + Sync + 'static {
    /// Verifies the exact RFC 8785 signed projection. Private key material is never provided.
    async fn verify(
        &self,
        key_id: &str,
        signature: &str,
        signing_bytes: &[u8],
    ) -> Result<bool, ControlDependencyError>;
}

/// Closed entity kinds eligible for semantic power control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControllableEntityKind {
    /// Home Assistant light entity.
    Light,
    /// Home Assistant switch entity.
    Switch,
    /// Home Assistant fan entity.
    Fan,
}

impl ControllableEntityKind {
    /// Returns the fixed provider domain selected inside the adapter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Fan => "fan",
        }
    }
}

/// Current local topology resolution for the exact offered target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedControlTarget {
    target: ActionTarget,
    entity_kind: ControllableEntityKind,
    source_address: String,
}

impl ResolvedControlTarget {
    /// Creates a Home Assistant target resolved from current local topology.
    pub fn home_assistant(
        target: ActionTarget,
        entity_kind: ControllableEntityKind,
        source_address: impl Into<String>,
    ) -> Result<Self, ControlDependencyError> {
        let source_address = source_address.into();
        if !valid_home_assistant_source(&source_address, entity_kind) {
            return Err(ControlDependencyError::new(
                ControlFailureCode::TargetNotFound,
            ));
        }
        Ok(Self {
            target,
            entity_kind,
            source_address,
        })
    }

    /// Returns the exact generation-fenced target resolved locally.
    #[must_use]
    pub const fn target(&self) -> &ActionTarget {
        &self.target
    }

    /// Returns the closed controllable kind.
    #[must_use]
    pub const fn entity_kind(&self) -> ControllableEntityKind {
        self.entity_kind
    }

    /// Returns the current edge-local provider source address.
    #[must_use]
    pub fn source_address(&self) -> &str {
        &self.source_address
    }
}

fn valid_home_assistant_source(value: &str, kind: ControllableEntityKind) -> bool {
    let Some((domain, object_id)) = value.split_once('.') else {
        return false;
    };
    domain == kind.as_str()
        && !object_id.is_empty()
        && object_id.len() <= 255
        && object_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Resolves stable entity identity to the current provider address and exact topology fence.
#[async_trait]
pub trait TargetResolver: Send + Sync + 'static {
    /// Resolves only the exact gateway, Integration, generation, entity, and `is_on` point.
    async fn resolve(
        &self,
        gateway_id: &str,
        target: &ActionTarget,
    ) -> Result<ResolvedControlTarget, ControlDependencyError>;
}

/// Inputs to the edge-final commissioning, delegation, policy, and confirmation decision.
pub struct LocalAuthorityRequest<'a> {
    offer: &'a ActionOffer,
    target: &'a ResolvedControlTarget,
}

impl<'a> LocalAuthorityRequest<'a> {
    pub(crate) const fn new(offer: &'a ActionOffer, target: &'a ResolvedControlTarget) -> Self {
        Self { offer, target }
    }

    /// Returns the signed offer evidence.
    #[must_use]
    pub const fn offer(&self) -> &ActionOffer {
        self.offer
    }

    /// Returns the current exact local target.
    #[must_use]
    pub const fn target(&self) -> &ResolvedControlTarget {
        self.target
    }
}

/// Explicit independent local checks. Every field must be true before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAuthorityDecision {
    /// The target was deliberately commissioned.
    pub commissioned: bool,
    /// Control of the target is delegated to this Integration.
    pub delegated: bool,
    /// Current deny-by-default policy grants `integration.device.control`.
    pub permission_granted: bool,
    /// Current local confirmation policy accepts the evidence.
    pub confirmation_valid: bool,
}

/// Supplies edge-final local commissioning, delegation, permission, and confirmation policy.
#[async_trait]
pub trait LocalControlAuthority: Send + Sync + 'static {
    /// Evaluates current local policy; cloud authorization never replaces this decision.
    async fn evaluate(
        &self,
        request: &LocalAuthorityRequest<'_>,
    ) -> Result<LocalAuthorityDecision, ControlDependencyError>;
}

/// Safe default authority used when no local control policy is composed.
pub struct DenyAllLocalControlAuthority;

#[async_trait]
impl LocalControlAuthority for DenyAllLocalControlAuthority {
    async fn evaluate(
        &self,
        _request: &LocalAuthorityRequest<'_>,
    ) -> Result<LocalAuthorityDecision, ControlDependencyError> {
        Ok(LocalAuthorityDecision {
            commissioned: false,
            delegated: false,
            permission_granted: false,
            confirmation_valid: false,
        })
    }
}

/// Closed semantic provider action. No caller-supplied domain or service exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPowerAction {
    job_id: String,
    target: ResolvedControlTarget,
    value: bool,
}

impl IntegrationPowerAction {
    pub(crate) fn new(job_id: String, target: ResolvedControlTarget, value: bool) -> Self {
        Self {
            job_id,
            target,
            value,
        }
    }

    /// Creates a closed action for adapter conformance tests and local composition.
    pub fn for_resolved_target(
        job_id: impl Into<String>,
        target: ResolvedControlTarget,
        value: bool,
    ) -> Result<Self, ControlDependencyError> {
        let job_id = job_id.into();
        uuid(&job_id)
            .map_err(|_source| ControlDependencyError::new(ControlFailureCode::TargetNotFound))?;
        Ok(Self::new(job_id, target, value))
    }

    /// Returns the governed job identity.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Returns the locally resolved exact target.
    #[must_use]
    pub const fn target(&self) -> &ResolvedControlTarget {
        &self.target
    }

    /// Returns the closed entity kind.
    #[must_use]
    pub const fn entity_kind(&self) -> ControllableEntityKind {
        self.target.entity_kind()
    }

    /// Returns the current edge-local provider address.
    #[must_use]
    pub fn source_address(&self) -> &str {
        self.target.source_address()
    }

    /// Returns the desired semantic power state.
    #[must_use]
    pub const fn value(&self) -> bool {
        self.value
    }
}

/// Provider-native request acceptance evidence, not physical completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAcceptance {
    context_id: String,
}

impl ProviderAcceptance {
    /// Creates bounded provider request-correlation evidence.
    pub fn new(context_id: impl Into<String>) -> Result<Self, ControlDependencyError> {
        let context_id = context_id.into();
        if context_id.is_empty()
            || context_id.len() > 512
            || context_id.chars().any(char::is_control)
        {
            return Err(ControlDependencyError::new(
                ControlFailureCode::ProviderOutcomeUnknown,
            ));
        }
        Ok(Self { context_id })
    }

    /// Returns provider request-correlation context.
    #[must_use]
    pub fn context_id(&self) -> &str {
        &self.context_id
    }

    /// Always false: a successful provider API response is not device evidence.
    #[must_use]
    pub const fn physical_completed(&self) -> bool {
        false
    }
}

/// Closed result of one provider invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderExecutionResult {
    /// The provider accepted the request and returned correlation context.
    Accepted(ProviderAcceptance),
    /// The provider rejected the fixed request before ambiguous completion.
    Rejected,
    /// The connection failed after dispatch may have begun.
    Unknown,
}

/// Executes only the internal fixed semantic power action.
#[async_trait]
pub trait IntegrationActionExecutor: Send + Sync + 'static {
    /// Invokes the provider once. The caller owns timeout and durable idempotency.
    async fn execute(&self, action: &IntegrationPowerAction) -> ProviderExecutionResult;
}

/// Clock used for expiry, evidence, and receipt timestamps.
pub trait ControlClock: Send + Sync + 'static {
    /// Returns Unix epoch milliseconds.
    fn now_ms(&self) -> u64;
}

/// Wall clock backed by `SystemTime`.
pub struct SystemControlClock;

impl ControlClock for SystemControlClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
            .unwrap_or_default()
    }
}

/// Generates stable receipt identities.
pub trait ControlIdGenerator: Send + Sync + 'static {
    /// Returns a new canonical UUID.
    fn next_receipt_id(&self) -> String;
}

/// Cryptographically random UUID v4 receipt identity generator.
pub struct UuidControlIdGenerator;

impl ControlIdGenerator for UuidControlIdGenerator {
    fn next_receipt_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Audit event kind at a trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEventKind {
    /// Local authority accepted the job before durable claim and provider dispatch.
    DispatchAuthorized,
    /// Local authority rejected the job.
    EdgeRejected,
    /// Provider accepted the fixed request.
    ProviderAccepted,
    /// Provider rejected the fixed request.
    ProviderRejected,
    /// Provider outcome is unknown.
    ProviderOutcomeUnknown,
    /// Startup converted a previously claimed job to unknown without retry.
    InterruptedRecovered,
}

/// Closed audit event carrying no credentials or arbitrary provider input.
pub struct AuditEvent<'a> {
    kind: AuditEventKind,
    job_id: &'a str,
    intent_digest: &'a str,
    failure_code: Option<ControlFailureCode>,
}

impl<'a> AuditEvent<'a> {
    pub(crate) const fn new(
        kind: AuditEventKind,
        job_id: &'a str,
        intent_digest: &'a str,
        failure_code: Option<ControlFailureCode>,
    ) -> Self {
        Self {
            kind,
            job_id,
            intent_digest,
            failure_code,
        }
    }

    /// Returns the audited boundary transition.
    #[must_use]
    pub const fn kind(&self) -> AuditEventKind {
        self.kind
    }

    /// Returns the governed job identity.
    #[must_use]
    pub const fn job_id(&self) -> &str {
        self.job_id
    }

    /// Returns the stable intent digest.
    #[must_use]
    pub const fn intent_digest(&self) -> &str {
        self.intent_digest
    }

    /// Returns a safe closed failure code, when applicable.
    #[must_use]
    pub const fn failure_code(&self) -> Option<ControlFailureCode> {
        self.failure_code
    }
}

/// Durable audit reference returned by the local audit sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    record_id: String,
}

impl AuditRecord {
    /// Creates a validated complete audit reference.
    pub fn complete(record_id: impl Into<String>) -> Result<Self, ControlDependencyError> {
        let record_id = record_id.into();
        identifier(&record_id)
            .map_err(|_source| ControlDependencyError::new(ControlFailureCode::AuditIncomplete))?;
        Ok(Self { record_id })
    }

    /// Returns the durable audit identity.
    #[must_use]
    pub fn record_id(&self) -> &str {
        &self.record_id
    }
}

/// Persists required local audit evidence.
#[async_trait]
pub trait IntegrationControlAudit: Send + Sync + 'static {
    /// Records one closed event and returns its durable identity.
    async fn record(&self, event: &AuditEvent<'_>) -> Result<AuditRecord, ControlDependencyError>;
}

/// Durable `(gateway_id, job_id)` identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerJobKey {
    gateway_id: String,
    job_id: String,
}

impl LedgerJobKey {
    /// Creates a validated durable job key.
    pub fn new(
        gateway_id: impl Into<String>,
        job_id: impl Into<String>,
    ) -> Result<Self, ControlDependencyError> {
        let value = Self {
            gateway_id: gateway_id.into(),
            job_id: job_id.into(),
        };
        uuid(&value.gateway_id)
            .and_then(|()| uuid(&value.job_id))
            .map_err(|_source| ControlDependencyError::new(ControlFailureCode::TargetNotFound))?;
        Ok(value)
    }

    pub(crate) fn from_offer(offer: &ActionOffer) -> Self {
        Self {
            gateway_id: offer.gateway_id().to_string(),
            job_id: offer.job_id().to_string(),
        }
    }

    /// Returns the gateway scope.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    /// Returns the governed job identity.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}

/// Persisted claim written before the provider is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerClaim {
    key: LedgerJobKey,
    intent_digest: String,
    target: ActionTarget,
    value: bool,
    audit_record_id: String,
}

impl LedgerClaim {
    pub(crate) fn from_offer(
        offer: &ActionOffer,
        audit_record_id: String,
    ) -> Result<Self, ControlDependencyError> {
        identifier(&audit_record_id)
            .map_err(|_source| ControlDependencyError::new(ControlFailureCode::AuditIncomplete))?;
        Ok(Self {
            key: LedgerJobKey::from_offer(offer),
            intent_digest: offer.intent_digest().to_string(),
            target: offer.intent().target().clone(),
            value: offer.intent().value(),
            audit_record_id,
        })
    }

    /// Creates a validated claim for ledger adapter conformance tests.
    pub fn new(
        key: LedgerJobKey,
        intent_digest: impl Into<String>,
        target: ActionTarget,
        value: bool,
        audit_record_id: impl Into<String>,
    ) -> Result<Self, ControlDependencyError> {
        let intent_digest = intent_digest.into();
        let audit_record_id = audit_record_id.into();
        crate::validation::digest(&intent_digest)
            .and_then(|()| target.validate())
            .and_then(|()| identifier(&audit_record_id))
            .map_err(|_source| ControlDependencyError::new(ControlFailureCode::AuditIncomplete))?;
        Ok(Self {
            key,
            intent_digest,
            target,
            value,
            audit_record_id,
        })
    }

    /// Returns the durable job key.
    #[must_use]
    pub const fn key(&self) -> &LedgerJobKey {
        &self.key
    }

    /// Returns the stable intent digest.
    #[must_use]
    pub fn intent_digest(&self) -> &str {
        &self.intent_digest
    }

    /// Returns the exact target needed for crash recovery.
    #[must_use]
    pub const fn target(&self) -> &ActionTarget {
        &self.target
    }

    /// Returns the desired semantic power state.
    #[must_use]
    pub const fn value(&self) -> bool {
        self.value
    }

    /// Returns the pre-dispatch audit identity.
    #[must_use]
    pub fn audit_record_id(&self) -> &str {
        &self.audit_record_id
    }
}

/// Persisted terminal or in-progress job state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "value")]
pub enum LedgerEntryState {
    /// Provider dispatch was durably claimed but no terminal receipt was persisted.
    InProgress(LedgerClaim),
    /// Terminal receipt is replayable forever for the same digest.
    Complete(ActionReceiptPayload),
}

/// One durable job entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    intent_digest: String,
    state: LedgerEntryState,
}

impl LedgerEntry {
    /// Creates a durable in-progress claim.
    #[must_use]
    pub fn in_progress(claim: LedgerClaim) -> Self {
        Self {
            intent_digest: claim.intent_digest.clone(),
            state: LedgerEntryState::InProgress(claim),
        }
    }

    /// Creates the terminal form persisted atomically with receipt spooling.
    #[must_use]
    pub fn complete(intent_digest: String, receipt: ActionReceiptPayload) -> Self {
        Self {
            intent_digest,
            state: LedgerEntryState::Complete(receipt),
        }
    }

    /// Returns the stable digest binding.
    #[must_use]
    pub fn intent_digest(&self) -> &str {
        &self.intent_digest
    }

    /// Returns the persisted state.
    #[must_use]
    pub const fn state(&self) -> &LedgerEntryState {
        &self.state
    }
}

/// Atomic claim outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerClaimOutcome {
    /// This caller durably claimed the job.
    Claimed,
    /// Same job and digest already has state.
    Existing(Box<LedgerEntry>),
    /// Same job identity is bound to a different digest.
    DigestConflict,
}

/// Redacted durable-ledger failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlStoreError;

impl fmt::Display for ControlStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Integration-control durable ledger failed")
    }
}

impl std::error::Error for ControlStoreError {}

/// One terminal receipt with a stable independent CloudLink delivery identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpooledActionReceipt {
    stream_epoch: u64,
    position: u64,
    batch_id: String,
    digest: String,
    sent_at_ms: u64,
    payload: ActionReceiptPayload,
}

impl SpooledActionReceipt {
    /// Assigns the first and only delivery identity for a terminal receipt.
    pub fn new(position: u64, payload: ActionReceiptPayload) -> Result<Self, ControlStoreError> {
        if position == 0 {
            return Err(ControlStoreError);
        }
        payload
            .validate_contract()
            .map_err(|_source| ControlStoreError)?;
        let batch_id = format!("integration-action-receipt:{}", payload.receipt_id());
        identifier(&batch_id).map_err(|_source| ControlStoreError)?;
        let digest = IntegrationControlCodec::receipt_digest(&payload)
            .map_err(|_source| ControlStoreError)?;
        Ok(Self {
            stream_epoch: 1,
            position,
            batch_id,
            digest,
            sent_at_ms: payload.observed_at_ms(),
            payload,
        })
    }

    /// Revalidates persisted delivery and business identity.
    pub fn validate(&self) -> Result<(), ControlStoreError> {
        let rebuilt = Self::new(self.position, self.payload.clone())?;
        if &rebuilt != self {
            return Err(ControlStoreError);
        }
        Ok(())
    }

    /// Returns the fixed receipt stream epoch.
    #[must_use]
    pub const fn stream_epoch(&self) -> u64 {
        self.stream_epoch
    }

    /// Returns the stable stream position.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.position
    }

    /// Returns the stable delivery batch identity.
    #[must_use]
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// Returns the canonical CloudLink business digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the immutable envelope send time sealed with this durable fact.
    #[must_use]
    pub const fn sent_at_ms(&self) -> u64 {
        self.sent_at_ms
    }

    /// Returns the closed terminal business receipt.
    #[must_use]
    pub const fn payload(&self) -> &ActionReceiptPayload {
        &self.payload
    }

    /// Creates the exact descriptor used in an authenticated uplink envelope.
    pub fn delivery(&self) -> Result<ReceiptDelivery, ControlStoreError> {
        ReceiptDelivery::new(
            self.stream_epoch,
            self.position,
            &self.batch_id,
            &self.digest,
        )
        .map_err(|_source| ControlStoreError)
    }
}

/// Durable idempotency ledger and terminal receipt spool.
#[async_trait]
pub trait IntegrationControlLedger: Send + Sync + 'static {
    /// Reads current state for a job key.
    async fn find(&self, key: &LedgerJobKey) -> Result<Option<LedgerEntry>, ControlStoreError>;

    /// Atomically claims an absent job or returns its existing digest-bound state.
    async fn claim(&self, claim: LedgerClaim) -> Result<LedgerClaimOutcome, ControlStoreError>;

    /// Atomically persists a terminal receipt and queues it for durable publication.
    async fn complete(
        &self,
        key: &LedgerJobKey,
        intent_digest: &str,
        receipt: ActionReceiptPayload,
    ) -> Result<(), ControlStoreError>;

    /// Requeues a stored receipt after an authenticated offer replay.
    async fn requeue(&self, key: &LedgerJobKey) -> Result<(), ControlStoreError>;

    /// Returns jobs claimed by a previous process but lacking a terminal receipt.
    async fn interrupted(&self) -> Result<Vec<LedgerClaim>, ControlStoreError>;

    /// Returns pending receipts without removing them.
    async fn pending_receipts(
        &self,
        limit: usize,
    ) -> Result<Vec<SpooledActionReceipt>, ControlStoreError>;

    /// Applies one exact existing CloudLink durable ACK through a contiguous position.
    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, ControlStoreError>;
}
