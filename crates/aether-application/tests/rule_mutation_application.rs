use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, ApplicationError, RequestContext, RuleMutationApplication, SafetyPolicy,
};
use aether_domain::{RuleId, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, AutomationRuleMutator, AutomationRulesRevision,
    PortError, PortErrorKind, PortResult, RevisionedRuleMutation, RuleMutation, RuleMutationKind,
    RuleMutationReceipt,
};
use async_trait::async_trait;

#[derive(Default)]
struct RecordingMutator {
    mutations: Mutex<Vec<RevisionedRuleMutation>>,
}

#[async_trait]
impl AutomationRuleMutator for RecordingMutator {
    async fn mutate(&self, mutation: RuleMutation) -> PortResult<RuleMutationReceipt> {
        Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("unexpected legacy mutation: {}", mutation.kind().as_str()),
        ))
    }

    async fn mutate_revisioned(
        &self,
        mutation: RevisionedRuleMutation,
    ) -> PortResult<RuleMutationReceipt> {
        let kind = mutation.kind();
        let rule_id = mutation.rule_id().unwrap_or_else(|| RuleId::new(17));
        self.mutations.lock().expect("mutation lock").push(mutation);
        Ok(RuleMutationReceipt::new_at_revision(
            rule_id,
            kind,
            AutomationRulesRevision::new(9),
        ))
    }
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    fail: bool,
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if self.fail {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit unavailable",
            ));
        }
        self.records.lock().expect("audit lock").push(record);
        Ok(())
    }
}

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("operator").with_permission("automation.rule.manage")
    } else {
        Actor::new("operator")
    };
    RequestContext::new("rule-mutation-1", actor, confirmed, TimestampMs::new(2_000))
}

fn mutation() -> RevisionedRuleMutation {
    RevisionedRuleMutation::set_enabled(RuleId::new(7), true, AutomationRulesRevision::new(8))
}

#[tokio::test]
async fn denied_or_unconfirmed_mutation_is_audited_without_storage_or_reload() {
    for request_context in [context(false, true), context(true, false)] {
        let mutator = Arc::new(RecordingMutator::default());
        let audit = Arc::new(RecordingAudit::default());
        let application =
            RuleMutationApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

        let result = application
            .mutate_revisioned(&request_context, mutation())
            .await;

        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::ConfirmationRequired { .. })
        ));
        assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
        let records = audit.records.lock().expect("audit lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome(), AuditOutcome::Rejected);
    }
}

#[tokio::test]
async fn mandatory_attempt_audit_failure_prevents_storage_and_reload() {
    let mutator = Arc::new(RecordingMutator::default());
    let audit = Arc::new(RecordingAudit {
        records: Mutex::new(Vec::new()),
        fail: true,
    });
    let application = RuleMutationApplication::new(mutator.clone(), audit, SafetyPolicy);

    let result = application
        .mutate_revisioned(&context(true, true), mutation())
        .await;

    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
}

#[tokio::test]
async fn confirmed_mutation_is_audited_before_and_after_the_single_mutation_port() {
    let mutator = Arc::new(RecordingMutator::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = RuleMutationApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

    let receipt = application
        .mutate_revisioned(&context(true, true), mutation())
        .await
        .expect("governed rule mutation succeeds");

    assert_eq!(receipt.rule_id(), Some(RuleId::new(7)));
    assert_eq!(receipt.kind(), RuleMutationKind::Enable);
    assert_eq!(
        receipt.resulting_revision(),
        AutomationRulesRevision::new(9)
    );
    assert_eq!(mutator.mutations.lock().expect("mutation lock").len(), 1);
    let outcomes: Vec<_> = audit
        .records
        .lock()
        .expect("audit lock")
        .iter()
        .map(AuditRecord::outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
    );
    let records = audit.records.lock().expect("audit lock");
    assert!(
        records[0]
            .detail()
            .expect("attempt audit detail")
            .contains("expected_revision=8")
    );
    assert!(
        records[1]
            .detail()
            .expect("completion audit detail")
            .contains("resulting_revision=9")
    );
}
