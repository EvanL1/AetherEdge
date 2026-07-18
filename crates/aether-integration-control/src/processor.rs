//! Governed job state machine with durable no-retry semantics.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::time::timeout;

use crate::IntegrationControlCodec;
use crate::error::{
    ControlResult, IntegrationControlError, IntegrationControlErrorCode as ErrorCode,
};
use crate::ports::{
    AuditEvent, AuditEventKind, CloudOfferVerifier, ControlClock, ControlFailureCode,
    ControlIdGenerator, ControlSession, IntegrationActionExecutor, IntegrationControlAudit,
    IntegrationControlLedger, LedgerClaim, LedgerClaimOutcome, LedgerEntry, LedgerEntryState,
    LedgerJobKey, LocalAuthorityRequest, LocalControlAuthority, ProviderExecutionResult,
    ResolvedControlTarget, TargetResolver,
};
use crate::wire::{
    ActionDecision, ActionOffer, ActionReceiptPayload, ActionReceiptStage, AuditStatus,
    ReceiptAudit,
};

/// Explicit runtime activation and provider deadline.
#[derive(Debug, Clone)]
pub struct IntegrationControlConfig {
    enabled: bool,
    provider_timeout: Duration,
}

impl IntegrationControlConfig {
    /// Explicitly enables the experimental extension with a bounded provider deadline.
    pub fn enabled(provider_timeout: Duration) -> ControlResult<Self> {
        if provider_timeout.is_zero() || provider_timeout > Duration::from_secs(300) {
            return Err(IntegrationControlError::new(
                ErrorCode::InvalidMessage,
                "provider timeout must be between zero and five minutes",
            ));
        }
        Ok(Self {
            enabled: true,
            provider_timeout,
        })
    }

    /// Returns whether control was explicitly enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Default for IntegrationControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_timeout: Duration::from_secs(5),
        }
    }
}

/// How a terminal receipt was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessDisposition {
    /// This call made the one permitted provider attempt or a new local rejection.
    Executed,
    /// A previously stored terminal receipt was replayed without provider execution.
    Replayed,
    /// Startup converted an interrupted claim to unknown without provider retry.
    Recovered,
}

/// Terminal process result ready for durable CloudLink publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedReceipt {
    disposition: ProcessDisposition,
    receipt: ActionReceiptPayload,
}

impl ProcessedReceipt {
    fn new(disposition: ProcessDisposition, receipt: ActionReceiptPayload) -> Self {
        Self {
            disposition,
            receipt,
        }
    }

    /// Returns whether this was new work, replay, or crash recovery.
    #[must_use]
    pub const fn disposition(&self) -> ProcessDisposition {
        self.disposition
    }

    /// Returns the durable terminal receipt.
    #[must_use]
    pub const fn receipt(&self) -> &ActionReceiptPayload {
        &self.receipt
    }
}

/// Default-off Integration-control application service.
pub struct IntegrationControlProcessor {
    config: IntegrationControlConfig,
    session: ControlSession,
    verifier: Arc<dyn CloudOfferVerifier>,
    target_resolver: Arc<dyn TargetResolver>,
    local_authority: Arc<dyn LocalControlAuthority>,
    audit: Arc<dyn IntegrationControlAudit>,
    ledger: Arc<dyn IntegrationControlLedger>,
    executor: Arc<dyn IntegrationActionExecutor>,
    clock: Arc<dyn ControlClock>,
    ids: Arc<dyn ControlIdGenerator>,
}

