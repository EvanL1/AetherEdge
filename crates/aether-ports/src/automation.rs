//! Deterministic automation execution capability.

use aether_domain::{
    ChannelCommandAddress, ChannelId, ChannelPointAddress, InstanceId, PointId, RuleId, TimestampMs,
};
use async_trait::async_trait;

use crate::{PortError, PortErrorKind, PortResult};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AutomationRulesRevision(u64);

impl AutomationRulesRevision {
    /// Creates an automation-rules aggregate revision.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next monotonic revision, or `None` when exhausted.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
}

/// Persisted automation-rule mutation.
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

/// Compare-and-set envelope for a governed rule mutation.
///
/// The envelope extends the published [`RuleMutation`] API without changing
/// its exhaustive variants or legacy constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionedRuleMutation {
    mutation: RuleMutation,
    expected_revision: AutomationRulesRevision,
}

impl RevisionedRuleMutation {
    /// Wraps one legacy-compatible mutation with its mandatory CAS revision.
    #[must_use]
    pub const fn new(mutation: RuleMutation, expected_revision: AutomationRulesRevision) -> Self {
        Self {
            mutation,
            expected_revision,
        }
    }

    /// Creates a revision-fenced disabled rule shell mutation.
    #[must_use]
    pub fn create(
        name: impl Into<String>,
        description: Option<String>,
        expected_revision: AutomationRulesRevision,
    ) -> Self {
        Self::new(RuleMutation::create(name, description), expected_revision)
    }

    /// Creates a revision-fenced enabled-state mutation.
    #[must_use]
    pub const fn set_enabled(
        rule_id: RuleId,
        enabled: bool,
        expected_revision: AutomationRulesRevision,
    ) -> Self {
        Self::new(
            RuleMutation::set_enabled(rule_id, enabled),
            expected_revision,
        )
    }

    /// Creates a revision-fenced deletion mutation.
    #[must_use]
    pub const fn delete(rule_id: RuleId, expected_revision: AutomationRulesRevision) -> Self {
        Self::new(RuleMutation::delete(rule_id), expected_revision)
    }

    /// Creates a revision-fenced scheduler reconciliation mutation.
    #[must_use]
    pub const fn reload(expected_revision: AutomationRulesRevision) -> Self {
        Self::new(RuleMutation::Reload, expected_revision)
    }

    /// Returns the transport-neutral mutation.
    #[must_use]
    pub const fn mutation(&self) -> &RuleMutation {
        &self.mutation
    }

    /// Consumes the envelope and returns its mutation.
    #[must_use]
    pub fn into_mutation(self) -> RuleMutation {
        self.mutation
    }

    /// Returns the revision that must still be authoritative at commit.
    #[must_use]
    pub const fn expected_revision(&self) -> AutomationRulesRevision {
        self.expected_revision
    }

    /// Returns the stable mutation classification.
    #[must_use]
    pub const fn kind(&self) -> RuleMutationKind {
        self.mutation.kind()
    }

    /// Returns the target identity when the rule already exists.
    #[must_use]
    pub const fn rule_id(&self) -> Option<RuleId> {
        self.mutation.rule_id()
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

/// Receipt returned after a rule command commits and runtime publication is attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMutationReceipt {
    rule_id: Option<RuleId>,
    kind: RuleMutationKind,
    resulting_revision: AutomationRulesRevision,
    scheduler_refresh: RuleSchedulerRefreshStatus,
    runtime_status: RuleRuntimeStatus,
}

impl RuleMutationReceipt {
    /// Creates a legacy-compatible mutation receipt without a known revision.
    #[must_use]
    pub const fn new(rule_id: RuleId, kind: RuleMutationKind) -> Self {
        Self::new_at_revision(rule_id, kind, AutomationRulesRevision::new(0))
    }

    /// Creates a mutation receipt with its authoritative resulting revision.
    #[must_use]
    pub const fn new_at_revision(
        rule_id: RuleId,
        kind: RuleMutationKind,
        resulting_revision: AutomationRulesRevision,
    ) -> Self {
        Self {
            rule_id: Some(rule_id),
            kind,
            resulting_revision,
            scheduler_refresh: RuleSchedulerRefreshStatus::Refreshed,
            runtime_status: RuleRuntimeStatus::Refreshed,
        }
    }

    /// Creates a legacy-compatible scheduler-reload receipt.
    #[must_use]
    pub const fn reload() -> Self {
        Self::reload_at_revision(AutomationRulesRevision::new(0))
    }

