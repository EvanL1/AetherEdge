//! Transport-neutral alarm-rule mutation governance.

use std::sync::Arc;

use aether_ports::{AlarmRuleMutation, AlarmRuleMutator, AuditOutcome, AuditRecord, AuditSink};
use sha2::{Digest, Sha256};

use crate::{
    AlarmRuleMutationAcceptance, ApplicationError, MANAGE_ALARM_RULE_CAPABILITY, RequestContext,
    SafetyPolicy,
};

/// Alarm rule facade shared by HTTP, CLI, MCP, and embedded transports.
pub struct AlarmRuleApplication {
    mutator: Arc<dyn AlarmRuleMutator>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl AlarmRuleApplication {
    /// Creates the facade from persistence/runtime and audit ports.
    #[must_use]
    pub fn new(
        mutator: Arc<dyn AlarmRuleMutator>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            mutator,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, persists, and reconciles one alarm-rule mutation.
    pub async fn mutate(
        &self,
        context: &RequestContext,
        mutation: AlarmRuleMutation,
    ) -> Result<AlarmRuleMutationAcceptance, ApplicationError> {
        let kind = mutation.kind();
        let target = mutation.rule_id();
        let detail = mutation_audit_detail(&mutation);

        if let Err(error) = self.policy.authorize(MANAGE_ALARM_RULE_CAPABILITY, context) {
            self.record_audit(
                context,
                kind.as_str(),
                target.map(aether_domain::AlarmRuleId::get),
                &detail,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        self.record_audit(
            context,
            kind.as_str(),
            target.map(aether_domain::AlarmRuleId::get),
            &detail,
            AuditOutcome::Attempted,
            None,
        )
        .await?;

        match self.mutator.mutate(mutation).await {
            Ok(receipt) => {
                match self
                    .record_audit(
                        context,
                        receipt.kind().as_str(),
                        Some(receipt.rule_id().get()),
                        &detail,
                        AuditOutcome::Succeeded,
                        None,
                    )
                    .await
                {
                    Ok(()) => Ok(AlarmRuleMutationAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(AlarmRuleMutationAcceptance::audit_incomplete(
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
                    target.map(aether_domain::AlarmRuleId::get),
                    &detail,
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
        let target = rule_id.map_or_else(
            || "alarm_rule_id=new".to_string(),
            |id| format!("alarm_rule_id={id}"),
        );
        let detail = Some(failure.map_or_else(
            || format!("operation={operation}; {target}; {mutation_detail}"),
            |failure| format!("operation={operation}; {target}; {mutation_detail}; {failure}"),
        ));
        self.audit
            .record(AuditRecord::new(
                context.request_id(),
                context.actor().id(),
                MANAGE_ALARM_RULE_CAPABILITY.name(),
                outcome,
                context.timestamp(),
                detail,
            ))
            .await
            .map_err(ApplicationError::AuditUnavailable)
    }
}

fn mutation_audit_detail(mutation: &AlarmRuleMutation) -> String {
    match mutation {
        AlarmRuleMutation::Create { definition } => format!(
            "name_sha256={}; {}; severity={}; comparator={}; threshold_sha256={}; enabled={}; description_sha256={}",
            digest(definition.name()),
            definition.target_label(),
            definition.severity().get(),
            definition.comparator().as_str(),
            digest(&definition.threshold().to_string()),
            definition.enabled(),
            definition
                .description()
                .map_or_else(|| "none".to_string(), digest)
        ),
        AlarmRuleMutation::Update { patch, .. } => {
            let mut fields = Vec::new();
            let mut values = Vec::new();
            if patch.target().is_some() {
                fields.push("target");
            }
            if let Some(name) = patch.name() {
                fields.push("name");
                values.push(format!("name_sha256={}", digest(name)));
            }
            if let Some(severity) = patch.severity() {
                fields.push("severity");
                values.push(format!("severity={}", severity.get()));
            }
            if let Some(comparator) = patch.comparator() {
                fields.push("comparator");
                values.push(format!("comparator={}", comparator.as_str()));
            }
            if let Some(threshold) = patch.threshold() {
                fields.push("threshold");
                values.push(format!(
                    "threshold_sha256={}",
                    digest(&threshold.to_string())
                ));
            }
            if let Some(enabled) = patch.enabled() {
                fields.push("enabled");
                values.push(format!("enabled={enabled}"));
            }
            if let Some(description) = patch.description() {
                fields.push("description");
                values.push(format!(
                    "description_sha256={}",
                    description.map_or_else(|| "none".to_string(), digest)
                ));
            }
            format!("changed_fields={}; {}", fields.join(","), values.join("; "))
        },
        AlarmRuleMutation::SetEnabled { enabled, .. } => format!("enabled={enabled}"),
        AlarmRuleMutation::Delete { .. } => "delete=true".to_string(),
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
