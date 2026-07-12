//! Transport-neutral manual automation-rule execution.

use std::sync::Arc;

use aether_domain::RuleId;
use aether_ports::{AuditOutcome, AuditRecord, AuditSink, AutomationRuleExecutor};

use crate::{
    ApplicationError, EXECUTE_RULE_CAPABILITY, RequestContext, RuleExecutionAcceptance,
    SafetyPolicy,
};

/// Manual rule-execution facade shared by HTTP, CLI, and MCP transports.
pub struct RuleExecutionApplication {
    executor: Arc<dyn AutomationRuleExecutor>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl RuleExecutionApplication {
    /// Creates the facade from its deterministic runtime and audit ports.
    #[must_use]
    pub fn new(
        executor: Arc<dyn AutomationRuleExecutor>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            executor,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, and executes one commissioned rule.
    pub async fn execute(
        &self,
        context: &RequestContext,
        rule_id: RuleId,
    ) -> Result<RuleExecutionAcceptance, ApplicationError> {
        if let Err(error) = self.policy.authorize(EXECUTE_RULE_CAPABILITY, context) {
            self.record_audit(
                context,
                rule_id,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        self.record_audit(context, rule_id, AuditOutcome::Attempted, None)
            .await?;
        match self.executor.execute(rule_id).await {
            Ok(receipt) => {
                let detail = format!(
                    "actions_attempted={}; actions_succeeded={}",
                    receipt.actions_attempted(),
                    receipt.actions_succeeded()
                );
                match self
                    .record_audit(context, rule_id, AuditOutcome::Succeeded, Some(detail))
                    .await
                {
                    Ok(()) => Ok(RuleExecutionAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(RuleExecutionAcceptance::audit_incomplete(
                            receipt,
                            context.request_id(),
                            failure,
                        ))
                    },
                    Err(error) => Err(error),
                }
            },
            Err(error) => {
                self.record_audit(
                    context,
                    rule_id,
                    AuditOutcome::Failed,
                    Some(error.to_string()),
                )
                .await?;
                Err(ApplicationError::Port(error))
            },
        }
    }

    async fn record_audit(
        &self,
        context: &RequestContext,
        rule_id: RuleId,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let detail = failure.map_or_else(
            || Some(format!("rule_id={}", rule_id.get())),
            |failure| Some(format!("rule_id={}; {failure}", rule_id.get())),
        );
        let record = AuditRecord::new(
            context.request_id(),
            context.actor().id(),
            EXECUTE_RULE_CAPABILITY.name(),
            outcome,
            context.timestamp(),
            detail,
        );
        self.audit
            .record(record)
            .await
            .map_err(ApplicationError::AuditUnavailable)
    }
}
