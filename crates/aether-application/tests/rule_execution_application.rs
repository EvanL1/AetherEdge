use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, ApplicationError, CompletionAuditStatus, RequestContext, RuleExecutionApplication,
    SafetyPolicy,
};
use aether_domain::{RuleId, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, AutomationRuleExecutor, PortError, PortErrorKind,
    PortResult, RuleExecutionReceipt,
};
use async_trait::async_trait;

#[derive(Default)]
struct RecordingRuleExecutor {
    invocations: Mutex<Vec<RuleId>>,
    fail: bool,
}

#[async_trait]
impl AutomationRuleExecutor for RecordingRuleExecutor {
    async fn execute(&self, rule_id: RuleId) -> PortResult<RuleExecutionReceipt> {
        self.invocations
            .lock()
            .expect("rule invocation lock")
            .push(rule_id);
        if self.fail {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "rule scheduler unavailable",
            ));
        }
        Ok(RuleExecutionReceipt::new(
            rule_id,
            TimestampMs::new(2_001),
            2,
            2,
        ))
    }
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    calls: Mutex<usize>,
    fail_on_call: Option<usize>,
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let call = {
            let mut calls = self.calls.lock().expect("audit calls lock");
            *calls += 1;
            *calls
        };
        if self.fail_on_call == Some(call) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit unavailable",
            ));
        }
        self.records.lock().expect("audit record lock").push(record);
        Ok(())
    }
}

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("operator").with_permission("automation.rule.execute")
    } else {
        Actor::new("operator")
    };
    RequestContext::new("rule-request-1", actor, confirmed, TimestampMs::new(2_000))
}

#[tokio::test]
async fn denied_or_unconfirmed_rule_execution_is_audited_without_dispatch() {
    for request_context in [context(false, true), context(true, false)] {
        let executor = Arc::new(RecordingRuleExecutor::default());
        let audit = Arc::new(RecordingAudit::default());
        let application =
            RuleExecutionApplication::new(executor.clone(), audit.clone(), SafetyPolicy);

        let result = application.execute(&request_context, RuleId::new(7)).await;

        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::ConfirmationRequired { .. })
        ));
        assert!(
            executor
                .invocations
                .lock()
                .expect("rule invocation lock")
                .is_empty()
        );
        let records = audit.records.lock().expect("audit record lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome(), AuditOutcome::Rejected);
    }
}

#[tokio::test]
async fn confirmed_rule_execution_is_audited_before_and_after_dispatch() {
    let executor = Arc::new(RecordingRuleExecutor::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = RuleExecutionApplication::new(executor.clone(), audit.clone(), SafetyPolicy);

    let receipt = application
        .execute(&context(true, true), RuleId::new(7))
        .await
        .expect("authorized rule execution succeeds");

    assert_eq!(receipt.rule_id(), RuleId::new(7));
    assert_eq!(receipt.completed_at(), TimestampMs::new(2_001));
    assert_eq!(receipt.actions_attempted(), 2);
    assert_eq!(receipt.actions_succeeded(), 2);
    assert!(receipt.completion_audit().is_recorded());
    assert_eq!(
        executor
            .invocations
            .lock()
            .expect("rule invocation lock")
            .as_slice(),
        &[RuleId::new(7)]
    );
    let outcomes: Vec<_> = audit
        .records
        .lock()
        .expect("audit record lock")
        .iter()
        .map(AuditRecord::outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
    );
    let records = audit.records.lock().expect("audit record lock");
    assert!(
        records[1]
            .detail()
            .is_some_and(|detail| detail.contains("rule_id=7"))
    );
    assert!(
        records[1]
            .detail()
            .is_some_and(|detail| detail.contains("actions_attempted=2"))
    );
    assert!(
        records[1]
            .detail()
            .is_some_and(|detail| detail.contains("actions_succeeded=2"))
    );
}

#[tokio::test]
async fn mandatory_audit_failure_prevents_rule_dispatch() {
    let executor = Arc::new(RecordingRuleExecutor::default());
    let audit = Arc::new(RecordingAudit {
        records: Mutex::new(Vec::new()),
        calls: Mutex::new(0),
        fail_on_call: Some(1),
    });
    let application = RuleExecutionApplication::new(executor.clone(), audit, SafetyPolicy);

    let result = application
        .execute(&context(true, true), RuleId::new(7))
        .await;

    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert!(
        executor
            .invocations
            .lock()
            .expect("rule invocation lock")
            .is_empty()
    );
}

#[tokio::test]
async fn completed_rule_with_final_audit_failure_returns_non_retryable_acceptance_once() {
    let executor = Arc::new(RecordingRuleExecutor::default());
    let audit = Arc::new(RecordingAudit {
        records: Mutex::new(Vec::new()),
        calls: Mutex::new(0),
        fail_on_call: Some(2),
    });
    let application = RuleExecutionApplication::new(executor.clone(), audit.clone(), SafetyPolicy);

    let acceptance = application
        .execute(&context(true, true), RuleId::new(7))
        .await
        .expect("completed rule remains accepted when completion audit is unavailable");

    assert_eq!(acceptance.rule_id(), RuleId::new(7));
    assert_eq!(acceptance.request_id(), "rule-request-1");
    assert!(!acceptance.is_retryable());
    assert!(matches!(
        acceptance.completion_audit(),
        CompletionAuditStatus::Incomplete { .. }
    ));
    assert_eq!(
        executor
            .invocations
            .lock()
            .expect("rule invocation lock")
            .as_slice(),
        &[RuleId::new(7)]
    );
    let records = audit.records.lock().expect("audit record lock");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome(), AuditOutcome::Attempted);
    assert!(
        records[0]
            .detail()
            .is_some_and(|detail| detail.contains("rule_id=7"))
    );
}

#[tokio::test]
async fn failed_rule_dispatch_is_audited_and_preserves_retry_semantics() {
    let executor = Arc::new(RecordingRuleExecutor {
        invocations: Mutex::new(Vec::new()),
        fail: true,
    });
    let audit = Arc::new(RecordingAudit::default());
    let application = RuleExecutionApplication::new(executor, audit.clone(), SafetyPolicy);

    let result = application
        .execute(&context(true, true), RuleId::new(7))
        .await;

    let error = match result {
        Err(ApplicationError::Port(error)) => error,
        other => panic!("expected typed port failure, got {other:?}"),
    };
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    let outcomes: Vec<_> = audit
        .records
        .lock()
        .expect("audit record lock")
        .iter()
        .map(AuditRecord::outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![AuditOutcome::Attempted, AuditOutcome::Failed]
    );
}
