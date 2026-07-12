//! Deterministic automation execution capability.

use aether_domain::{ChannelCommandAddress, ChannelId, InstanceId, PointId, RuleId, TimestampMs};
use async_trait::async_trait;

use crate::{PortError, PortResult};

/// Summary returned after one rule invocation completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleExecutionReceipt {
    rule_id: RuleId,
    completed_at: TimestampMs,
    actions_attempted: u32,
    actions_succeeded: u32,
}

impl RuleExecutionReceipt {
    /// Creates a completed rule-execution summary.
    #[must_use]
    pub const fn new(
        rule_id: RuleId,
        completed_at: TimestampMs,
        actions_attempted: u32,
        actions_succeeded: u32,
    ) -> Self {
        Self {
            rule_id,
            completed_at,
            actions_attempted,
            actions_succeeded,
        }
    }

    /// Returns the invoked rule identifier.
    #[must_use]
    pub const fn rule_id(self) -> RuleId {
        self.rule_id
    }

    /// Returns when execution completed.
    #[must_use]
    pub const fn completed_at(self) -> TimestampMs {
        self.completed_at
    }

    /// Returns the number of action branches that attempted dispatch.
    #[must_use]
    pub const fn actions_attempted(self) -> u32 {
        self.actions_attempted
    }

    /// Returns the number of action branches that completed successfully.
    #[must_use]
    pub const fn actions_succeeded(self) -> u32 {
        self.actions_succeeded
    }
}

/// Executes one already-validated, commissioned automation rule.
///
/// Authorization, confirmation, and invocation audit remain application-layer
/// responsibilities. Implementations own deterministic rule evaluation and
/// return typed recovery failures when the runtime is unavailable.
#[async_trait]
pub trait AutomationRuleExecutor: Send + Sync + 'static {
    /// Executes a rule once.
    async fn execute(&self, rule_id: RuleId) -> PortResult<RuleExecutionReceipt>;
}

/// Persisted automation-rule mutation selected by an application use case.
///
/// Flow and trigger definitions remain opaque JSON text at this boundary. The
/// concrete rule repository owns their schema and validates them before
/// changing durable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleMutation {
    /// Create a disabled rule shell.
    Create {
        /// Human-readable rule name.
        name: String,
        /// Optional rule description.
        description: Option<String>,
    },
    /// Partially update one rule definition.
    Update {
        /// Rule identity.
        rule_id: RuleId,
        /// Optional replacement name.
        name: Option<String>,
        /// Optional replacement description.
        description: Option<String>,
        /// Optional enabled state.
        enabled: Option<bool>,
        /// Optional execution priority.
        priority: Option<u32>,
        /// Optional cooldown in milliseconds.
        cooldown_ms: Option<u64>,
        /// Optional validated-at-adapter Vue Flow JSON text.
        flow_json: Option<String>,
        /// Optional validated-at-adapter trigger JSON text.
        trigger_config: Option<String>,
    },
    /// Change whether one rule participates in deterministic scheduling.
    SetEnabled {
        /// Rule identity.
        rule_id: RuleId,
        /// Desired enabled state.
        enabled: bool,
    },
    /// Permanently remove one rule definition.
    Delete {
        /// Rule identity.
        rule_id: RuleId,
    },
    /// Refresh the active scheduler from the governed rule repository.
    Reload,
}

impl RuleMutation {
    /// Creates a disabled rule shell mutation.
    #[must_use]
    pub fn create(name: impl Into<String>, description: Option<String>) -> Self {
        Self::Create {
            name: name.into(),
            description,
        }
    }

    /// Creates an enabled-state mutation.
    #[must_use]
    pub const fn set_enabled(rule_id: RuleId, enabled: bool) -> Self {
        Self::SetEnabled { rule_id, enabled }
    }

    /// Creates a deletion mutation.
    #[must_use]
    pub const fn delete(rule_id: RuleId) -> Self {
        Self::Delete { rule_id }
    }

    /// Returns the stable mutation classification.
    #[must_use]
    pub const fn kind(&self) -> RuleMutationKind {
        match self {
            Self::Create { .. } => RuleMutationKind::Create,
            Self::Update { .. } => RuleMutationKind::Update,
            Self::SetEnabled { enabled: true, .. } => RuleMutationKind::Enable,
            Self::SetEnabled { enabled: false, .. } => RuleMutationKind::Disable,
            Self::Delete { .. } => RuleMutationKind::Delete,
            Self::Reload => RuleMutationKind::Reload,
        }
    }

