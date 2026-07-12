//! Logical device-action command boundary used by the rule runtime.
//!
//! The rule engine describes an instance action. The automation composition
//! root decides how that action is authorized, audited, and dispatched.

use aether_domain::{InstanceId, PointAddress, PointId, PointKind};
use aether_ports::{CommandReceipt, PortResult};
use async_trait::async_trait;

/// One logical instance action produced by a deterministic rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuleActionCommand {
    target: PointAddress,
    value: f64,
}

impl RuleActionCommand {
    /// Creates an action target. Command and measurement point kinds are not
    /// representable at this boundary.
    #[must_use]
    pub const fn new(instance_id: InstanceId, point_id: PointId, value: f64) -> Self {
        Self {
            target: PointAddress::new(instance_id, PointKind::Action, point_id),
            value,
        }
    }

    /// Returns the logical instance action address.
    #[must_use]
    pub const fn target(self) -> PointAddress {
        self.target
    }

    /// Returns the requested action value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }
}

/// Transport-neutral command facade injected by the runtime composition root.
#[async_trait]
pub trait RuleActionCommandFacade: Send + Sync + 'static {
    /// Applies one logical action through the host's governed command path.
    async fn write_action(&self, command: RuleActionCommand) -> PortResult<CommandReceipt>;
}
