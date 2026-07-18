use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aether_integration_control::{
    ActionReceiptStage, AuditEvent, AuditRecord, CloudOfferVerifier, ControlClock,
    ControlDependencyError, ControlIdGenerator, ControlSession, IntegrationActionExecutor,
    IntegrationControlAudit, IntegrationControlCodec, IntegrationControlConfig,
    IntegrationControlLedger, IntegrationControlProcessor, LedgerClaim, LedgerClaimOutcome,
    LedgerJobKey, LocalAuthorityDecision, LocalControlAuthority, MemoryIntegrationControlLedger,
    ProcessDisposition, ProviderAcceptance, ProviderExecutionResult, ResolvedControlTarget,
    TargetResolver,
};
use async_trait::async_trait;

const OFFER: &[u8] =
    include_bytes!("fixtures/integration-control/v1alpha1/action-offer.valid.json");

struct FixedClock(u64);

impl ControlClock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

struct AcceptingVerifier {
    calls: AtomicUsize,
}

struct RejectingVerifier;

#[async_trait]
impl CloudOfferVerifier for RejectingVerifier {
    async fn verify(
        &self,
        _key_id: &str,
        _signature: &str,
        _signing_bytes: &[u8],
    ) -> Result<bool, ControlDependencyError> {
        Ok(false)
    }
}

#[async_trait]
impl CloudOfferVerifier for AcceptingVerifier {
    async fn verify(
        &self,
        _key_id: &str,
        _signature: &str,
        signing_bytes: &[u8],
    ) -> Result<bool, ControlDependencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let value: serde_json::Value =
            serde_json::from_slice(signing_bytes).expect("signed object");
        assert!(value.get("cloud_authentication").is_none());
        Ok(true)
    }
}

struct ExactTargetResolver;

#[async_trait]
impl TargetResolver for ExactTargetResolver {
    async fn resolve(
        &self,
        _gateway_id: &str,
        target: &aether_integration_control::ActionTarget,
    ) -> Result<ResolvedControlTarget, ControlDependencyError> {
        ResolvedControlTarget::home_assistant(
            target.clone(),
            aether_integration_control::ControllableEntityKind::Light,
            "light.bedroom",
        )
    }
}

struct AllowLocalAuthority;

#[async_trait]
impl LocalControlAuthority for AllowLocalAuthority {
    async fn evaluate(
        &self,
        _request: &aether_integration_control::LocalAuthorityRequest<'_>,
    ) -> Result<LocalAuthorityDecision, ControlDependencyError> {
        Ok(LocalAuthorityDecision {
            commissioned: true,
            delegated: true,
            permission_granted: true,
            confirmation_valid: true,
        })
    }
}

struct StaticLocalAuthority(LocalAuthorityDecision);

#[async_trait]
impl LocalControlAuthority for StaticLocalAuthority {
    async fn evaluate(
        &self,
        _request: &aether_integration_control::LocalAuthorityRequest<'_>,
    ) -> Result<LocalAuthorityDecision, ControlDependencyError> {
        Ok(self.0)
    }
}

struct RecordingAudit;

#[async_trait]
impl IntegrationControlAudit for RecordingAudit {
    async fn record(&self, _event: &AuditEvent<'_>) -> Result<AuditRecord, ControlDependencyError> {
        AuditRecord::complete("audit-control-1")
    }
}

struct CountingExecutor {
    calls: AtomicUsize,
}

#[async_trait]
impl IntegrationActionExecutor for CountingExecutor {
    async fn execute(
        &self,
        action: &aether_integration_control::IntegrationPowerAction,
    ) -> ProviderExecutionResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(action.source_address(), "light.bedroom");
        assert!(action.value());
        ProviderExecutionResult::Accepted(
            ProviderAcceptance::new("ctx-provider-accepted").expect("provider context"),
        )
    }
}

struct FixedId;

impl ControlIdGenerator for FixedId {
    fn next_receipt_id(&self) -> String {
        "77777777-7777-4777-8777-777777777777".to_string()
    }
}

fn session() -> ControlSession {
    ControlSession::new(
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
        7,
        3,
    )
    .expect("session")
}

fn processor(
    enabled: bool,
    ledger: Arc<MemoryIntegrationControlLedger>,
    verifier: Arc<AcceptingVerifier>,
    executor: Arc<CountingExecutor>,
) -> IntegrationControlProcessor {
    let config = if enabled {
        IntegrationControlConfig::enabled(Duration::from_millis(100)).expect("config")
    } else {
        IntegrationControlConfig::default()
    };
    IntegrationControlProcessor::new(
        config,
        session(),
        verifier,
        Arc::new(ExactTargetResolver),
        Arc::new(AllowLocalAuthority),
        Arc::new(RecordingAudit),
        ledger,
        executor,
        Arc::new(FixedClock(1_784_217_600_100)),
        Arc::new(FixedId),
    )
}