    /// Returns the target identity when the rule already exists.
    #[must_use]
    pub const fn rule_id(&self) -> Option<RuleId> {
        match self {
            Self::Create { .. } => None,
            Self::Update { rule_id, .. }
            | Self::SetEnabled { rule_id, .. }
            | Self::Delete { rule_id } => Some(*rule_id),
            Self::Reload => None,
        }
    }
}

/// Stable rule-mutation classification for receipts and audit details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMutationKind {
    /// Rule shell creation.
    Create,
    /// Definition update.
    Update,
    /// Scheduler enablement.
    Enable,
    /// Scheduler disablement.
    Disable,
    /// Definition deletion.
    Delete,
    /// Explicit scheduler refresh.
    Reload,
}

impl RuleMutationKind {
    /// Returns a stable audit-friendly name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Delete => "delete",
            Self::Reload => "reload",
        }
    }
}

/// Receipt returned after durable mutation and scheduler refresh succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMutationReceipt {
    rule_id: Option<RuleId>,
    kind: RuleMutationKind,
    scheduler_refresh: RuleSchedulerRefreshStatus,
}

impl RuleMutationReceipt {
    /// Creates a mutation receipt.
    #[must_use]
    pub const fn new(rule_id: RuleId, kind: RuleMutationKind) -> Self {
        Self {
            rule_id: Some(rule_id),
            kind,
            scheduler_refresh: RuleSchedulerRefreshStatus::Refreshed,
        }
    }

    /// Creates a scheduler-reload receipt.
    #[must_use]
    pub const fn reload() -> Self {
        Self {
            rule_id: None,
            kind: RuleMutationKind::Reload,
            scheduler_refresh: RuleSchedulerRefreshStatus::Refreshed,
        }
    }

    /// Creates an accepted mutation receipt after the scheduler was stopped
    /// fail-closed because its refresh failed.
    #[must_use]
    pub fn scheduler_stopped(
        rule_id: Option<RuleId>,
        kind: RuleMutationKind,
        failure: PortError,
    ) -> Self {
        Self {
            rule_id,
            kind,
            scheduler_refresh: RuleSchedulerRefreshStatus::Stopped { failure },
        }
    }

    /// Returns the affected rule.
    #[must_use]
    pub const fn rule_id(&self) -> Option<RuleId> {
        self.rule_id
    }

    /// Returns the applied mutation kind.
    #[must_use]
    pub const fn kind(&self) -> RuleMutationKind {
        self.kind
    }

    /// Returns the scheduler refresh result following durable mutation.
    #[must_use]
    pub const fn scheduler_refresh(&self) -> &RuleSchedulerRefreshStatus {
        &self.scheduler_refresh
    }
}

/// Runtime activation state after a rule-management command is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleSchedulerRefreshStatus {
    /// The active scheduler view was refreshed successfully.
    Refreshed,
    /// Refresh failed and the scheduler was stopped to prevent stale policy
    /// from continuing to issue commands.
    Stopped {
        /// Internal refresh failure retained for audit/log reconciliation.
        failure: PortError,
    },
}

impl RuleSchedulerRefreshStatus {
    /// Returns whether the scheduler is running the refreshed rule set.
    #[must_use]
    pub const fn is_refreshed(&self) -> bool {
        matches!(self, Self::Refreshed)
    }

    /// Returns the refresh failure when the scheduler was stopped.
    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Refreshed => None,
            Self::Stopped { failure } => Some(failure),
        }
    }
}

/// Applies a validated rule definition mutation and refreshes the scheduler.
///
/// Implementations own persistence and runtime refresh together. Callers must
/// invoke this port only through the governed application command.
#[async_trait]
pub trait AutomationRuleMutator: Send + Sync + 'static {
    /// Applies one mutation and refreshes the active scheduler view.
    async fn mutate(&self, mutation: RuleMutation) -> PortResult<RuleMutationReceipt>;
}

/// Stable identity of one instance action route.
///
/// `action_id` is intentionally distinct from the physical destination. It is
/// the action-point identity in the logical instance model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionRouteKey {
    instance_id: InstanceId,
    action_id: PointId,
}

impl ActionRouteKey {
    /// Creates a logical action-route identity.
    #[must_use]
    pub const fn new(instance_id: InstanceId, action_id: PointId) -> Self {
        Self {
            instance_id,
            action_id,
        }
    }

