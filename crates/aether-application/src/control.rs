//! Transport-neutral device-control use cases.

use std::sync::Arc;

use aether_domain::{CommandId, ControlCommand, PointAddress};
use aether_ports::{AuditOutcome, AuditRecord, AuditSink, CommandDispatcher, CommandReceipt};

use crate::{
    ApplicationError, DEFAULT_COMMAND_TTL_MS, RequestContext, SafetyPolicy, WRITE_POINT_CAPABILITY,
};

/// Device-control facade shared by HTTP, CLI, MCP, and embedded hosts.
///
/// Unlike [`crate::EdgeApplication`], this facade does not require a live-state
/// port, so a process that owns only command routing cannot manufacture a fake
/// read path merely to obtain authorization and auditing.
pub struct ControlApplication {
    dispatcher: Arc<dyn CommandDispatcher>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl ControlApplication {
    /// Creates a control facade from its mandatory ports.
    #[must_use]
    pub fn new(
        dispatcher: Arc<dyn CommandDispatcher>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            dispatcher,
            audit,
            policy,
        }
    }

    /// Authorizes, validates, audits, and dispatches one device command.
    pub async fn write_point(
        &self,
        context: &RequestContext,
        command_id: CommandId,
        target: PointAddress,
        value: f64,
    ) -> Result<CommandReceipt, ApplicationError> {
        if let Err(error) = self.policy.authorize(WRITE_POINT_CAPABILITY, context) {
            self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                .await?;
            return Err(error);
        }

        let expires_at = aether_domain::TimestampMs::new(
            context
                .timestamp()
                .get()
                .saturating_add(DEFAULT_COMMAND_TTL_MS),
        );
        let command =
            match ControlCommand::new(command_id, target, value, context.timestamp(), expires_at) {
                Ok(command) => command,
                Err(error) => {
                    self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                        .await?;
                    return Err(ApplicationError::InvalidCommand(error));
                },
            };

        self.record_audit(context, AuditOutcome::Attempted, None)
            .await?;

        match self.dispatcher.dispatch(command).await {
            Ok(receipt) => {
                self.record_audit(context, AuditOutcome::Succeeded, None)
                    .await?;
                Ok(receipt)
            },
            Err(error) => {
                self.record_audit(context, AuditOutcome::Failed, Some(error.to_string()))
                    .await?;
                Err(ApplicationError::Port(error))
            },
        }
    }

    async fn record_audit(
        &self,
        context: &RequestContext,
        outcome: AuditOutcome,
        detail: Option<String>,
    ) -> Result<(), ApplicationError> {
        let record = AuditRecord::new(
            context.request_id(),
            context.actor().id(),
            WRITE_POINT_CAPABILITY.name(),
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
