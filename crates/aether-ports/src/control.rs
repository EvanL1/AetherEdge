//! Device-control dispatch capability.

use aether_domain::{CommandId, ControlCommand, PhysicalDeviceCommand, TimestampMs};
use async_trait::async_trait;

use crate::PortResult;

/// Acceptance information from the local command plane.
///
/// This receipt does not assert that a physical device executed or
/// acknowledged the command. The legacy `completed_at` field name is retained
/// for API compatibility and means "accepted by the local command transport";
/// it can be renamed when the public response contract is versioned.
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

    /// Returns the accepted command's correlation identifier.
    #[must_use]
    pub const fn command_id(self) -> CommandId {
        self.command_id
    }

    /// Returns when the local command transport accepted the command.
    #[must_use]
    pub const fn completed_at(self) -> TimestampMs {
        self.completed_at
    }
}

/// Routes a validated command to the responsible local device-command plane.
#[async_trait]
pub trait CommandDispatcher: Send + Sync + 'static {
    /// Dispatches a command or reports a typed recoverable/permanent failure.
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt>;
}

/// Delivers an already-routed command to the physical device data plane.
///
/// Implementations return success only after the IO transport notification is
/// written; mirroring a value into SHM alone is not acceptance. Success is not
/// a physical-device acknowledgement.
#[async_trait]
pub trait DeviceCommandSink: Send + Sync + 'static {
    /// Writes and signals one physical command or reports a typed failure.
    async fn send(&self, command: PhysicalDeviceCommand) -> PortResult<CommandReceipt>;
}
