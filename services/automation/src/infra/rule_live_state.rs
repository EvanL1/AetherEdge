//! Automation composition adapter for authoritative rule live-state reads.

use std::sync::Arc;

use aether_ports::CommandTopologyFence;
use aether_rules::{RuleExecutionContext, RuleLiveState};

/// Rule live-state adapter backed by the current SHM generation and routing
/// snapshot owned by the Automation service composition root.
pub struct ShmRuleLiveState {
    topology: Arc<crate::infra::runtime_topology::AutomationTopologyHandle>,
}

impl ShmRuleLiveState {
    /// Creates a production adapter over the atomically replaceable complete topology.
    #[must_use]
    pub fn from_topology(
        topology: Arc<crate::infra::runtime_topology::AutomationTopologyHandle>,
    ) -> Self {
        Self { topology }
    }
}

impl RuleLiveState for ShmRuleLiveState {
    fn begin_execution(&self) -> RuleExecutionContext {
        RuleExecutionContext::topology_fenced(CommandTopologyFence::new(
            self.topology.load().sequence(),
        ))
    }

    fn get_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)> {
        self.topology
            .load()
            .read_instance_point(instance_id, instance_type != 0, point_id)
            .ok()
            .flatten()
    }

    fn get_instance_for_execution(
        &self,
        execution: RuleExecutionContext,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)> {
        let fence = execution.command_topology_fence()?;
        let generation = self.topology.load();
        if generation.sequence() != fence.expected_sequence() {
            return None;
        }
        generation
            .read_instance_point(instance_id, instance_type != 0, point_id)
            .ok()
            .flatten()
    }
}