    /// Returns the instance that owns the logical action point.
    #[must_use]
    pub const fn instance_id(self) -> InstanceId {
        self.instance_id
    }

    /// Returns the action-point identity within the instance.
    #[must_use]
    pub const fn action_id(self) -> PointId {
        self.action_id
    }
}

/// One route from a logical instance action to a physical command-owned point.
///
/// [`ChannelCommandAddress`] makes telemetry/status destinations
/// unrepresentable at this boundary. A route may target either a command or an
/// action slot owned by the device-command plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionRoute {
    key: ActionRouteKey,
    destination: ChannelCommandAddress,
    enabled: bool,
}

impl ActionRoute {
    /// Creates an action route.
    #[must_use]
    pub const fn new(
        key: ActionRouteKey,
        destination: ChannelCommandAddress,
        enabled: bool,
    ) -> Self {
        Self {
            key,
            destination,
            enabled,
        }
    }

    /// Returns the logical route identity.
    #[must_use]
    pub const fn key(self) -> ActionRouteKey {
        self.key
    }

    /// Returns the physical command-plane destination.
    #[must_use]
    pub const fn destination(self) -> ChannelCommandAddress {
        self.destination
    }

    /// Returns whether the route participates in action dispatch.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }
}

/// Typed scope affected by an action-routing mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionRoutingTarget {
    /// One logical action route.
    Route(ActionRouteKey),
    /// Every action route owned by one instance.
    Instance(InstanceId),
    /// Every action route targeting one physical channel.
    Channel(ChannelId),
    /// Every action route in the commissioned model.
    AllActions,
}

impl ActionRoutingTarget {
    /// Returns the single-route key when this target selects one route.
    #[must_use]
    pub const fn route_key(self) -> Option<ActionRouteKey> {
        match self {
            Self::Route(key) => Some(key),
            Self::Instance(_) | Self::Channel(_) | Self::AllActions => None,
        }
    }
}

/// Transport-neutral mutation of commissioned action routes.
///
/// Measurement routing deliberately does not share this command boundary.
/// Scoped deletion variants make adapter-specific bulk-delete endpoints
/// unnecessary and prevent a generic delete from silently widening its scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRoutingMutation {
    /// Create or replace one logical action route.
    Upsert {
        /// Complete replacement route.
        route: ActionRoute,
    },
    /// Delete one logical action route.
    Delete {
        /// Route to remove.
        route_key: ActionRouteKey,
    },
    /// Toggle one logical action route.
    SetEnabled {
        /// Route to toggle.
        route_key: ActionRouteKey,
        /// Desired participation state.
        enabled: bool,
    },
    /// Delete every action route owned by one instance.
    DeleteActionsForInstance {
        /// Instance whose action routes are removed.
        instance_id: InstanceId,
    },
    /// Delete every action route targeting one physical channel.
    DeleteActionsForChannel {
        /// Destination channel whose action routes are removed.
        channel_id: ChannelId,
    },
    /// Delete every commissioned action route.
    DeleteAllActions,
}

impl ActionRoutingMutation {
    /// Creates or replaces one action route.
    #[must_use]
    pub const fn upsert(route: ActionRoute) -> Self {
        Self::Upsert { route }
    }

    /// Deletes one action route.
    #[must_use]
    pub const fn delete(route_key: ActionRouteKey) -> Self {
        Self::Delete { route_key }
    }

    /// Changes whether one action route participates in dispatch.
    #[must_use]
    pub const fn set_enabled(route_key: ActionRouteKey, enabled: bool) -> Self {
        Self::SetEnabled { route_key, enabled }
    }

    /// Deletes all action routes owned by one instance.
    #[must_use]
    pub const fn delete_actions_for_instance(instance_id: InstanceId) -> Self {
        Self::DeleteActionsForInstance { instance_id }
    }

    /// Deletes all action routes targeting one physical channel.
    #[must_use]
    pub const fn delete_actions_for_channel(channel_id: ChannelId) -> Self {
        Self::DeleteActionsForChannel { channel_id }
    }

    /// Deletes every commissioned action route.
    #[must_use]
    pub const fn delete_all() -> Self {
        Self::DeleteAllActions
    }