    /// Creates a scheduler-reload receipt with its authoritative revision.
    #[must_use]
    pub const fn reload_at_revision(resulting_revision: AutomationRulesRevision) -> Self {
        Self {
            rule_id: None,
            kind: RuleMutationKind::Reload,
            resulting_revision,
            scheduler_refresh: RuleSchedulerRefreshStatus::Refreshed,
            runtime_status: RuleRuntimeStatus::Refreshed,
        }
    }

    /// Creates an accepted receipt whose durable rule view was loaded but the
    /// PointWatch projection remains gated pending reconciliation.
    #[must_use]
    pub fn point_watch_gated(
        rule_id: Option<RuleId>,
        kind: RuleMutationKind,
        resulting_revision: AutomationRulesRevision,
        failure: PortError,
    ) -> Self {
        Self {
            rule_id,
            kind,
            resulting_revision,
            scheduler_refresh: RuleSchedulerRefreshStatus::Refreshed,
            runtime_status: RuleRuntimeStatus::PointWatchGated { failure },
        }
    }

    /// Creates a legacy-compatible accepted receipt after scheduler shutdown.
    #[must_use]
    pub fn scheduler_stopped(
        rule_id: Option<RuleId>,
        kind: RuleMutationKind,
        failure: PortError,
    ) -> Self {
        Self::scheduler_stopped_at_revision(rule_id, kind, AutomationRulesRevision::new(0), failure)
    }

    /// Creates an accepted mutation receipt after the scheduler was stopped
    /// fail-closed because its refresh failed.
    #[must_use]
    pub fn scheduler_stopped_at_revision(
        rule_id: Option<RuleId>,
        kind: RuleMutationKind,
        resulting_revision: AutomationRulesRevision,
        failure: PortError,
    ) -> Self {
        Self {
            rule_id,
            kind,
            resulting_revision,
            scheduler_refresh: RuleSchedulerRefreshStatus::Stopped {
                failure: failure.clone(),
            },
            runtime_status: RuleRuntimeStatus::Stopped { failure },
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

    /// Returns the authoritative automation-rules revision after commit.
    #[must_use]
    pub const fn resulting_revision(&self) -> AutomationRulesRevision {
        self.resulting_revision
    }

    /// Returns the scheduler refresh result following durable mutation.
    #[must_use]
    pub const fn scheduler_refresh(&self) -> &RuleSchedulerRefreshStatus {
        &self.scheduler_refresh
    }

    /// Returns the complete runtime state, including PointWatch degradation.
    #[must_use]
    pub const fn runtime_status(&self) -> &RuleRuntimeStatus {
        &self.runtime_status
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

    /// Returns a stable transport and audit representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Refreshed => "refreshed",
            Self::Stopped { .. } => "stopped",
        }
    }

    /// Returns whether runtime reconciliation is still required.
    #[must_use]
    pub const fn reconciliation_required(&self) -> bool {
        !self.is_refreshed()
    }

    /// Returns whether deterministic scheduler evaluation remains active.
    #[must_use]
    pub const fn scheduler_running(&self) -> bool {
        matches!(self, Self::Refreshed)
    }

    /// Returns the refresh/publication failure when runtime reconciliation is required.
    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Refreshed => None,
            Self::Stopped { failure } => Some(failure),
        }
    }
}

/// Complete runtime activation state for a revisioned rule command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleRuntimeStatus {
    /// The scheduler and PointWatch projection use the committed rule set.
    Refreshed,
    /// Deterministic ticks use the committed rules while PointWatch hints are
    /// gated pending reconciliation.
    PointWatchGated {
        /// Publication failure retained for audit and reconciliation.
        failure: PortError,
    },
    /// Refresh failed and the scheduler was stopped fail-closed.
    Stopped {
        /// Refresh failure retained for audit and reconciliation.
        failure: PortError,
    },
}

