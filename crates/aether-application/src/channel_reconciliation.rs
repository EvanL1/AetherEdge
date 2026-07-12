//! Transport-neutral governance for I/O channel runtime reconciliation.

use std::sync::Arc;

use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelReconciler, ChannelReconciliationReceipt,
    ChannelReconciliationScope,
};

use crate::{
    ApplicationError, ChannelReconciliationAcceptance, RECONCILE_CHANNELS_CAPABILITY,
    RequestContext, SafetyPolicy,
};

/// Channel runtime-reconciliation facade shared by every application transport.
pub struct ChannelReconciliationApplication {
    reconciler: Arc<dyn ChannelReconciler>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl ChannelReconciliationApplication {
    /// Creates the facade from its runtime-reconciliation and audit ports.
    #[must_use]
    pub fn new(
        reconciler: Arc<dyn ChannelReconciler>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            reconciler,
            audit,
            policy,
        }
    }

    /// Authorizes, audits, and reconciles the selected runtime scope.
    pub async fn reconcile(
        &self,
        context: &RequestContext,
        scope: ChannelReconciliationScope,
    ) -> Result<ChannelReconciliationAcceptance, ApplicationError> {
        let scope_detail = scope_audit_detail(scope);

        if let Err(error) = self
            .policy
            .authorize(RECONCILE_CHANNELS_CAPABILITY, context)
        {
            self.record_audit(
                context,
                &scope_detail,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }

        self.record_audit(context, &scope_detail, AuditOutcome::Attempted, None)
            .await?;

        match self.reconciler.reconcile(scope).await {
            Ok(receipt) => self.accept_success(context, scope_detail, receipt).await,
            Err(error) => {
                self.record_audit(
                    context,
                    &scope_detail,
                    AuditOutcome::Failed,
                    Some(format!("port_error_kind={:?}", error.kind())),
                )
                .await?;
                Err(ApplicationError::Port(error))
            },
        }
    }

    async fn accept_success(
        &self,
        context: &RequestContext,
        scope_detail: String,
        receipt: ChannelReconciliationReceipt,
    ) -> Result<ChannelReconciliationAcceptance, ApplicationError> {
        let completion_detail = format!(
            "{scope_detail}; item_count={}; degraded_count={}; reconciliation_required={}",
            receipt.items().len(),
            receipt.degraded_count(),
            receipt.reconciliation_required()
        );
        match self
            .record_audit(context, &completion_detail, AuditOutcome::Succeeded, None)
            .await
        {
            Ok(()) => Ok(ChannelReconciliationAcceptance::recorded(
                receipt,
                context.request_id(),
            )),
            Err(ApplicationError::AuditUnavailable(failure)) => {
                Ok(ChannelReconciliationAcceptance::audit_incomplete(
                    receipt,
                    context.request_id(),
                    failure,
                ))
            },
            Err(error) => Err(error),
        }
    }

    async fn record_audit(
        &self,
        context: &RequestContext,
        scope_detail: &str,
        outcome: AuditOutcome,
        failure: Option<String>,
    ) -> Result<(), ApplicationError> {
        let detail = Some(failure.map_or_else(
            || scope_detail.to_string(),
            |failure| format!("{scope_detail}; {failure}"),
        ));
        let record = AuditRecord::new(
            context.request_id(),
            context.actor().id(),
            RECONCILE_CHANNELS_CAPABILITY.name(),
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

fn scope_audit_detail(scope: ChannelReconciliationScope) -> String {
    match scope {
        ChannelReconciliationScope::All => "scope=all".to_string(),
        ChannelReconciliationScope::One(channel_id) => {
            format!("scope=one; channel_id={}", channel_id.get())
        },
    }
}