    /// Returns the stable mutation classification.
    #[must_use]
    pub const fn kind(self) -> ActionRoutingMutationKind {
        match self {
            Self::Upsert { .. } => ActionRoutingMutationKind::Upsert,
            Self::Delete { .. } => ActionRoutingMutationKind::Delete,
            Self::SetEnabled { enabled: true, .. } => ActionRoutingMutationKind::Enable,
            Self::SetEnabled { enabled: false, .. } => ActionRoutingMutationKind::Disable,
            Self::DeleteActionsForInstance { .. } => {
                ActionRoutingMutationKind::DeleteActionsForInstance
            },
            Self::DeleteActionsForChannel { .. } => {
                ActionRoutingMutationKind::DeleteActionsForChannel
            },
            Self::DeleteAllActions => ActionRoutingMutationKind::DeleteAllActions,
        }
    }

    /// Returns the typed scope affected by this mutation.
    #[must_use]
    pub const fn target(self) -> ActionRoutingTarget {
        match self {
            Self::Upsert { route } => ActionRoutingTarget::Route(route.key()),
            Self::Delete { route_key } | Self::SetEnabled { route_key, .. } => {
                ActionRoutingTarget::Route(route_key)
            },
            Self::DeleteActionsForInstance { instance_id } => {
                ActionRoutingTarget::Instance(instance_id)
            },
            Self::DeleteActionsForChannel { channel_id } => {
                ActionRoutingTarget::Channel(channel_id)
            },
            Self::DeleteAllActions => ActionRoutingTarget::AllActions,
        }
    }

    /// Returns the single-route key when this mutation targets one route.
    #[must_use]
    pub const fn route_key(self) -> Option<ActionRouteKey> {
        self.target().route_key()
    }

    /// Returns the replacement route for an upsert mutation.
    #[must_use]
    pub const fn route(&self) -> Option<&ActionRoute> {
        match self {
            Self::Upsert { route } => Some(route),
            Self::Delete { .. }
            | Self::SetEnabled { .. }
            | Self::DeleteActionsForInstance { .. }
            | Self::DeleteActionsForChannel { .. }
            | Self::DeleteAllActions => None,
        }
    }
}

/// Stable action-routing mutation classification for receipts and audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRoutingMutationKind {
    /// Route creation or replacement.
    Upsert,
    /// Single-route deletion.
    Delete,
    /// Single-route enablement.
    Enable,
    /// Single-route disablement.
    Disable,
    /// Instance-scoped action-route deletion.
    DeleteActionsForInstance,
    /// Channel-scoped action-route deletion.
    DeleteActionsForChannel,
    /// Global action-route deletion.
    DeleteAllActions,
}

impl ActionRoutingMutationKind {
    /// Returns a stable audit-friendly operation name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::DeleteActionsForInstance => "delete_actions_for_instance",
            Self::DeleteActionsForChannel => "delete_actions_for_channel",
            Self::DeleteAllActions => "delete_all_actions",
        }
    }
}

/// Receipt returned after an action-routing mutation is durably applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionRoutingMutationReceipt {
    kind: ActionRoutingMutationKind,
    target: ActionRoutingTarget,
    affected_routes: u64,
}

impl ActionRoutingMutationReceipt {
    /// Creates an action-routing mutation receipt.
    #[must_use]
    pub const fn new(
        kind: ActionRoutingMutationKind,
        target: ActionRoutingTarget,
        affected_routes: u64,
    ) -> Self {
        Self {
            kind,
            target,
            affected_routes,
        }
    }

    /// Returns the applied operation.
    #[must_use]
    pub const fn kind(self) -> ActionRoutingMutationKind {
        self.kind
    }

    /// Returns the affected route scope.
    #[must_use]
    pub const fn target(self) -> ActionRoutingTarget {
        self.target
    }

    /// Returns the single-route key when the mutation targeted one route.
    #[must_use]
    pub const fn route_key(self) -> Option<ActionRouteKey> {
        self.target.route_key()
    }

    /// Returns the number of routes inserted, updated, toggled, or removed.
    #[must_use]
    pub const fn affected_routes(self) -> u64 {
        self.affected_routes
    }
}

/// Applies commissioned action-routing mutations.
///
/// Authorization, confirmation, and audit are application-layer concerns.
/// Implementations own persistence and any atomic refresh of their runtime
/// routing view.
#[async_trait]
pub trait AutomationActionRoutingMutator: Send + Sync + 'static {
    /// Applies one typed action-routing mutation.
    async fn mutate(
        &self,
        mutation: ActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt>;
}