impl RuleRuntimeStatus {
    /// Returns a stable transport and audit representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Refreshed => "refreshed",
            Self::PointWatchGated { .. } => "point_watch_gated",
            Self::Stopped { .. } => "stopped",
        }
    }

    /// Returns whether runtime reconciliation is still required.
    #[must_use]
    pub const fn reconciliation_required(&self) -> bool {
        !matches!(self, Self::Refreshed)
    }

    /// Returns whether deterministic scheduler evaluation remains active.
    #[must_use]
    pub const fn scheduler_running(&self) -> bool {
        !matches!(self, Self::Stopped { .. })
    }

    /// Returns the retained runtime failure, when degraded.
    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Refreshed => None,
            Self::PointWatchGated { failure } | Self::Stopped { failure } => Some(failure),
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

    /// Applies one revision-fenced mutation.
    ///
    /// Existing third-party implementations remain source-compatible and
    /// fail closed until they explicitly implement revisioned commands.
    async fn mutate_revisioned(
        &self,
        command: RevisionedRuleMutation,
    ) -> PortResult<RuleMutationReceipt> {
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "rule mutator does not support required revision {} for {}",
                command.expected_revision().get(),
                command.kind().as_str()
            ),
        ))
    }
}

/// Monotonic compare-and-set revision of the complete logical-routing authority.
///
/// Measurement and action commands fence against the shared `logical_routing`
/// head so mutations across either plane conflict instead of silently applying
/// against different snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalRoutingRevision(u64);

impl LogicalRoutingRevision {
    /// Creates a routing revision.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next monotonic revision, or `None` when exhausted.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
}

/// Stable identity of one instance measurement route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeasurementRouteKey {
    instance_id: InstanceId,
    measurement_id: PointId,
}

impl MeasurementRouteKey {
    /// Creates a logical measurement-route identity.
    #[must_use]
    pub const fn new(instance_id: InstanceId, measurement_id: PointId) -> Self {
        Self {
            instance_id,
            measurement_id,
        }
    }

    /// Returns the owning instance.
    #[must_use]
    pub const fn instance_id(self) -> InstanceId {
        self.instance_id
    }

    /// Returns the logical measurement point identifier.
    #[must_use]
    pub const fn measurement_id(self) -> PointId {
        self.measurement_id
    }
}

/// One logical measurement bound to an acquisition-owned physical point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasurementRoute {
    key: MeasurementRouteKey,
    destination: ChannelPointAddress,
    enabled: bool,
}

impl MeasurementRoute {
    /// Creates a measurement route.
    #[must_use]
    pub const fn new(
        key: MeasurementRouteKey,
        destination: ChannelPointAddress,
        enabled: bool,
    ) -> Self {
        Self {
            key,
            destination,
            enabled,
        }
    }

    /// Returns the logical route key.
    #[must_use]
    pub const fn key(self) -> MeasurementRouteKey {
        self.key
    }

    /// Returns the acquisition-owned destination.
    #[must_use]
    pub const fn destination(self) -> ChannelPointAddress {
        self.destination
    }

    /// Returns whether the route participates in live projection.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }
}

/// Typed scope affected by one measurement-routing mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeasurementRoutingTarget {
    /// One logical measurement route.
    Route(MeasurementRouteKey),
    /// Every measurement route owned by one instance.
    Instance(InstanceId),
    /// Every measurement route targeting one physical channel.
    Channel(ChannelId),
    /// Every commissioned measurement route.
    AllMeasurements,
}

impl MeasurementRoutingTarget {
    /// Returns the route key when exactly one route is targeted.
    #[must_use]
    pub const fn route_key(self) -> Option<MeasurementRouteKey> {
        match self {
            Self::Route(key) => Some(key),
            Self::Instance(_) | Self::Channel(_) | Self::AllMeasurements => None,
        }
    }
}

/// Transport-neutral, revision-fenced measurement-routing mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementRoutingMutation {
    /// Creates or replaces one measurement route.
    Upsert {
        /// Complete replacement route.
        route: MeasurementRoute,
        /// Revision that must still be authoritative at commit.
        expected_revision: LogicalRoutingRevision,
    },
    /// Removes one measurement route while retaining its revision tombstone.
    Delete {
        /// Logical route to remove.
        route_key: MeasurementRouteKey,
        /// Revision that must still be authoritative at commit.
        expected_revision: LogicalRoutingRevision,
    },
    /// Changes whether one measurement route participates in projection.
    SetEnabled {
        /// Logical route to toggle.
        route_key: MeasurementRouteKey,
        /// Desired enabled state.
        enabled: bool,
        /// Revision that must still be authoritative at commit.
        expected_revision: LogicalRoutingRevision,
    },
    /// Deletes every measurement route owned by one instance.
    DeleteForInstance {
        /// Owning instance.
        instance_id: InstanceId,
        /// Shared logical-routing revision expected at commit.
        expected_revision: LogicalRoutingRevision,
    },
    /// Deletes every measurement route targeting one physical channel.
    DeleteForChannel {
        /// Target channel.
        channel_id: ChannelId,
        /// Shared logical-routing revision expected at commit.
        expected_revision: LogicalRoutingRevision,
    },
    /// Deletes every commissioned measurement route.
    DeleteAll {
        /// Shared logical-routing revision expected at commit.
        expected_revision: LogicalRoutingRevision,
    },
}