#[tokio::test]
async fn integration_control_is_default_off_before_signature_or_provider_use() {
    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let verifier = Arc::new(AcceptingVerifier {
        calls: AtomicUsize::new(0),
    });
    let executor = Arc::new(CountingExecutor {
        calls: AtomicUsize::new(0),
    });
    let error = processor(false, ledger, Arc::clone(&verifier), Arc::clone(&executor))
        .process(OFFER)
        .await
        .expect_err("default-off control");
    assert_eq!(
        error.code(),
        aether_integration_control::IntegrationControlErrorCode::Disabled
    );
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejected_cloud_signature_never_reaches_local_authority_or_provider() {
    let executor = Arc::new(CountingExecutor {
        calls: AtomicUsize::new(0),
    });
    let processor = IntegrationControlProcessor::new(
        IntegrationControlConfig::enabled(Duration::from_millis(100)).expect("config"),
        session(),
        Arc::new(RejectingVerifier),
        Arc::new(ExactTargetResolver),
        Arc::new(AllowLocalAuthority),
        Arc::new(RecordingAudit),
        Arc::new(MemoryIntegrationControlLedger::new()),
        Arc::clone(&executor) as Arc<dyn IntegrationActionExecutor>,
        Arc::new(FixedClock(1_784_217_600_100)),
        Arc::new(FixedId),
    );
    let error = processor
        .process(OFFER)
        .await
        .expect_err("signature rejection");
    assert_eq!(
        error.code(),
        aether_integration_control::IntegrationControlErrorCode::SignatureRejected
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn same_job_and_digest_replays_one_stored_receipt_without_second_execution() {
    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let verifier = Arc::new(AcceptingVerifier {
        calls: AtomicUsize::new(0),
    });
    let executor = Arc::new(CountingExecutor {
        calls: AtomicUsize::new(0),
    });
    let processor = processor(
        true,
        Arc::clone(&ledger),
        Arc::clone(&verifier),
        Arc::clone(&executor),
    );

    let first = processor.process(OFFER).await.expect("first execution");
    assert_eq!(first.disposition(), ProcessDisposition::Executed);
    assert_eq!(
        first.receipt().stage(),
        ActionReceiptStage::ProviderAccepted
    );
    assert!(!first.receipt().physical_completed());
    assert!(!first.receipt().job_succeeded());

    let replay = processor.process(OFFER).await.expect("receipt replay");
    assert_eq!(replay.disposition(), ProcessDisposition::Replayed);
    assert_eq!(replay.receipt(), first.receipt());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.pending_receipt_count().await, 1);
}

#[tokio::test]
async fn timeout_becomes_unknown_and_is_never_retried() {
    struct HangingExecutor {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl IntegrationActionExecutor for HangingExecutor {
        async fn execute(
            &self,
            _action: &aether_integration_control::IntegrationPowerAction,
        ) -> ProviderExecutionResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let verifier = Arc::new(AcceptingVerifier {
        calls: AtomicUsize::new(0),
    });
    let executor = Arc::new(HangingExecutor {
        calls: AtomicUsize::new(0),
    });
    let processor = IntegrationControlProcessor::new(
        IntegrationControlConfig::enabled(Duration::from_millis(5)).expect("config"),
        session(),
        verifier,
        Arc::new(ExactTargetResolver),
        Arc::new(AllowLocalAuthority),
        Arc::new(RecordingAudit),
        ledger,
        Arc::clone(&executor) as Arc<dyn IntegrationActionExecutor>,
        Arc::new(FixedClock(1_784_217_600_100)),
        Arc::new(FixedId),
    );

    let first = processor.process(OFFER).await.expect("unknown receipt");
    assert_eq!(first.receipt().stage(), ActionReceiptStage::Unknown);
    let replay = processor.process(OFFER).await.expect("stored unknown");
    assert_eq!(replay.disposition(), ProcessDisposition::Replayed);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn codec_rejects_a_same_job_with_a_changed_intent_digest_at_the_wire_boundary() {
    let offer = IntegrationControlCodec::decode_offer(OFFER).expect("offer");
    assert_eq!(
        offer.intent_digest(),
        "sha256:40108827ca617c95f9d9c48c357fdd94b2b5f019d8ccf8a23842642e934c7327"
    );
}

#[tokio::test]
async fn same_job_with_a_different_valid_digest_is_a_conflict_without_execution() {
    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let verifier = Arc::new(AcceptingVerifier {
        calls: AtomicUsize::new(0),
    });
    let executor = Arc::new(CountingExecutor {
        calls: AtomicUsize::new(0),
    });
    let processor = processor(true, ledger, verifier, Arc::clone(&executor));
    processor.process(OFFER).await.expect("first execution");

    let mut changed: serde_json::Value = serde_json::from_slice(OFFER).expect("offer JSON");
    changed["intent"]["arguments"]["value"] = serde_json::Value::Bool(false);
    let digest =
        IntegrationControlCodec::intent_digest_json(&changed["intent"]).expect("changed digest");
    changed["intent_digest"] = serde_json::Value::String(digest);
    let changed = serde_json::to_vec(&changed).expect("changed offer");
    let error = processor
        .process(&changed)
        .await
        .expect_err("digest conflict");
    assert_eq!(
        error.code(),
        aether_integration_control::IntegrationControlErrorCode::DigestConflict
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn each_local_authority_gate_rejects_before_provider_dispatch() {
    let cases = [
        (
            LocalAuthorityDecision {
                commissioned: false,
                delegated: true,
                permission_granted: true,
                confirmation_valid: true,
            },
            "NOT_COMMISSIONED",
        ),
        (
            LocalAuthorityDecision {
                commissioned: true,
                delegated: false,
                permission_granted: true,
                confirmation_valid: true,
            },
            "DELEGATION_DENIED",
        ),
        (
            LocalAuthorityDecision {
                commissioned: true,
                delegated: true,
                permission_granted: false,
                confirmation_valid: true,
            },
            "POLICY_DENIED",
        ),
        (
            LocalAuthorityDecision {
                commissioned: true,
                delegated: true,
                permission_granted: true,
                confirmation_valid: false,
            },
            "CONFIRMATION_INVALID",
        ),
    ];

    for (decision, failure_code) in cases {
        let executor = Arc::new(CountingExecutor {
            calls: AtomicUsize::new(0),
        });
        let processor = IntegrationControlProcessor::new(
            IntegrationControlConfig::enabled(Duration::from_millis(100)).expect("config"),
            session(),
            Arc::new(AcceptingVerifier {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ExactTargetResolver),
            Arc::new(StaticLocalAuthority(decision)),
            Arc::new(RecordingAudit),
            Arc::new(MemoryIntegrationControlLedger::new()),
            Arc::clone(&executor) as Arc<dyn IntegrationActionExecutor>,
            Arc::new(FixedClock(1_784_217_600_100)),
            Arc::new(FixedId),
        );
        let result = processor.process(OFFER).await.expect("edge rejection");
        assert_eq!(result.receipt().stage(), ActionReceiptStage::EdgeRejected);
        assert_eq!(result.receipt().failure_code(), Some(failure_code));
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn startup_recovery_marks_a_claim_unknown_and_future_delivery_only_replays_it() {
    let offer = IntegrationControlCodec::decode_offer(OFFER).expect("offer");
    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let claim = LedgerClaim::new(
        LedgerJobKey::new(offer.gateway_id(), offer.job_id()).expect("key"),
        offer.intent_digest(),
        offer.intent().target().clone(),
        offer.intent().value(),
        "audit-before-crash",
    )
    .expect("claim");
    assert_eq!(
        ledger.claim(claim).await.expect("claim"),
        LedgerClaimOutcome::Claimed
    );
    let executor = Arc::new(CountingExecutor {
        calls: AtomicUsize::new(0),
    });
    let processor = processor(
        true,
        ledger,
        Arc::new(AcceptingVerifier {
            calls: AtomicUsize::new(0),
        }),
        Arc::clone(&executor),
    );

    let recovered = processor.recover_interrupted().await.expect("recovery");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].disposition(), ProcessDisposition::Recovered);
    assert_eq!(recovered[0].receipt().stage(), ActionReceiptStage::Unknown);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);

    let replay = processor.process(OFFER).await.expect("replay");
    assert_eq!(replay.disposition(), ProcessDisposition::Replayed);
    assert_eq!(replay.receipt(), recovered[0].receipt());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn session_fence_and_expiry_are_checked_before_local_execution() {
    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let verifier = Arc::new(AcceptingVerifier {
        calls: AtomicUsize::new(0),
    });
    let executor = Arc::new(CountingExecutor {
        calls: AtomicUsize::new(0),
    });
    let processor = processor(
        true,
        Arc::clone(&ledger),
        Arc::clone(&verifier),
        Arc::clone(&executor),
    );
    let mut wrong_session: serde_json::Value = serde_json::from_slice(OFFER).expect("offer");
    wrong_session["credential_generation"] = serde_json::Value::String("4".to_string());
    let error = processor
        .process(&serde_json::to_vec(&wrong_session).expect("offer bytes"))
        .await
        .expect_err("session mismatch");
    assert_eq!(
        error.code(),
        aether_integration_control::IntegrationControlErrorCode::SessionMismatch
    );
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);

    let expired = IntegrationControlProcessor::new(
        IntegrationControlConfig::enabled(Duration::from_millis(100)).expect("config"),
        session(),
        verifier,
        Arc::new(ExactTargetResolver),
        Arc::new(AllowLocalAuthority),
        Arc::new(RecordingAudit),
        ledger,
        Arc::clone(&executor) as Arc<dyn IntegrationActionExecutor>,
        Arc::new(FixedClock(1_784_217_660_000)),
        Arc::new(FixedId),
    );
    let error = expired.process(OFFER).await.expect_err("expired offer");
    assert_eq!(
        error.code(),
        aether_integration_control::IntegrationControlErrorCode::Expired
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}
