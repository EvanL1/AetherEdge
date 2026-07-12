//! Transport-neutral manual alert-resolution governance.

use std::sync::Arc;

use aether_domain::AlertId;
use aether_ports::{AlertResolver, AuditOutcome, AuditRecord, AuditSink};

use crate::{
    AlertResolutionAcceptance, ApplicationError, RESOLVE_ALERT_CAPABILITY, RequestContext,
    SafetyPolicy,
};

/// Manual alert-resolution facade shared by HTTP, CLI, and embedded transports.
pub struct AlertResolutionApplication {
    resolver: Arc<dyn AlertResolver>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl AlertResolutionApplication {
    /// Creates the facade from alarm storage and audit ports.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn AlertResolver>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            resolver,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, and durably resolves one active alert.
    pub async fn resolve(
        &self,
        context: &RequestContext,
        alert_id: AlertId,
    ) -> Result<AlertResolutionAcceptance, ApplicationError> {
        if let Err(error) = self.policy.authorize(RESOLVE_ALERT_CAPABILITY, context) {
            self.record_audit(
                context,
                alert_id,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        self.record_audit(context, alert_id, AuditOutcome::Attempted, None)
            .await?;
        match self.resolver.resolve(alert_id).await {
            Ok(receipt) => {
                match self
                    .record_audit(context, alert_id, AuditOutcome::Succeeded, None)
                    .await
                {
                    Ok(()) => Ok(AlertResolutionAcceptance::recorded(
                        receipt,
                        context.request_id(),
                    )),
                    Err(ApplicationError::AuditUnavailable(failure)) => {
                        Ok(AlertResolutionAcceptance::audit_incomplete(
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
                    alert_id,
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
        alert_id: AlertId,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let detail = Some(failure.map_or_else(
            || format!("alert_id={}; manual_resolution=true", alert_id.get()),
            |failure| {
                format!(
                    "alert_id={}; manual_resolution=true; {failure}",
                    alert_id.get()
                )
            },
        ));
        self.audit
            .record(AuditRecord::new(
                context.request_id(),
                context.actor().id(),
                RESOLVE_ALERT_CAPABILITY.name(),
                outcome,
                context.timestamp(),
                detail,
            ))
            .await
            .map_err(ApplicationError::AuditUnavailable)
    }
}
