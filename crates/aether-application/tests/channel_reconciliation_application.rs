use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, ApplicationError, AuditPolicy, ChannelReconciliationApplication, ConfirmationPolicy,
    OperationKind, RECONCILE_CHANNELS_CAPABILITY, RequestContext, RiskLevel, SafetyPolicy,
    capability_catalog,
};
use aether_domain::{ChannelId, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelDesiredStateObservation, ChannelReconciler,
    ChannelReconciliationItem, ChannelReconciliationReceipt, ChannelReconciliationScope,
    ChannelRevision, ChannelRuntimeProjection, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

struct RecordingReconciler {
    scopes: Mutex<Vec<ChannelReconciliationScope>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    failure: Option<PortError>,
}

impl RecordingReconciler {
    fn successful(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            scopes: Mutex::new(Vec::new()),
            events,
            failure: None,
        }
    }

    fn failing(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            scopes: Mutex::new(Vec::new()),
            events,
            failure: Some(PortError::new(
                PortErrorKind::Unavailable,
                "runtime reconciliation unavailable",
            )),
        }
    }
}

#[async_trait]
impl ChannelReconciler for RecordingReconciler {
    async fn reconcile(
        &self,
        scope: ChannelReconciliationScope,
    ) -> PortResult<ChannelReconciliationReceipt> {
        self.events.lock().expect("event lock").push("reconcile");
        self.scopes.lock().expect("scope lock").push(scope);
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        Ok(ChannelReconciliationReceipt::new(
            scope,
            vec![ChannelReconciliationItem::new(
                ChannelId::new(7),
                ChannelDesiredStateObservation::present(ChannelRevision::new(4), true),
                ChannelRuntimeProjection::Degraded,
            )],
        ))
    }
}

struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    calls: Mutex<usize>,
    fail_on_call: Option<usize>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingAudit {
    fn successful(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
            fail_on_call: None,
            events,
        }
    }

    fn failing_on(events: Arc<Mutex<Vec<&'static str>>>, call: usize) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
            fail_on_call: Some(call),
            events,
        }
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let call = {
            let mut calls = self.calls.lock().expect("call lock");
            *calls += 1;
            *calls
        };
        if self.fail_on_call == Some(call) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit sink unavailable",
            ));
        }
        let event = match record.outcome() {
            AuditOutcome::Rejected => "audit.rejected",
            AuditOutcome::Attempted => "audit.attempted",
            AuditOutcome::Succeeded => "audit.succeeded",
            AuditOutcome::Failed => "audit.failed",
        };
        self.events.lock().expect("event lock").push(event);
        self.records.lock().expect("record lock").push(record);
        Ok(())
    }
}

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("commissioner").with_permission("io.channel.manage")
    } else {
        Actor::new("commissioner")
    };
    RequestContext::new(
        "channel-reconcile-1",
        actor,
        confirmed,
        TimestampMs::new(2_000),
    )
}

#[test]
fn channel_reconciliation_is_high_risk_confirmed_audited_and_non_idempotent() {
    assert_eq!(RECONCILE_CHANNELS_CAPABILITY.name(), "io.channel.reconcile");
    assert_eq!(RECONCILE_CHANNELS_CAPABILITY.kind(), OperationKind::Command);
    assert_eq!(RECONCILE_CHANNELS_CAPABILITY.risk(), RiskLevel::High);
    assert_eq!(
        RECONCILE_CHANNELS_CAPABILITY.required_permission(),
        "io.channel.manage"
    );
    assert_eq!(
        RECONCILE_CHANNELS_CAPABILITY.confirmation(),
        ConfirmationPolicy::Always
    );
    assert_eq!(
        RECONCILE_CHANNELS_CAPABILITY.audit_policy(),
        AuditPolicy::Required
    );
    assert!(!RECONCILE_CHANNELS_CAPABILITY.is_idempotent());
    assert!(capability_catalog().contains(&RECONCILE_CHANNELS_CAPABILITY));
}

#[tokio::test]
async fn reconciliation_is_attempt_audited_before_the_port_and_terminal_audited() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let reconciler = Arc::new(RecordingReconciler::successful(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application =
        ChannelReconciliationApplication::new(reconciler.clone(), audit.clone(), SafetyPolicy);

    let acceptance = application
        .reconcile(&context(true, true), ChannelReconciliationScope::All)
        .await
        .expect("governed reconciliation succeeds");

    assert_eq!(acceptance.scope(), ChannelReconciliationScope::All);
    assert_eq!(acceptance.request_id(), "channel-reconcile-1");
    assert_eq!(acceptance.degraded_count(), 1);
    assert!(acceptance.reconciliation_required());
    assert!(!acceptance.is_retryable());
    assert_eq!(
        *events.lock().expect("event lock"),
        vec!["audit.attempted", "reconcile", "audit.succeeded"]
    );
    let records = audit.records.lock().expect("record lock");
    assert!(records.iter().all(|record| {
        record.capability() == "io.channel.reconcile"
            && !record.detail().unwrap_or_default().contains("protocol")
            && !record.detail().unwrap_or_default().contains("parameters")
    }));
}

#[tokio::test]
async fn denied_unconfirmed_or_unauditable_reconciliation_never_reaches_the_port() {
    for (request_context, fail_audit) in [
        (context(false, true), false),
        (context(true, false), false),
        (context(true, true), true),
    ] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let reconciler = Arc::new(RecordingReconciler::successful(Arc::clone(&events)));
        let audit: Arc<dyn AuditSink> = if fail_audit {
            Arc::new(RecordingAudit::failing_on(Arc::clone(&events), 1))
        } else {
            Arc::new(RecordingAudit::successful(Arc::clone(&events)))
        };
        let application =
            ChannelReconciliationApplication::new(reconciler.clone(), audit, SafetyPolicy);

        let result = application
            .reconcile(&request_context, ChannelReconciliationScope::All)
            .await;

        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::ConfirmationRequired { .. })
                | Err(ApplicationError::AuditUnavailable(_))
        ));
        assert!(reconciler.scopes.lock().expect("scope lock").is_empty());
    }
}

#[tokio::test]
async fn port_failure_is_failed_audited_and_terminal_audit_failure_is_accepted() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::new(RecordingReconciler::failing(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application = ChannelReconciliationApplication::new(failing, audit, SafetyPolicy);
    let result = application
        .reconcile(&context(true, true), ChannelReconciliationScope::All)
        .await;
    assert!(
        matches!(result, Err(ApplicationError::Port(error)) if error.kind() == PortErrorKind::Unavailable)
    );
    assert_eq!(
        *events.lock().expect("event lock"),
        vec!["audit.attempted", "reconcile", "audit.failed"]
    );

    let events = Arc::new(Mutex::new(Vec::new()));
    let reconciler = Arc::new(RecordingReconciler::successful(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::failing_on(Arc::clone(&events), 2));
    let application = ChannelReconciliationApplication::new(reconciler, audit, SafetyPolicy);
    let acceptance = application
        .reconcile(
            &context(true, true),
            ChannelReconciliationScope::One(ChannelId::new(7)),
        )
        .await
        .expect("completed reconciliation remains accepted");
    assert!(!acceptance.completion_audit().is_recorded());
    assert!(!acceptance.is_retryable());
}