impl MeasurementRoutingMutation {
    /// Creates or replaces one route with mandatory CAS.
    #[must_use]
    pub const fn upsert(
        route: MeasurementRoute,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::Upsert {
            route,
            expected_revision,
        }
    }

    /// Deletes one route with mandatory CAS.
    #[must_use]
    pub const fn delete(
        route_key: MeasurementRouteKey,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::Delete {
            route_key,
            expected_revision,
        }
    }

    /// Toggles one route with mandatory CAS.
    #[must_use]
    pub const fn set_enabled(
        route_key: MeasurementRouteKey,
        enabled: bool,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::SetEnabled {
            route_key,
            enabled,
            expected_revision,
        }
    }

    /// Deletes all measurement routes for one instance with mandatory CAS.
    #[must_use]
    pub const fn delete_for_instance(
        instance_id: InstanceId,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::DeleteForInstance {
            instance_id,
            expected_revision,
        }
    }

    /// Deletes all measurement routes for one channel with mandatory CAS.
    #[must_use]
    pub const fn delete_for_channel(
        channel_id: ChannelId,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::DeleteForChannel {
            channel_id,
            expected_revision,
        }
    }

    /// Deletes every measurement route with mandatory CAS.
    #[must_use]
    pub const fn delete_all(expected_revision: LogicalRoutingRevision) -> Self {
        Self::DeleteAll { expected_revision }
    }

    /// Returns the mutation kind.
    #[must_use]
    pub const fn kind(self) -> MeasurementRoutingMutationKind {
        match self {
            Self::Upsert { .. } => MeasurementRoutingMutationKind::Upsert,
            Self::Delete { .. } => MeasurementRoutingMutationKind::Delete,
            Self::SetEnabled { enabled: true, .. } => MeasurementRoutingMutationKind::Enable,
            Self::SetEnabled { enabled: false, .. } => MeasurementRoutingMutationKind::Disable,
            Self::DeleteForInstance { .. } => MeasurementRoutingMutationKind::DeleteForInstance,
            Self::DeleteForChannel { .. } => MeasurementRoutingMutationKind::DeleteForChannel,
            Self::DeleteAll { .. } => MeasurementRoutingMutationKind::DeleteAll,
        }
    }

    /// Returns the typed mutation scope.
    #[must_use]
    pub const fn target(self) -> MeasurementRoutingTarget {
        match self {
            Self::Upsert { route, .. } => MeasurementRoutingTarget::Route(route.key()),
            Self::Delete { route_key, .. } | Self::SetEnabled { route_key, .. } => {
                MeasurementRoutingTarget::Route(route_key)
            },
            Self::DeleteForInstance { instance_id, .. } => {
                MeasurementRoutingTarget::Instance(instance_id)
            },
            Self::DeleteForChannel { channel_id, .. } => {
                MeasurementRoutingTarget::Channel(channel_id)
            },
            Self::DeleteAll { .. } => MeasurementRoutingTarget::AllMeasurements,
        }
    }

    /// Returns the route key when exactly one route is targeted.
    #[must_use]
    pub const fn route_key(self) -> Option<MeasurementRouteKey> {
        self.target().route_key()
    }

    /// Returns the revision that must still be authoritative.
    #[must_use]
    pub const fn expected_revision(self) -> LogicalRoutingRevision {
        match self {
            Self::Upsert {
                expected_revision, ..
            }
            | Self::Delete {
                expected_revision, ..
            }
            | Self::SetEnabled {
                expected_revision, ..
            }
            | Self::DeleteForInstance {
                expected_revision, ..
            }
            | Self::DeleteForChannel {
                expected_revision, ..
            }
            | Self::DeleteAll { expected_revision } => expected_revision,
        }
    }
}

