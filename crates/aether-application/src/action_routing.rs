//! Transport-neutral action-routing mutation governance.

use std::sync::Arc;

use aether_ports::{
    ActionRoutingMutation, ActionRoutingTarget, AuditOutcome, AuditRecord, AuditSink,
    AutomationActionRoutingMutator, RevisionedActionRoutingMutation,
};

use crate::{
    ActionRoutingMutationAcceptance, ApplicationError, MANAGE_ROUTING_CAPABILITY, RequestContext,
    SafetyPolicy, context::point_kind_name,
};

/// Action-routing management facade shared by every application transport.
pub struct ActionRoutingApplication {
    mutator: Arc<dyn AutomationActionRoutingMutator>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl ActionRoutingApplication {
    /// Creates the facade from its routing and audit ports.
    #[must_use]
    pub fn new(
        mutator: Arc<dyn AutomationActionRoutingMutator>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            mutator,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, and applies one revision-fenced routing mutation.
    pub async fn mutate_revisioned(
        &self,
        context: &RequestContext,
        command: RevisionedActionRoutingMutation,
    ) -> Result<ActionRoutingMutationAcceptance, ApplicationError> {
        let mutation = command.mutation();
        let kind = mutation.kind();
        let target = mutation.target();
        let mutation_detail = mutation_audit_detail(&mutation, command.expected_revision().get());

        if let Err(error) = self.policy.authorize(MANAGE_ROUTING_CAPABILITY, context) {
            self.record_audit(
                context,
                kind.as_str(),
                target,
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
            target,
            &mutation_detail,
            AuditOutcome::Attempted,
            None,
        )
        .await?;

        let result = self.mutator.mutate_revisioned(command).await;
        match result {
            Ok(receipt) => {
                let runtime = receipt.runtime_status();
                let completion_detail = runtime.failure().map_or_else(
                    || {
                        format!(
                            "{mutation_detail}; affected_routes={}; resulting_revision={}; runtime_status={}; reconciliation_required={}",
                            receipt.affected_routes(),
                            receipt.resulting_revision().get(),
                            runtime.as_str(),
                            runtime.reconciliation_required()
                        )
                    },
                    |failure| {
                        format!(
                            "{mutation_detail}; affected_routes={}; resulting_revision={}; runtime_status={}; reconciliation_required={}; runtime_failure={failure}",
                            receipt.affected_routes(),
                            receipt.resulting_revision().get(),
                            runtime.as_str(),
                            runtime.reconciliation_required()
                        )
                    },
                );
                match self
                    .record_audit(
                        context,
                        receipt.kind().as_str(),
                        receipt.target(),
                        &completion_detail,
                        AuditOutcome::Succeeded,
                        None,
                    )
                    .await
                {
                    Ok(()) => Ok(ActionRoutingMutationAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(ActionRoutingMutationAcceptance::audit_incomplete(
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
                    target,
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
        target: ActionRoutingTarget,
        mutation_detail: &str,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let target_detail = target_audit_detail(target);
        let detail = failure.map_or_else(
            || {
                Some(format!(
                    "operation={operation}; {target_detail}; {mutation_detail}"
                ))
            },
            |failure| {
                Some(format!(
                    "operation={operation}; {target_detail}; {mutation_detail}; {failure}"
                ))
            },
        );
        let record = AuditRecord::new(
            context.request_id(),
            context.actor().id(),
            MANAGE_ROUTING_CAPABILITY.name(),
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

fn target_audit_detail(target: ActionRoutingTarget) -> String {
    match target {
        ActionRoutingTarget::Route(key) => format!(
            "instance_id={}; action_id={}",
            key.instance_id().get(),
            key.action_id().get()
        ),
        ActionRoutingTarget::Instance(instance_id) => {
            format!("instance_id={}; scope=instance_actions", instance_id.get())
        },
        ActionRoutingTarget::Channel(channel_id) => {
            format!("channel_id={}; scope=channel_actions", channel_id.get())
        },
        ActionRoutingTarget::AllActions => "scope=all_actions".to_string(),
    }
}

fn mutation_audit_detail(mutation: &ActionRoutingMutation, expected_revision: u64) -> String {
    let expected = expected_revision.to_string();
    match mutation {
        ActionRoutingMutation::Upsert { route, .. } => {
            let destination = route.destination();
            format!(
                "expected_revision={expected}; destination_channel_id={}; destination_kind={}; destination_point_id={}; enabled={}",
                destination.channel_id().get(),
                point_kind_name(destination.kind()),
                destination.point_id().get(),
                route.enabled()
            )
        },
        ActionRoutingMutation::SetEnabled { enabled, .. } => {
            format!("expected_revision={expected}; enabled={enabled}")
        },
        ActionRoutingMutation::Delete { .. } => {
            format!("expected_revision={expected}; delete=true")
        },
        ActionRoutingMutation::DeleteActionsForInstance { .. } => {
            format!("expected_revision={expected}; delete_scope=instance_actions")
        },
        ActionRoutingMutation::DeleteActionsForChannel { .. } => {
            format!("expected_revision={expected}; delete_scope=channel_actions")
        },
        ActionRoutingMutation::DeleteAllActions => {
            format!("expected_revision={expected}; delete_scope=all_actions")
        },
    }
}
