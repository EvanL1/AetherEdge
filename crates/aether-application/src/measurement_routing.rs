//! Transport-neutral measurement-routing mutation governance.

use std::sync::Arc;

use aether_domain::PointKind;
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, AutomationMeasurementRoutingMutator,
    MeasurementRoutingMutation, MeasurementRoutingTarget,
};

use crate::{
    ApplicationError, MANAGE_ROUTING_CAPABILITY, MeasurementRoutingMutationAcceptance,
    RequestContext, SafetyPolicy,
};

/// Authenticated, audited application boundary for logical measurement routes.
pub struct MeasurementRoutingApplication {
    mutator: Arc<dyn AutomationMeasurementRoutingMutator>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl MeasurementRoutingApplication {
    /// Creates the application boundary from routing and audit ports.
    #[must_use]
    pub fn new(
        mutator: Arc<dyn AutomationMeasurementRoutingMutator>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            mutator,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, and applies one revision-fenced mutation.
    pub async fn mutate(
        &self,
        context: &RequestContext,
        mutation: MeasurementRoutingMutation,
    ) -> Result<MeasurementRoutingMutationAcceptance, ApplicationError> {
        let detail = mutation_audit_detail(mutation);
        if let Err(error) = self.policy.authorize(MANAGE_ROUTING_CAPABILITY, context) {
            self.record_audit(
                context,
                mutation,
                &detail,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        self.record_audit(context, mutation, &detail, AuditOutcome::Attempted, None)
            .await?;

        match self.mutator.mutate(mutation).await {
            Ok(receipt) => {
                let runtime = receipt.runtime_status();
                let completion_detail = runtime.failure().map_or_else(
                    || {
                        format!(
                            "{detail}; affected_routes={}; resulting_revision={}; runtime_status={}; reconciliation_required={}",
                            receipt.affected_routes(),
                            receipt.resulting_revision().get(),
                            runtime.as_str(),
                            runtime.reconciliation_required()
                        )
                    },
                    |failure| {
                        format!(
                            "{detail}; affected_routes={}; resulting_revision={}; runtime_status={}; reconciliation_required={}; runtime_failure={failure}",
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
                        mutation,
                        &completion_detail,
                        AuditOutcome::Succeeded,
                        None,
                    )
                    .await
                {
                    Ok(()) => Ok(MeasurementRoutingMutationAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(MeasurementRoutingMutationAcceptance::audit_incomplete(
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
                    mutation,
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
        mutation: MeasurementRoutingMutation,
        detail: &str,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let target = target_audit_detail(mutation.target());
        let detail = failure.map_or_else(
            || {
                Some(format!(
                    "operation={}; {target}; {detail}",
                    mutation.kind().as_str(),
                ))
            },
            |failure| {
                Some(format!(
                    "operation={}; {target}; {detail}; {failure}",
                    mutation.kind().as_str(),
                ))
            },
        );
        self.audit
            .record(AuditRecord::new(
                context.request_id(),
                context.actor().id(),
                MANAGE_ROUTING_CAPABILITY.name(),
                outcome,
                context.timestamp(),
                detail,
            ))
            .await
            .map_err(ApplicationError::AuditUnavailable)
    }
}

fn mutation_audit_detail(mutation: MeasurementRoutingMutation) -> String {
    let expected = mutation.expected_revision().get();
    match mutation {
        MeasurementRoutingMutation::Upsert { route, .. } => {
            let destination = route.destination();
            format!(
                "expected_revision={expected}; destination_channel_id={}; destination_kind={}; destination_point_id={}; enabled={}",
                destination.channel_id().get(),
                point_kind_name(destination.kind()),
                destination.point_id().get(),
                route.enabled()
            )
        },
        MeasurementRoutingMutation::SetEnabled { enabled, .. } => {
            format!("expected_revision={expected}; enabled={enabled}")
        },
        MeasurementRoutingMutation::Delete { .. } => {
            format!("expected_revision={expected}; delete=true")
        },
        MeasurementRoutingMutation::DeleteForInstance { .. } => {
            format!("expected_revision={expected}; delete_scope=instance_measurements")
        },
        MeasurementRoutingMutation::DeleteForChannel { .. } => {
            format!("expected_revision={expected}; delete_scope=channel_measurements")
        },
        MeasurementRoutingMutation::DeleteAll { .. } => {
            format!("expected_revision={expected}; delete_scope=all_measurements")
        },
    }
}

fn target_audit_detail(target: MeasurementRoutingTarget) -> String {
    match target {
        MeasurementRoutingTarget::Route(key) => format!(
            "instance_id={}; measurement_id={}",
            key.instance_id().get(),
            key.measurement_id().get()
        ),
        MeasurementRoutingTarget::Instance(instance_id) => {
            format!(
                "instance_id={}; scope=instance_measurements",
                instance_id.get()
            )
        },
        MeasurementRoutingTarget::Channel(channel_id) => {
            format!(
                "channel_id={}; scope=channel_measurements",
                channel_id.get()
            )
        },
        MeasurementRoutingTarget::AllMeasurements => "scope=all_measurements".to_string(),
    }
}

const fn point_kind_name(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Telemetry => "telemetry",
        PointKind::Status => "status",
        PointKind::Command => "command",
        PointKind::Action => "action",
    }
}
