//! Device-control dispatch capability.

use aether_domain::{CommandId, ControlCommand, TimestampMs};
use async_trait::async_trait;

use crate::PortResult;

/// Successful completion information for a device command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandReceipt {
    command_id: CommandId,
    completed_at: TimestampMs,
}

impl CommandReceipt {
    /// Creates a command receipt.
    #[must_use]
    pub const fn new(command_id: CommandId, completed_at: TimestampMs) -> Self {
        Self {
            command_id,
            completed_at,
        }
    }

    /// Returns the completed command identifier.
    #[must_use]
    pub const fn command_id(self) -> CommandId {
        self.command_id
    }

    /// Returns the completion timestamp.
    #[must_use]
    pub const fn completed_at(self) -> TimestampMs {
        self.completed_at
    }
}

/// Routes a validated command to the responsible device driver.
#[async_trait]
pub trait CommandDispatcher: Send + Sync + 'static {
    /// Dispatches a command or reports a typed recoverable/permanent failure.
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt>;
}