impl IntegrationControlProcessor {
    /// Composes all trust boundaries without starting background work.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        config: IntegrationControlConfig,
        session: ControlSession,
        verifier: Arc<dyn CloudOfferVerifier>,
        target_resolver: Arc<dyn TargetResolver>,
        local_authority: Arc<dyn LocalControlAuthority>,
        audit: Arc<dyn IntegrationControlAudit>,
        ledger: Arc<dyn IntegrationControlLedger>,
        executor: Arc<dyn IntegrationActionExecutor>,
        clock: Arc<dyn ControlClock>,
        ids: Arc<dyn ControlIdGenerator>,
    ) -> Self {
        Self {
            config,
            session,
            verifier,
            target_resolver,
            local_authority,
            audit,
            ledger,
            executor,
            clock,
            ids,
        }
    }

    /// Validates, authorizes, durably claims, and attempts one governed offer.
    ///
    /// A completed same-digest job only requeues its stored receipt. A claimed
    /// job never invokes the provider again; call [`Self::recover_interrupted`]
    /// once during startup before accepting downlink messages.
    pub async fn process(&self, bytes: &[u8]) -> ControlResult<ProcessedReceipt> {
        if !self.config.enabled {
            return Err(IntegrationControlError::new(
                ErrorCode::Disabled,
                "Integration control is disabled",
            ));
        }

        let offer = IntegrationControlCodec::decode_offer(bytes)?;
        self.validate_session(&offer)?;
        self.verify_signature(&offer).await?;
        let key = LedgerJobKey::from_offer(&offer);

        if let Some(existing) = self.ledger.find(&key).await.map_err(ledger_error)? {
            return self.replay_existing(&key, &offer, existing).await;
        }

        let now = self.clock.now_ms();
        if offer.issued_at_ms() > now || now >= offer.expires_at_ms() {
            return Err(IntegrationControlError::new(
                ErrorCode::Expired,
                "Integration-control offer is outside its execution window",
            ));
        }

        let target = match self
            .target_resolver
            .resolve(offer.gateway_id(), offer.intent().target())
            .await
        {
            Ok(target) if target.target() == offer.intent().target() => target,
            Ok(_target) => {
                return self
                    .persist_local_rejection(
                        &offer,
                        ControlFailureCode::TargetNotFound,
                        AuditStatus::Complete,
                    )
                    .await;
            },
            Err(error) => {
                return self
                    .persist_local_rejection(&offer, error.failure_code(), AuditStatus::Complete)
                    .await;
            },
        };

        let authority = match self
            .local_authority
            .evaluate(&LocalAuthorityRequest::new(&offer, &target))
            .await
        {
            Ok(decision) => decision,
            Err(error) => {
                return self
                    .persist_local_rejection(&offer, error.failure_code(), AuditStatus::Complete)
                    .await;
            },
        };
        if !authority.commissioned {
            return self
                .persist_local_rejection(
                    &offer,
                    ControlFailureCode::NotCommissioned,
                    AuditStatus::Complete,
                )
                .await;
        }
        if !authority.delegated {
            return self
                .persist_local_rejection(
                    &offer,
                    ControlFailureCode::DelegationDenied,
                    AuditStatus::Complete,
                )
                .await;
        }
        if !authority.permission_granted {
            return self
                .persist_local_rejection(
                    &offer,
                    ControlFailureCode::PolicyDenied,
                    AuditStatus::Complete,
                )
                .await;
        }
        if !authority.confirmation_valid {
            return self
                .persist_local_rejection(
                    &offer,
                    ControlFailureCode::ConfirmationInvalid,
                    AuditStatus::Complete,
                )
                .await;
        }

        self.dispatch(&offer, target).await
    }

    /// Converts every persisted in-progress claim to `unknown` without provider execution.
    ///
    /// Production composition must call this once after opening the persistent
    /// ledger and before enabling the offer subscription.
    pub async fn recover_interrupted(&self) -> ControlResult<Vec<ProcessedReceipt>> {
        let claims = self.ledger.interrupted().await.map_err(ledger_error)?;
        let mut recovered = Vec::with_capacity(claims.len());
        for claim in claims {
            let event = AuditEvent::new(
                AuditEventKind::InterruptedRecovered,
                claim.key().job_id(),
                claim.intent_digest(),
                Some(ControlFailureCode::Interrupted),
            );
            let (audit_id, audit_status) = match self.audit.record(&event).await {
                Ok(record) => (record.record_id().to_string(), AuditStatus::Complete),
                Err(_error) => (claim.audit_record_id().to_string(), AuditStatus::Incomplete),
            };
            let receipt = self.make_receipt(
                claim.key().job_id(),
                claim.target().clone(),
                claim.intent_digest(),
                ActionReceiptStage::Unknown,
                ActionDecision::Unknown,
                Some(ControlFailureCode::Interrupted),
                None,
                &audit_id,
                audit_status,
            )?;
            self.ledger
                .complete(claim.key(), claim.intent_digest(), receipt.clone())
                .await
                .map_err(ledger_error)?;
            recovered.push(ProcessedReceipt::new(
                ProcessDisposition::Recovered,
                receipt,
            ));
        }
        Ok(recovered)
    }

    fn validate_session(&self, offer: &ActionOffer) -> ControlResult<()> {
        if offer.gateway_id() != self.session.gateway_id()
            || offer.session_id() != self.session.session_id()
            || offer.session_epoch() != self.session.session_epoch()
            || offer.credential_generation() != self.session.credential_generation()
        {
            return Err(IntegrationControlError::new(
                ErrorCode::SessionMismatch,
                "offer does not match the current authenticated session",
            ));
        }
        Ok(())
    }

    async fn verify_signature(&self, offer: &ActionOffer) -> ControlResult<()> {
        let authentication = offer.cloud_authentication();
        let verified = self
            .verifier
            .verify(
                authentication.key_id(),
                authentication.signature(),
                &offer.signing_bytes()?,
            )
            .await
            .map_err(|_error| {
                IntegrationControlError::new(
                    ErrorCode::DependencyUnavailable,
                    "cloud signature verifier is unavailable",
                )
            })?;
        if !verified {
            return Err(IntegrationControlError::new(
                ErrorCode::SignatureRejected,
                "cloud signature was rejected",
            ));
        }
        Ok(())
    }

    async fn replay_existing(
        &self,
        key: &LedgerJobKey,
        offer: &ActionOffer,
        existing: LedgerEntry,
    ) -> ControlResult<ProcessedReceipt> {
        if existing.intent_digest() != offer.intent_digest() {
            return Err(IntegrationControlError::new(
                ErrorCode::DigestConflict,
                "job identity was reused with a different intent digest",
            ));
        }
        match existing.state() {
            LedgerEntryState::Complete(receipt) => {
                self.ledger.requeue(key).await.map_err(ledger_error)?;
                Ok(ProcessedReceipt::new(
                    ProcessDisposition::Replayed,
                    receipt.clone(),
                ))
            },
            LedgerEntryState::InProgress(_claim) => Err(IntegrationControlError::new(
                ErrorCode::JobInProgress,
                "job was already durably claimed; provider retry is forbidden",
            )),
        }
    }

    async fn persist_local_rejection(
        &self,
        offer: &ActionOffer,
        failure: ControlFailureCode,
        requested_audit_status: AuditStatus,
    ) -> ControlResult<ProcessedReceipt> {
        let event = AuditEvent::new(
            AuditEventKind::EdgeRejected,
            offer.job_id(),
            offer.intent_digest(),
            Some(failure),
        );
        let (audit_id, audit_status) = match self.audit.record(&event).await {
            Ok(record) => (record.record_id().to_string(), requested_audit_status),
            Err(_error) => ("audit-incomplete".to_string(), AuditStatus::Incomplete),
        };
        let claim = LedgerClaim::from_offer(offer, audit_id.clone()).map_err(dependency_error)?;
        let key = claim.key().clone();
        match self.ledger.claim(claim).await.map_err(ledger_error)? {
            LedgerClaimOutcome::Claimed => {},
            LedgerClaimOutcome::Existing(existing) => {
                return self.replay_existing(&key, offer, *existing).await;
            },
            LedgerClaimOutcome::DigestConflict => {
                return Err(IntegrationControlError::new(
                    ErrorCode::DigestConflict,
                    "job identity was reused with a different intent digest",
                ));
            },
        }
        let receipt = self.make_receipt(
            offer.job_id(),
            offer.intent().target().clone(),
            offer.intent_digest(),
            ActionReceiptStage::EdgeRejected,
            ActionDecision::Rejected,
            Some(failure),
            None,
            &audit_id,
            audit_status,
        )?;
        self.ledger
            .complete(&key, offer.intent_digest(), receipt.clone())
            .await
            .map_err(ledger_error)?;
        Ok(ProcessedReceipt::new(ProcessDisposition::Executed, receipt))
    }

    async fn dispatch(
        &self,
        offer: &ActionOffer,
        target: ResolvedControlTarget,
    ) -> ControlResult<ProcessedReceipt> {
        let authorized = AuditEvent::new(
            AuditEventKind::DispatchAuthorized,
            offer.job_id(),
            offer.intent_digest(),
            None,
        );
        let audit_record = match self.audit.record(&authorized).await {
            Ok(record) => record,
            Err(_error) => {
                return self
                    .persist_local_rejection(
                        offer,
                        ControlFailureCode::AuditIncomplete,
                        AuditStatus::Incomplete,
                    )
                    .await;
            },
        };
        let claim = LedgerClaim::from_offer(offer, audit_record.record_id().to_string())
            .map_err(dependency_error)?;
        let key = claim.key().clone();
        match self.ledger.claim(claim).await.map_err(ledger_error)? {
            LedgerClaimOutcome::Claimed => {},
            LedgerClaimOutcome::Existing(existing) => {
                return self.replay_existing(&key, offer, *existing).await;
            },
            LedgerClaimOutcome::DigestConflict => {
                return Err(IntegrationControlError::new(
                    ErrorCode::DigestConflict,
                    "job identity was reused with a different intent digest",
                ));
            },
        }

        let action = crate::IntegrationPowerAction::new(
            offer.job_id().to_string(),
            target,
            offer.intent().value(),
        );
        let (provider_result, timed_out) =
            match timeout(self.config.provider_timeout, self.executor.execute(&action)).await {
                Ok(result) => (result, false),
                Err(_elapsed) => (ProviderExecutionResult::Unknown, true),
            };
        let observed_at_ms = self.clock.now_ms();
        let (stage, decision, failure, evidence, audit_kind) = match provider_result {
            ProviderExecutionResult::Accepted(acceptance) => (
                ActionReceiptStage::ProviderAccepted,
                ActionDecision::Accepted,
                None,
                Some(provider_evidence_digest(
                    "accepted",
                    Some(acceptance.context_id()),
                    observed_at_ms,
                )?),
                AuditEventKind::ProviderAccepted,
            ),
            ProviderExecutionResult::Rejected => (
                ActionReceiptStage::ProviderRejected,
                ActionDecision::Rejected,
                Some(ControlFailureCode::ProviderRejected),
                Some(provider_evidence_digest("rejected", None, observed_at_ms)?),
                AuditEventKind::ProviderRejected,
            ),
            ProviderExecutionResult::Unknown => (
                ActionReceiptStage::Unknown,
                ActionDecision::Unknown,
                Some(if timed_out {
                    ControlFailureCode::ProviderTimeout
                } else {
                    ControlFailureCode::ProviderOutcomeUnknown
                }),
                Some(provider_evidence_digest("unknown", None, observed_at_ms)?),
                AuditEventKind::ProviderOutcomeUnknown,
            ),
        };

        let outcome_event =
            AuditEvent::new(audit_kind, offer.job_id(), offer.intent_digest(), failure);
        let (stage, decision, failure, audit_id, audit_status) =
            match self.audit.record(&outcome_event).await {
                Ok(record) => (
                    stage,
                    decision,
                    failure,
                    record.record_id().to_string(),
                    AuditStatus::Complete,
                ),
                Err(_error) => (
                    ActionReceiptStage::Unknown,
                    ActionDecision::Unknown,
                    Some(ControlFailureCode::AuditIncomplete),
                    audit_record.record_id().to_string(),
                    AuditStatus::Incomplete,
                ),
            };
        let receipt = self.make_receipt(
            offer.job_id(),
            offer.intent().target().clone(),
            offer.intent_digest(),
            stage,
            decision,
            failure,
            evidence,
            &audit_id,
            audit_status,
        )?;
        self.ledger
            .complete(&key, offer.intent_digest(), receipt.clone())
            .await
            .map_err(ledger_error)?;
        Ok(ProcessedReceipt::new(ProcessDisposition::Executed, receipt))
    }

    #[allow(clippy::too_many_arguments)]
    fn make_receipt(
        &self,
        job_id: &str,
        target: crate::ActionTarget,
        intent_digest: &str,
        stage: ActionReceiptStage,
        decision: ActionDecision,
        failure: Option<ControlFailureCode>,
        evidence_digest: Option<String>,
        audit_id: &str,
        audit_status: AuditStatus,
    ) -> ControlResult<ActionReceiptPayload> {
        ActionReceiptPayload::terminal(
            job_id.to_string(),
            self.ids.next_receipt_id(),
            target,
            intent_digest.to_string(),
            stage,
            decision,
            self.clock.now_ms(),
            evidence_digest,
            failure.map(|code| code.as_str().to_string()),
            ReceiptAudit::new(audit_id.to_string(), audit_status)?,
        )
    }
}

fn provider_evidence_digest(
    result: &'static str,
    context_id: Option<&str>,
    observed_at_ms: u64,
) -> ControlResult<String> {
    #[derive(Serialize)]
    struct ProviderEvidence<'a> {
        schema: &'static str,
        provider: &'static str,
        result: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        context_id: Option<&'a str>,
        observed_at_ms: String,
    }
    let bytes = serde_json_canonicalizer::to_vec(&ProviderEvidence {
        schema: "aether.edge.integration-control.provider-evidence.v1",
        provider: "home-assistant",
        result,
        context_id,
        observed_at_ms: observed_at_ms.to_string(),
    })
    .map_err(|_source| {
        IntegrationControlError::new(
            ErrorCode::InvalidMessage,
            "provider evidence canonicalization failed",
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn ledger_error(_error: crate::ControlStoreError) -> IntegrationControlError {
    IntegrationControlError::new(
        ErrorCode::LedgerFailure,
        "Integration-control ledger transition failed",
    )
}

fn dependency_error(_error: crate::ControlDependencyError) -> IntegrationControlError {
    IntegrationControlError::new(
        ErrorCode::DependencyUnavailable,
        "Integration-control dependency rejected local data",
    )
}
