//! Transport-neutral edge use cases.

use std::sync::Arc;

use aether_domain::{CommandId, PointAddress};
use aether_ports::{AuditSink, CommandDispatcher, CommandReceipt, LiveState};

use crate::{
    ApplicationError, ControlApplication, READ_POINT_CAPABILITY, RequestContext, SafetyPolicy,
};

/// Application facade shared by CLI, MCP, and optional network transports.
pub struct EdgeApplication {
    live_state: Arc<dyn LiveState>,
    control: ControlApplication,
    policy: SafetyPolicy,
}

impl EdgeApplication {
    /// Creates an application facade from capability ports.
    #[must_use]
    pub fn new(
        live_state: Arc<dyn LiveState>,
        dispatcher: Arc<dyn CommandDispatcher>,
        audit: Arc<dyn AuditSink>,
        policy: SafetyPolicy,
    ) -> Self {
        Self {
            live_state,
            control: ControlApplication::new(dispatcher, audit, policy),
            policy,
        }
    }

    /// Reads one point after transport-independent authorization.
    pub async fn read_point(
        &self,
        context: &RequestContext,
        address: PointAddress,
    ) -> Result<Option<aether_domain::PointSample>, ApplicationError> {
        self.policy.authorize(READ_POINT_CAPABILITY, context)?;
        self.live_state
            .read(address)
            .await
            .map_err(ApplicationError::Port)
    }

    /// Validates, audits, and dispatches one device-control request.
    pub async fn write_point(
        &self,
        context: &RequestContext,
        command_id: CommandId,
        target: PointAddress,
        value: f64,
    ) -> Result<CommandReceipt, ApplicationError> {
        self.control
            .write_point(context, command_id, target, value)
            .await
    }
}
