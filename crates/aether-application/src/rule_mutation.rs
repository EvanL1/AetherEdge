//! Transport-neutral automation-rule mutation governance.

use std::sync::Arc;

use aether_ports::{AuditOutcome, AuditRecord, AuditSink, AutomationRuleMutator, RuleMutation};
use sha2::{Digest, Sha256};

use crate::{
    ApplicationError, MANAGE_RULE_CAPABILITY, RequestContext, RuleMutationAcceptance, SafetyPolicy,
};

/// Rule-management facade shared by HTTP, CLI, MCP, and embedded transports.
pub struct RuleMutationApplication {
    mutator: Arc<dyn AutomationRuleMutator>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl RuleMutationApplication {
    /// Creates the facade from its persistence/runtime and audit ports.
    #[must_use]
    pub fn new(
        mutator: Arc<dyn AutomationRuleMutator>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            mutator,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, persists, and activates one rule mutation.
    pub async fn mutate(
        &self,
        context: &RequestContext,
        mutation: RuleMutation,
    ) -> Result<RuleMutationAcceptance, ApplicationError> {
        let kind = mutation.kind();
        let target = mutation.rule_id();
        let mutation_detail = mutation_audit_detail(&mutation);
        if let Err(error) = self.policy.authorize(MANAGE_RULE_CAPABILITY, context) {
            self.record_audit(
                context,
                kind.as_str(),
                target.map(aether_domain::RuleId::get),
                &mutation_detail,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        self.record_audit(
            context,
            kind.as_str(),
            target.map(aether_domain::RuleId::get),
            &mutation_detail,
            AuditOutcome::Attempted,
            None,
        )
        .await?;

        match self.mutator.mutate(mutation).await {
            Ok(receipt) => {
                let completion_detail = match receipt.scheduler_refresh().failure() {
                    Some(failure) => format!(
                        "{mutation_detail}; scheduler_refresh=stopped; refresh_failure={failure}"
                    ),
                    None => format!("{mutation_detail}; scheduler_refresh=refreshed"),
                };
                match self
                    .record_audit(
                        context,
                        receipt.kind().as_str(),
                        receipt.rule_id().map(aether_domain::RuleId::get),
                        &completion_detail,
                        AuditOutcome::Succeeded,
                        None,
                    )
                    .await
                {
                    Ok(()) => Ok(RuleMutationAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(RuleMutationAcceptance::audit_incomplete(
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
                    kind.as_str(),
                    target.map(aether_domain::RuleId::get),
                    &mutation_detail,
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
        operation: &str,
        rule_id: Option<u64>,
        mutation_detail: &str,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let target =
            rule_id.map_or_else(|| "rule_id=new".to_string(), |id| format!("rule_id={id}"));
        let detail = failure.map_or_else(
            || {
                Some(format!(
                    "operation={operation}; {target}; {mutation_detail}"
                ))
            },
            |failure| {
                Some(format!(
                    "operation={operation}; {target}; {mutation_detail}; {failure}"
                ))
            },
        );
        let record = AuditRecord::new(
            context.request_id(),
            context.actor().id(),
            MANAGE_RULE_CAPABILITY.name(),
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

fn mutation_audit_detail(mutation: &RuleMutation) -> String {
    match mutation {
        RuleMutation::Create { name, description } => format!(
            "name_sha256={}; description_sha256={}",
            digest(name),
            description.as_deref().map_or("none".to_string(), digest)
        ),
        RuleMutation::Update {
            name,
            description,
            enabled,
            priority,
            cooldown_ms,
            flow_json,
            trigger_config,
            ..
        } => {
            let mut fields = Vec::new();
            let mut values = Vec::new();
            if let Some(name) = name {
                fields.push("name");
                values.push(format!("name_sha256={}", digest(name)));
            }
            if let Some(description) = description {
                fields.push("description");
                values.push(format!("description_sha256={}", digest(description)));
            }
            if let Some(enabled) = enabled {
                fields.push("enabled");
                values.push(format!("enabled={enabled}"));
            }
            if let Some(priority) = priority {
                fields.push("priority");
                values.push(format!("priority={priority}"));
            }
            if let Some(cooldown_ms) = cooldown_ms {
                fields.push("cooldown_ms");
                values.push(format!("cooldown_ms={cooldown_ms}"));
            }
            if let Some(flow_json) = flow_json {
                fields.push("flow_json");
                values.push(format!("flow_sha256={}", digest(flow_json)));
            }
            if let Some(trigger_config) = trigger_config {
                fields.push("trigger_config");
                values.push(format!("trigger_sha256={}", digest(trigger_config)));
            }
            format!("changed_fields={}; {}", fields.join(","), values.join("; "))
        },
        RuleMutation::SetEnabled { enabled, .. } => format!("enabled={enabled}"),
        RuleMutation::Delete { .. } => "delete=true".to_string(),
        RuleMutation::Reload => "reload=true".to_string(),
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
