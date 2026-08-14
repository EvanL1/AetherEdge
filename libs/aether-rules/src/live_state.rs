//! Live-value input for deterministic rule evaluation.

use std::collections::HashMap;
use std::sync::RwLock;

use aether_ports::CommandTopologyFence;

type InstancePointKey = (u32, u8, u32);
type TimestampedValue = (f64, u64);

/// Immutable context captured once before a rule begins reading live state.
///
/// Production attaches the current automation topology sequence. Every read in
/// that execution may validate the context, and every derived device action
/// carries the same fence to the command dispatcher. The in-memory test
/// adapter remains explicitly unfenced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleExecutionContext {
    command_topology_fence: Option<CommandTopologyFence>,
}

impl RuleExecutionContext {
    /// Creates an in-memory test context without a runtime topology fence.
    #[must_use]
    pub const fn unfenced() -> Self {
        Self {
            command_topology_fence: None,
        }
    }

    /// Creates an execution context tied to one exact topology publication.
    #[must_use]
    pub const fn topology_fenced(fence: CommandTopologyFence) -> Self {
        Self {
            command_topology_fence: Some(fence),
        }
    }

    /// Returns the fence that every command derived by this execution must use.
    #[must_use]
    pub const fn command_topology_fence(self) -> Option<CommandTopologyFence> {
        self.command_topology_fence
    }
}

/// Read-only live state consumed by rule evaluation.
///
/// Production composition injects its adapter at the service boundary. The
/// trait exists so unit tests can supply deterministic values without creating
/// an mmap file.
pub trait RuleLiveState: Send + Sync {
    /// Captures one context before the first read in a rule execution.
    ///
    /// The default supports the deterministic in-memory test adapter. A
    /// production topology-aware adapter returns a fenced context.
    fn begin_execution(&self) -> RuleExecutionContext {
        RuleExecutionContext::unfenced()
    }

    /// Read `(value, timestamp_ms)` for an instance point.
    /// `instance_type` is `0` for Measurement and `1` for Action.
    fn get_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)>;

    /// Reads one point under a previously captured execution context.
    ///
    /// The default delegates to the direct read. Topology-aware adapters
    /// override this to reject a read after the captured generation changes.
    fn get_instance_for_execution(
        &self,
        _execution: RuleExecutionContext,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)> {
        self.get_instance(instance_id, instance_type, point_id)
    }
}

/// Deterministic in-process adapter for tests and simulations.
#[derive(Default)]
pub struct MemoryRuleLiveState {
    values: RwLock<HashMap<InstancePointKey, TimestampedValue>>,
}

impl MemoryRuleLiveState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a test value. Returns false only if a previous test
    /// poisoned the lock by panicking while holding it.
    pub fn set_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
        value: f64,
        timestamp_ms: u64,
    ) -> bool {
        let Ok(mut values) = self.values.write() else {
            return false;
        };
        values.insert(
            (instance_id, instance_type, point_id),
            (value, timestamp_ms),
        );
        true
    }
}

impl RuleLiveState for MemoryRuleLiveState {
    fn get_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)> {
        self.values
            .read()
            .ok()?
            .get(&(instance_id, instance_type, point_id))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_adapter_distinguishes_measurements_and_actions() {
        let state = MemoryRuleLiveState::new();
        assert!(state.set_instance(9, 0, 4, 12.5, 100));
        assert!(state.set_instance(9, 1, 4, 7.5, 101));
        assert_eq!(state.get_instance(9, 0, 4), Some((12.5, 100)));
        assert_eq!(state.get_instance(9, 1, 4), Some((7.5, 101)));
    }
}
