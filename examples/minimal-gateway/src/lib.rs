//! Minimal no-external-service gateway composition.

use std::sync::Arc;

use aether_sdk::application::{EdgeApplication, SafetyPolicy};
use aether_sdk::domain::{ControlCommand, PointSample};
use aether_sdk::ports::{
    CommandDispatcher, CommandReceipt, LiveStateWriter, PortError, PortErrorKind, PortResult,
};
use aether_sdk::{AetherBuilder, BuildError};
use aether_store_local::{MemoryAuditSink, MemoryLiveState};
use async_trait::async_trait;

struct NoDeviceDispatcher;

#[async_trait]
impl CommandDispatcher for NoDeviceDispatcher {
    async fn dispatch(&self, _command: ControlCommand) -> PortResult<CommandReceipt> {
        Err(PortError::new(
            PortErrorKind::Rejected,
            "no device driver is configured in the minimal gateway",
        ))
    }
}

/// Smallest complete Aether composition with no external services.
pub struct MinimalGateway {
    application: EdgeApplication,
    live_state: Arc<MemoryLiveState>,
    audit: Arc<MemoryAuditSink>,
}

impl MinimalGateway {
    /// Builds the local-only gateway.
    pub fn new() -> Result<Self, BuildError> {
        let live_state = Arc::new(MemoryLiveState::new());
        let audit = Arc::new(MemoryAuditSink::new());
        let application = AetherBuilder::new()
            .with_live_state(Arc::clone(&live_state))
            .with_command_dispatcher(Arc::new(NoDeviceDispatcher))
            .with_audit_sink(Arc::clone(&audit))
            .with_safety_policy(SafetyPolicy)
            .build()?;

        Ok(Self {
            application,
            live_state,
            audit,
        })
    }

    /// Returns the shared application API used by every interface.
    #[must_use]
    pub const fn application(&self) -> &EdgeApplication {
        &self.application
    }

    /// Publishes a sample as the acquisition owner.
    pub async fn publish(&self, sample: PointSample) -> PortResult<()> {
        self.live_state.write(sample).await
    }

    /// Returns the local audit snapshot.
    pub fn audit_records(&self) -> PortResult<Vec<aether_sdk::ports::AuditRecord>> {
        self.audit.records()
    }
}