/// Stable measurement-routing mutation classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementRoutingMutationKind {
    /// Route creation or replacement.
    Upsert,
    /// Route deletion.
    Delete,
    /// Route enablement.
    Enable,
    /// Route disablement.
    Disable,
    /// Instance-scoped deletion.
    DeleteForInstance,
    /// Channel-scoped deletion.
    DeleteForChannel,
    /// Global measurement-route deletion.
    DeleteAll,
}

impl MeasurementRoutingMutationKind {
    /// Returns a stable audit-friendly operation name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert_measurement",
            Self::Delete => "delete_measurement",
            Self::Enable => "enable_measurement",
            Self::Disable => "disable_measurement",
            Self::DeleteForInstance => "delete_instance_measurements",
            Self::DeleteForChannel => "delete_channel_measurements",
            Self::DeleteAll => "delete_all_measurements",
        }
    }
}

/// Runtime measurement publication state after SQLite commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasurementRoutingRuntimeStatus {
    /// The committed measurement routes are active.
    Published,
    /// Measurement projection was revoked after publication failed.
    MeasurementsRevoked {
        /// Publication failure retained for reconciliation and audit.
        failure: PortError,
    },
}

impl MeasurementRoutingRuntimeStatus {
    /// Returns the stable transport representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::MeasurementsRevoked { .. } => "measurements_revoked",
        }
    }

    /// Returns whether the committed routes are active.
    #[must_use]
    pub const fn is_published(&self) -> bool {
        matches!(self, Self::Published)
    }

    /// Returns whether reconciliation is required.
    #[must_use]
    pub const fn reconciliation_required(&self) -> bool {
        !self.is_published()
    }

    /// Returns the publication failure, when degraded.
    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Published => None,
            Self::MeasurementsRevoked { failure } => Some(failure),
        }
    }
}

/// Receipt for one durably committed measurement-routing mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementRoutingMutationReceipt {
    kind: MeasurementRoutingMutationKind,
    target: MeasurementRoutingTarget,
    affected_routes: u64,
    resulting_revision: LogicalRoutingRevision,
    runtime_status: MeasurementRoutingRuntimeStatus,
}

impl MeasurementRoutingMutationReceipt {
    /// Creates a successfully published receipt.
    #[must_use]
    pub const fn new(
        kind: MeasurementRoutingMutationKind,
        target: MeasurementRoutingTarget,
        affected_routes: u64,
        resulting_revision: LogicalRoutingRevision,
    ) -> Self {
        Self {
            kind,
            target,
            affected_routes,
            resulting_revision,
            runtime_status: MeasurementRoutingRuntimeStatus::Published,
        }
    }

    /// Creates an accepted receipt whose measurement projection is revoked.
    #[must_use]
    pub fn measurements_revoked(
        kind: MeasurementRoutingMutationKind,
        target: MeasurementRoutingTarget,
        affected_routes: u64,
        resulting_revision: LogicalRoutingRevision,
        failure: PortError,
    ) -> Self {
        Self {
            kind,
            target,
            affected_routes,
            resulting_revision,
            runtime_status: MeasurementRoutingRuntimeStatus::MeasurementsRevoked { failure },
        }
    }

    /// Returns the applied mutation kind.
    #[must_use]
    pub const fn kind(&self) -> MeasurementRoutingMutationKind {
        self.kind
    }

    /// Returns the affected typed scope.
    #[must_use]
    pub const fn target(&self) -> MeasurementRoutingTarget {
        self.target
    }

    /// Returns the route key when exactly one route was targeted.
    #[must_use]
    pub const fn route_key(&self) -> Option<MeasurementRouteKey> {
        self.target.route_key()
    }

    /// Returns the number of affected routes.
    #[must_use]
    pub const fn affected_routes(&self) -> u64 {
        self.affected_routes
    }

    /// Returns the authoritative revision after commit.
    #[must_use]
    pub const fn resulting_revision(&self) -> LogicalRoutingRevision {
        self.resulting_revision
    }

    /// Returns runtime publication state.
    #[must_use]
    pub const fn runtime_status(&self) -> &MeasurementRoutingRuntimeStatus {
        &self.runtime_status
    }
}

