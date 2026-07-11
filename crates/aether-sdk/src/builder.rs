//! Explicit SDK composition without infrastructure defaults.

use std::sync::Arc;

use aether_application::{EdgeApplication, SafetyPolicy};
use aether_ports::{AuditSink, CommandDispatcher, LiveState};
use thiserror::Error;

/// Error returned when a required SDK port was not supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BuildError {
    /// A required capability port is missing.
    #[error("required port {0} was not configured")]
    MissingPort(&'static str),
}

/// Builder for the transport-neutral Aether application facade.
///
/// The builder has no concrete storage or network defaults. Embedders select
/// adapters explicitly, which keeps external databases out of the SDK graph.
#[derive(Default)]
pub struct AetherBuilder {
    live_state: Option<Arc<dyn LiveState>>,
    dispatcher: Option<Arc<dyn CommandDispatcher>>,
    audit: Option<Arc<dyn AuditSink>>,
    policy: SafetyPolicy,
}

impl AetherBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects the authoritative live-state adapter.
    #[must_use]
    pub fn with_live_state<T>(mut self, live_state: Arc<T>) -> Self
    where
        T: LiveState,
    {
        self.live_state = Some(live_state);
        self
    }

    /// Selects the device-control dispatcher.
    #[must_use]
    pub fn with_command_dispatcher<T>(mut self, dispatcher: Arc<T>) -> Self
    where
        T: CommandDispatcher,
    {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Selects the mandatory audit destination.
    #[must_use]
    pub fn with_audit_sink<T>(mut self, audit: Arc<T>) -> Self
    where
        T: AuditSink,
    {
        self.audit = Some(audit);
        self
    }

    /// Replaces the default deny-by-default safety policy.
    #[must_use]
    pub const fn with_safety_policy(mut self, policy: SafetyPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Builds the application facade after checking all mandatory ports.
    pub fn build(self) -> Result<EdgeApplication, BuildError> {
        let live_state = self
            .live_state
            .ok_or(BuildError::MissingPort("live_state"))?;
        let dispatcher = self
            .dispatcher
            .ok_or(BuildError::MissingPort("command_dispatcher"))?;
        let audit = self.audit.ok_or(BuildError::MissingPort("audit_sink"))?;

        Ok(EdgeApplication::new(
            live_state,
            dispatcher,
            audit,
            self.policy,
        ))
    }
}
