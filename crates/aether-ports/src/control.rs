//! Device-control dispatch capability.

use aether_domain::{CommandId, ControlCommand, PhysicalDeviceCommand, TimestampMs};
use async_trait::async_trait;

use crate::PortResult;

/// Expected service-local topology publication for one derived command.
///
/// The token is deliberately opaque to the rule engine. A dispatcher that owns
/// the corresponding runtime topology must compare it after pinning its command
/// generation and before resolving or sending the physical command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandTopologyFence {
    expected_sequence: u64,
}

impl CommandTopologyFence {
    /// Creates a fence from the publication sequence captured before evaluation.
    #[must_use]
    pub const fn new(expected_sequence: u64) -> Self {
        Self { expected_sequence }
    }

    /// Returns the exact publication sequence required by the derived command.
    #[must_use]
    pub const fn expected_sequence(self) -> u64 {
        self.expected_sequence
    }
}

/// Acceptance information from the local command plane.
///
/// This receipt does not assert that a physical device executed or
/// acknowledged the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandReceipt {
    command_id: CommandId,
    accepted_at: TimestampMs,
}

impl CommandReceipt {
    /// Creates a command receipt.
    #[must_use]
    pub const fn new(command_id: CommandId, accepted_at: TimestampMs) -> Self {
        Self {
            command_id,
            accepted_at,
        }
    }

    /// Returns the accepted command's correlation identifier.
    #[must_use]
    pub const fn command_id(self) -> CommandId {
        self.command_id
    }

    /// Returns when the local command transport accepted the command.
    #[must_use]
    pub const fn accepted_at(self) -> TimestampMs {
        self.accepted_at
    }
}

/// Routes a validated command to the responsible local device-command plane.
#[async_trait]
pub trait CommandDispatcher: Send + Sync + 'static {
    /// Dispatches a command or reports a typed recoverable/permanent failure.
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt>;

    /// Dispatches a command only if the dispatcher can pin the expected topology.
    ///
    /// Implementations must handle this explicitly so a derived rule command
    /// can never silently lose its generation fence.
    async fn dispatch_fenced(
        &self,
        command: ControlCommand,
        fence: CommandTopologyFence,
    ) -> PortResult<CommandReceipt>;
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