/// Applies validated, revision-fenced measurement-routing mutations.
#[async_trait]
pub trait AutomationMeasurementRoutingMutator: Send + Sync + 'static {
    /// Applies one mutation and publishes or revokes the runtime projection.
    async fn mutate(
        &self,
        mutation: MeasurementRoutingMutation,
    ) -> PortResult<MeasurementRoutingMutationReceipt>;
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
            Self::DeleteActionsForInstance { instance_id, .. } => {
                ActionRoutingTarget::Instance(instance_id)
            },
            Self::DeleteActionsForChannel { channel_id, .. } => {
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

/// Compare-and-set envelope for a governed action-routing mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionedActionRoutingMutation {
    mutation: ActionRoutingMutation,
    expected_revision: LogicalRoutingRevision,
}

impl RevisionedActionRoutingMutation {
    /// Wraps one legacy-compatible routing mutation with its mandatory CAS revision.
    #[must_use]
    pub const fn new(
        mutation: ActionRoutingMutation,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self {
            mutation,
            expected_revision,
        }
    }

    /// Creates or replaces one action route with mandatory CAS.
    #[must_use]
    pub const fn upsert(route: ActionRoute, expected_revision: LogicalRoutingRevision) -> Self {
        Self::new(ActionRoutingMutation::upsert(route), expected_revision)
    }

    /// Deletes one action route with mandatory CAS.
    #[must_use]
    pub const fn delete(
        route_key: ActionRouteKey,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::new(ActionRoutingMutation::delete(route_key), expected_revision)
    }

    /// Changes whether one action route participates in dispatch with mandatory CAS.
    #[must_use]
    pub const fn set_enabled(
        route_key: ActionRouteKey,
        enabled: bool,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::new(
            ActionRoutingMutation::set_enabled(route_key, enabled),
            expected_revision,
        )
    }

    /// Deletes all action routes owned by one instance with mandatory CAS.
    #[must_use]
    pub const fn delete_actions_for_instance(
        instance_id: InstanceId,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::new(
            ActionRoutingMutation::delete_actions_for_instance(instance_id),
            expected_revision,
        )
    }

    /// Deletes all action routes targeting one channel with mandatory CAS.
    #[must_use]
    pub const fn delete_actions_for_channel(
        channel_id: ChannelId,
        expected_revision: LogicalRoutingRevision,
    ) -> Self {
        Self::new(
            ActionRoutingMutation::delete_actions_for_channel(channel_id),
            expected_revision,
        )
    }

    /// Deletes every commissioned action route with mandatory CAS.
    #[must_use]
    pub const fn delete_all(expected_revision: LogicalRoutingRevision) -> Self {
        Self::new(ActionRoutingMutation::delete_all(), expected_revision)
    }

    /// Returns the transport-neutral routing mutation.
    #[must_use]
    pub const fn mutation(self) -> ActionRoutingMutation {
        self.mutation
    }

    /// Returns the shared revision that must still be authoritative.
    #[must_use]
    pub const fn expected_revision(self) -> LogicalRoutingRevision {
        self.expected_revision
    }

    /// Returns the stable mutation classification.
    #[must_use]
    pub const fn kind(self) -> ActionRoutingMutationKind {
        self.mutation.kind()
    }

    /// Returns the typed mutation target.
    #[must_use]
    pub const fn target(self) -> ActionRoutingTarget {
        self.mutation.target()
    }

    /// Returns the single-route key when this command targets one route.
    #[must_use]
    pub const fn route_key(self) -> Option<ActionRouteKey> {
        self.mutation.route_key()
    }

    /// Returns the replacement route for an upsert command.
    #[must_use]
    pub const fn route(&self) -> Option<&ActionRoute> {
        self.mutation.route()
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

/// Runtime publication state after an action-routing mutation commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRoutingRuntimeStatus {
    /// The command dispatcher is using the committed routing snapshot.
    Published,
    /// Runtime publication failed and every action route was revoked
    /// fail-closed until reconciliation succeeds.
    CommandsRevoked {
        /// Internal publication failure retained for audit and reconciliation.
        failure: PortError,
    },
}

impl ActionRoutingRuntimeStatus {
    /// Returns the stable transport and audit representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::CommandsRevoked { .. } => "commands_revoked",
        }
    }

    /// Returns whether the runtime is using the committed routing snapshot.
    #[must_use]
    pub const fn is_published(&self) -> bool {
        matches!(self, Self::Published)
    }

    /// Returns whether a later reconciliation must restore command routing.
    #[must_use]
    pub const fn reconciliation_required(&self) -> bool {
        !self.is_published()
    }

    /// Returns the retained runtime-publication failure, when degraded.
    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Published => None,
            Self::CommandsRevoked { failure } => Some(failure),
        }
    }
}

