//! Transport-neutral I/O channel commissioning governance.

use std::sync::Arc;

use aether_domain::ChannelId;
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelMutation, ChannelMutator, ChannelParameterValue,
    ChannelParameters, ChannelPatch,
};
use sha2::{Digest, Sha256};

use crate::{
    ApplicationError, ChannelMutationAcceptance, MANAGE_CHANNEL_CAPABILITY, RequestContext,
    SafetyPolicy,
};

/// Channel-management facade shared by every application transport.
pub struct ChannelManagementApplication {
    mutator: Arc<dyn ChannelMutator>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl ChannelManagementApplication {
    /// Creates the facade from its commissioning and audit ports.
    #[must_use]
    pub fn new(
        mutator: Arc<dyn ChannelMutator>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            mutator,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, and applies one I/O channel mutation.
    pub async fn mutate(
        &self,
        context: &RequestContext,
        mutation: ChannelMutation,
    ) -> Result<ChannelMutationAcceptance, ApplicationError> {
        let kind = mutation.kind();
        let channel_id = mutation.channel_id();
        let mutation_detail = mutation_audit_detail(&mutation);

        if let Err(error) = self.policy.authorize(MANAGE_CHANNEL_CAPABILITY, context) {
            self.record_audit(
                context,
                kind.as_str(),
                channel_id,
                &mutation_detail,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        if let Err(error) = validate_mutation(&mutation) {
            self.record_audit(
                context,
                kind.as_str(),
                channel_id,
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
            channel_id,
            &mutation_detail,
            AuditOutcome::Attempted,
            None,
        )
        .await?;

        match self.mutator.mutate(mutation).await {
            Ok(receipt) => {
                let completion_detail = format!(
                    "{mutation_detail}; resulting_channel_id={}; resulting_revision={}; desired_enabled={}; runtime_projection={}; reconciliation_required={}",
                    receipt.channel_id().get(),
                    receipt.resulting_revision().get(),
                    receipt.desired_enabled(),
                    receipt.runtime_projection().as_str(),
                    receipt.reconciliation_required()
                );
                match self
                    .record_audit(
                        context,
                        receipt.kind().as_str(),
                        Some(receipt.channel_id()),
                        &completion_detail,
                        AuditOutcome::Succeeded,
                        None,
                    )
                    .await
                {
                    Ok(()) => Ok(ChannelMutationAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(ChannelMutationAcceptance::audit_incomplete(
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
                    channel_id,
                    &mutation_detail,
                    AuditOutcome::Failed,
                    Some(format!("port_error_kind={:?}", error.kind())),
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
        channel_id: Option<ChannelId>,
        mutation_detail: &str,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let target = channel_id.map_or_else(
            || "channel_id=auto".to_string(),
            |channel_id| format!("channel_id={}", channel_id.get()),
        );
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
            MANAGE_CHANNEL_CAPABILITY.name(),
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

fn mutation_audit_detail(mutation: &ChannelMutation) -> String {
    let expected_revision = mutation.expected_revision().map_or_else(
        || "expected_revision=none".to_string(),
        |revision| format!("expected_revision={}", revision.get()),
    );
    let detail = match mutation {
        ChannelMutation::Create { definition } => format!(
            "changed_fields=name,description,protocol,parameters,logging,enabled; name_sha256={}; description_sha256={}; protocol_sha256={}; protocol_bytes={}; enabled={}",
            digest(definition.name()),
            definition.description().map_or("none".to_string(), digest),
            digest(definition.protocol()),
            definition.protocol().len(),
            definition.enabled()
        ),
        ChannelMutation::Update { patch, .. } => patch_audit_detail(patch),
        ChannelMutation::Delete { .. } => "delete=true".to_string(),
        ChannelMutation::SetEnabled { enabled, .. } => format!("enabled={enabled}"),
    };
    format!("{expected_revision}; {detail}")
}

fn patch_audit_detail(patch: &ChannelPatch) -> String {
    let mut fields = Vec::new();
    let mut values = Vec::new();
    if let Some(name) = patch.name() {
        fields.push("name");
        values.push(format!("name_sha256={}", digest(name)));
    }
    if let Some(description) = patch.description() {
        fields.push("description");
        values.push(format!("description_sha256={}", digest(description)));
    }
    if let Some(protocol) = patch.protocol() {
        fields.push("protocol");
        values.push(format!(
            "protocol_sha256={}; protocol_bytes={}",
            digest(protocol),
            protocol.len()
        ));
    }
    if patch.parameters().is_some() {
        fields.push("parameters");
    }
    if patch.logging().is_some() {
        fields.push("logging");
    }
    let changed_fields = format!("changed_fields={}", fields.join(","));
    if values.is_empty() {
        changed_fields
    } else {
        format!("{changed_fields}; {}", values.join("; "))
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn validate_mutation(mutation: &ChannelMutation) -> Result<(), ApplicationError> {
    if mutation
        .channel_id()
        .is_some_and(|channel_id| channel_id.get() >= 10_000)
    {
        return Err(invalid("channel_id must be less than 10000"));
    }
    if mutation
        .expected_revision()
        .is_some_and(|revision| revision.get() == 0)
    {
        return Err(invalid("expected_revision must be at least 1"));
    }

    match mutation {
        ChannelMutation::Create { definition } => {
            validate_non_blank("name", definition.name())?;
            validate_non_blank("protocol", definition.protocol())?;
            validate_parameters(definition.parameters())
        },
        ChannelMutation::Update { patch, .. } => {
            if patch.is_empty() {
                return Err(invalid("update patch must change at least one field"));
            }
            if let Some(name) = patch.name() {
                validate_non_blank("name", name)?;
            }
            if let Some(protocol) = patch.protocol() {
                validate_non_blank("protocol", protocol)?;
            }
            patch.parameters().map_or(Ok(()), validate_parameters)
        },
        ChannelMutation::Delete { .. } | ChannelMutation::SetEnabled { .. } => Ok(()),
    }
}

fn validate_non_blank(field: &str, value: &str) -> Result<(), ApplicationError> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{field} must not be blank")));
    }
    Ok(())
}

fn validate_parameters(parameters: &ChannelParameters) -> Result<(), ApplicationError> {
    if parameters.values().all(parameter_value_is_finite) {
        Ok(())
    } else {
        Err(invalid("parameters contain a non-finite float"))
    }
}

fn parameter_value_is_finite(value: &ChannelParameterValue) -> bool {
    match value {
        ChannelParameterValue::Float(value) => value.is_finite(),
        ChannelParameterValue::Array(values) => values.iter().all(parameter_value_is_finite),
        ChannelParameterValue::Object(values) => values.values().all(parameter_value_is_finite),
        ChannelParameterValue::Null
        | ChannelParameterValue::Bool(_)
        | ChannelParameterValue::Integer(_)
        | ChannelParameterValue::String(_) => true,
    }
}

fn invalid(reason: impl Into<String>) -> ApplicationError {
    ApplicationError::InvalidChannelMutation(reason.into())
}
