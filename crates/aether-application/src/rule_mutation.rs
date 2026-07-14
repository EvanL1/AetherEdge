//! Transport-neutral automation-rule mutation governance.

use std::sync::Arc;

use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, AutomationRuleMutator, RevisionedRuleMutation,
    RuleMutation,
};
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
        self.mutate_inner(context, PendingRuleMutation::Legacy(mutation))
            .await
    }

    /// Authorizes, audits, persists, and activates one revision-fenced rule mutation.
    pub async fn mutate_revisioned(
        &self,
        context: &RequestContext,
        command: RevisionedRuleMutation,
    ) -> Result<RuleMutationAcceptance, ApplicationError> {
        self.mutate_inner(context, PendingRuleMutation::Revisioned(command))
            .await
    }

    async fn mutate_inner(
        &self,
        context: &RequestContext,
        command: PendingRuleMutation,
    ) -> Result<RuleMutationAcceptance, ApplicationError> {
        let mutation = command.mutation();
        let kind = mutation.kind();
        let target = mutation.rule_id();
        let mutation_detail = mutation_audit_detail(mutation, command.expected_revision());
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

        let result = match command {
            PendingRuleMutation::Legacy(mutation) => self.mutator.mutate(mutation).await,
            PendingRuleMutation::Revisioned(command) => {
                self.mutator.mutate_revisioned(command).await
            },
        };
        match result {
            Ok(receipt) => {
                let runtime = receipt.runtime_status();
                let completion_detail = match runtime.failure() {
                    Some(failure) => format!(
                        "{mutation_detail}; resulting_revision={}; scheduler_refresh={}; reconciliation_required={}; refresh_failure={failure}",
                        receipt.resulting_revision().get(),
                        runtime.as_str(),
                        runtime.reconciliation_required()
                    ),
                    None => format!(
                        "{mutation_detail}; resulting_revision={}; scheduler_refresh=refreshed; reconciliation_required=false",
                        receipt.resulting_revision().get()
                    ),
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

enum PendingRuleMutation {
    Legacy(RuleMutation),
    Revisioned(RevisionedRuleMutation),
}

impl PendingRuleMutation {
    const fn mutation(&self) -> &RuleMutation {
        match self {
            Self::Legacy(mutation) => mutation,
            Self::Revisioned(command) => command.mutation(),
        }
    }

    const fn expected_revision(&self) -> Option<u64> {
        match self {
            Self::Legacy(_) => None,
            Self::Revisioned(command) => Some(command.expected_revision().get()),
        }
    }
}

fn mutation_audit_detail(mutation: &RuleMutation, expected_revision: Option<u64>) -> String {
    let expected =
        expected_revision.map_or_else(|| "legacy".to_string(), |value| value.to_string());
    match mutation {
        RuleMutation::Create {
            name, description, ..
        } => format!(
            "expected_revision={expected}; name_sha256={}; description_sha256={}",
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
            format!(
                "expected_revision={expected}; changed_fields={}; {}",
                fields.join(","),
                values.join("; ")
            )
        },
        RuleMutation::SetEnabled { enabled, .. } => {
            format!("expected_revision={expected}; enabled={enabled}")
        },
        RuleMutation::Delete { .. } => {
            format!("expected_revision={expected}; delete=true")
        },
        RuleMutation::Reload => {
            format!("expected_revision={expected}; reload=true")
        },
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
