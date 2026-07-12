use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, AlarmRuleApplication, ApplicationError, RequestContext, SafetyPolicy,
};
use aether_domain::{AlarmRuleId, TimestampMs};
use aether_ports::{
    AlarmRuleMutation, AlarmRuleMutationKind, AlarmRuleMutationReceipt, AlarmRuleMutator,
    AuditOutcome, AuditRecord, AuditSink, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

#[derive(Default)]
struct RecordingMutator {
    mutations: Mutex<Vec<AlarmRuleMutation>>,
}

#[async_trait]
impl AlarmRuleMutator for RecordingMutator {
    async fn mutate(&self, mutation: AlarmRuleMutation) -> PortResult<AlarmRuleMutationReceipt> {
        let kind = mutation.kind();
        let id = mutation.rule_id().unwrap_or_else(|| AlarmRuleId::new(17));
        self.mutations.lock().expect("mutation lock").push(mutation);
        Ok(AlarmRuleMutationReceipt::new(id, kind))
    }
}

struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    fail_on: Option<AuditOutcome>,
}

impl RecordingAudit {
    fn available() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            fail_on: None,
        }
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if self.fail_on == Some(record.outcome()) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "alarm audit unavailable",
            ));
        }
        self.records.lock().expect("audit lock").push(record);
        Ok(())
    }
}

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("operator").with_permission("alarm.rule.manage")
    } else {
        Actor::new("operator")
    };
    RequestContext::new("alarm-rule-1", actor, confirmed, TimestampMs::new(4_000))
}

fn mutation() -> AlarmRuleMutation {
    AlarmRuleMutation::set_enabled(AlarmRuleId::new(9), true)
}

#[tokio::test]
async fn denied_unconfirmed_or_unaudited_alarm_mutation_never_reaches_storage() {
    for (request_context, fail_on) in [
        (context(false, true), None),
        (context(true, false), None),
        (context(true, true), Some(AuditOutcome::Attempted)),
    ] {
        let mutator = Arc::new(RecordingMutator::default());
        let audit = Arc::new(RecordingAudit {
            records: Mutex::new(Vec::new()),
            fail_on,
        });
        let application = AlarmRuleApplication::new(mutator.clone(), audit, SafetyPolicy);

        let result = application.mutate(&request_context, mutation()).await;

        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::ConfirmationRequired { .. })
                | Err(ApplicationError::AuditUnavailable(_))
        ));
        assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
    }
}

#[tokio::test]
async fn accepted_alarm_mutation_is_audited_and_never_retryable() {
    let mutator = Arc::new(RecordingMutator::default());
    let audit = Arc::new(RecordingAudit::available());
    let application = AlarmRuleApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

    let acceptance = application
        .mutate(&context(true, true), mutation())
        .await
        .expect("governed alarm mutation");

    assert_eq!(acceptance.rule_id(), AlarmRuleId::new(9));
    assert_eq!(acceptance.kind(), AlarmRuleMutationKind::Enable);
    assert!(!acceptance.is_retryable());
    assert_eq!(mutator.mutations.lock().expect("mutation lock").len(), 1);
    let outcomes = audit
        .records
        .lock()
        .expect("audit lock")
        .iter()
        .map(AuditRecord::outcome)
        .collect::<Vec<_>>();
    assert_eq!(outcomes, [AuditOutcome::Attempted, AuditOutcome::Succeeded]);
}

#[tokio::test]
async fn terminal_alarm_audit_failure_returns_accepted_non_retryable_outcome_once() {
    let mutator = Arc::new(RecordingMutator::default());
    let audit = Arc::new(RecordingAudit {
        records: Mutex::new(Vec::new()),
        fail_on: Some(AuditOutcome::Succeeded),
    });
    let application = AlarmRuleApplication::new(mutator.clone(), audit, SafetyPolicy);

    let acceptance = application
        .mutate(&context(true, true), mutation())
        .await
        .expect("persistence already accepted the mutation");

    assert_eq!(mutator.mutations.lock().expect("mutation lock").len(), 1);
    assert!(acceptance.completion_audit().failure().is_some());
    assert!(!acceptance.is_retryable());
    assert_eq!(acceptance.request_id(), "alarm-rule-1");
}
