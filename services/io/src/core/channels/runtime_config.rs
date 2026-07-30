//! Immutable channel configuration consumed by one protocol runtime.

use crate::core::config::{
    AdjustmentPoint, ChannelConfig, ControlPoint, SignalPoint, TelemetryPoint,
};
use aether_ports::ChannelRevision;

/// One channel row and its complete, transactionally loaded point topology.
///
/// Point identifiers are unique only within a point type. Runtime consumers
/// must keep the four typed collections separate.
#[derive(Debug)]
pub struct RuntimeChannelConfig {
    pub(crate) base: ChannelConfig,
    persisted_revision: Option<ChannelRevision>,
    pub(crate) telemetry_points: Vec<TelemetryPoint>,
    pub(crate) signal_points: Vec<SignalPoint>,
    pub(crate) control_points: Vec<ControlPoint>,
    pub(crate) adjustment_points: Vec<AdjustmentPoint>,
}

impl RuntimeChannelConfig {
    pub(crate) fn from_base(base: ChannelConfig) -> Self {
        Self {
            base,
            persisted_revision: None,
            telemetry_points: Vec::new(),
            signal_points: Vec::new(),
            control_points: Vec::new(),
            adjustment_points: Vec::new(),
        }
    }

    pub(crate) fn from_persisted(base: ChannelConfig, revision: ChannelRevision) -> Self {
        Self {
            persisted_revision: Some(revision),
            ..Self::from_base(base)
        }
    }

    #[must_use]
    pub(crate) const fn persisted_revision(&self) -> Option<ChannelRevision> {
        self.persisted_revision
    }

    #[must_use]
    pub fn id(&self) -> u32 {
        self.base.core.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.base.core.name
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.base.core.protocol
    }

    #[must_use]
    pub fn channel_config(&self) -> &ChannelConfig {
        &self.base
    }

    #[must_use]
    pub fn point_count(&self) -> usize {
        self.telemetry_points.len()
            + self.signal_points.len()
            + self.control_points.len()
            + self.adjustment_points.len()
    }
}