/// Receipt returned after an action-routing mutation is durably applied.
///
/// A revoked runtime is still an accepted, non-idempotent mutation: callers
/// must reconcile it from SQLite and must not retry the committed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRoutingMutationReceipt {
    kind: ActionRoutingMutationKind,
    target: ActionRoutingTarget,
    affected_routes: u64,
    resulting_revision: LogicalRoutingRevision,
    runtime_status: ActionRoutingRuntimeStatus,
}

impl ActionRoutingMutationReceipt {
    /// Creates a legacy-compatible receipt without a known revision.
    #[must_use]
    pub const fn new(
        kind: ActionRoutingMutationKind,
        target: ActionRoutingTarget,
        affected_routes: u64,
    ) -> Self {
        Self::new_at_revision(
            kind,
            target,
            affected_routes,
            LogicalRoutingRevision::new(0),
        )
    }

    /// Creates a receipt with its authoritative resulting revision.
    #[must_use]
    pub const fn new_at_revision(
        kind: ActionRoutingMutationKind,
        target: ActionRoutingTarget,
        affected_routes: u64,
        resulting_revision: LogicalRoutingRevision,
    ) -> Self {
        Self {
            kind,
            target,
            affected_routes,
            resulting_revision,
            runtime_status: ActionRoutingRuntimeStatus::Published,
        }
    }

    /// Creates a legacy-compatible degraded receipt without a known revision.
    #[must_use]
    pub fn commands_revoked(
        kind: ActionRoutingMutationKind,
        target: ActionRoutingTarget,
        affected_routes: u64,
        failure: PortError,
    ) -> Self {
        Self::commands_revoked_at_revision(
            kind,
            target,
            affected_routes,
            LogicalRoutingRevision::new(0),
            failure,
        )
    }

    /// Creates an accepted receipt whose runtime command routes were revoked
    /// after the durable mutation committed but publication failed.
    #[must_use]
    pub fn commands_revoked_at_revision(
        kind: ActionRoutingMutationKind,
        target: ActionRoutingTarget,
        affected_routes: u64,
        resulting_revision: LogicalRoutingRevision,
        failure: PortError,
    ) -> Self {
        Self {
            kind,
            target,
            affected_routes,
            resulting_revision,
            runtime_status: ActionRoutingRuntimeStatus::CommandsRevoked { failure },
        }
    }

    /// Returns the applied operation.
    #[must_use]
    pub const fn kind(&self) -> ActionRoutingMutationKind {
        self.kind
    }

    /// Returns the affected route scope.
    #[must_use]
    pub const fn target(&self) -> ActionRoutingTarget {
        self.target
    }

    /// Returns the single-route key when the mutation targeted one route.
    #[must_use]
    pub const fn route_key(&self) -> Option<ActionRouteKey> {
        self.target.route_key()
    }

    /// Returns the number of routes inserted, updated, toggled, or removed.
    #[must_use]
    pub const fn affected_routes(&self) -> u64 {
        self.affected_routes
    }

    /// Returns the authoritative shared logical-routing revision after commit.
    #[must_use]
    pub const fn resulting_revision(&self) -> LogicalRoutingRevision {
        self.resulting_revision
    }

    /// Returns the runtime publication state after the durable commit.
    #[must_use]
    pub const fn runtime_status(&self) -> &ActionRoutingRuntimeStatus {
        &self.runtime_status
    }
}

/// Applies commissioned action-routing mutations.
///
/// Authorization, confirmation, and audit are application-layer concerns.
/// Implementations own persistence and any atomic refresh of their runtime
/// routing view. Once persistence commits, a publication failure must be
/// returned as an accepted [`ActionRoutingMutationReceipt::commands_revoked`]
/// receipt; a port error means the durable mutation did not commit.
#[async_trait]
pub trait AutomationActionRoutingMutator: Send + Sync + 'static {
    /// Applies one typed action-routing mutation.
    async fn mutate(
        &self,
        mutation: ActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt>;

    /// Applies one revision-fenced action-routing mutation.
    ///
    /// Existing implementations remain source-compatible and fail closed
    /// until they explicitly support logical-routing CAS.
    async fn mutate_revisioned(
        &self,
        command: RevisionedActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt> {
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "action-routing mutator does not support required revision {} for {}",
                command.expected_revision().get(),
                command.kind().as_str()
            ),
        ))
    }
}
