//! Accepted command outcomes whose completion audit may need reconciliation.

use aether_domain::{AlarmRuleId, AlertId, ChannelId, CommandId, RuleId, TimestampMs};
use aether_ports::{
    ActionRouteKey, ActionRoutingMutationKind, ActionRoutingMutationReceipt, ActionRoutingTarget,
    AlarmRuleMutationReceipt, AlertResolutionReceipt, ChannelMutationKind, ChannelMutationReceipt,
    ChannelReconciliationItem, ChannelReconciliationReceipt, ChannelReconciliationScope,
    ChannelRevision, ChannelRuntimeProjection, CommandReceipt, LogicalRoutingRevision,
    MeasurementRouteKey, MeasurementRoutingMutationKind, MeasurementRoutingMutationReceipt,
    MeasurementRoutingTarget, PortError, RuleExecutionReceipt, RuleMutationReceipt,
};

/// Persistence state of the terminal audit event for an accepted operation.
///
/// An incomplete terminal audit never changes the fact that the underlying
/// non-idempotent operation was accepted. Callers must surface the correlation
/// identifiers and must not retry the operation automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionAuditStatus {
    /// The terminal `Succeeded` event was durably recorded.
    Recorded,
    /// Execution completed, but its terminal audit event could not be recorded.
    Incomplete {
        /// Internal persistence failure retained for logging and reconciliation.
        failure: PortError,
    },
}

impl CompletionAuditStatus {
    /// Returns whether the terminal audit event was durably recorded.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        matches!(self, Self::Recorded)
    }

    /// Returns the terminal-audit persistence failure, when present.
    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Recorded => None,
            Self::Incomplete { failure } => Some(failure),
        }
    }
}

/// A non-idempotent operation already accepted by its execution boundary.
///
/// The outcome deliberately is not an error when only the terminal audit write
/// failed: turning it into a retryable failure could execute the operation a
/// second time. `request_id` remains available even when the concrete receipt
/// uses a separate binary identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedOutcome<R> {
    receipt: R,
    request_id: String,
    completion_audit: CompletionAuditStatus,
}

impl<R> AcceptedOutcome<R> {
    pub(crate) fn recorded(receipt: R, request_id: &str) -> Self {
        Self {
            receipt,
            request_id: request_id.to_string(),
            completion_audit: CompletionAuditStatus::Recorded,
        }
    }

    pub(crate) fn audit_incomplete(receipt: R, request_id: &str, failure: PortError) -> Self {
        Self {
            receipt,
            request_id: request_id.to_string(),
            completion_audit: CompletionAuditStatus::Incomplete { failure },
        }
    }

    /// Returns the transport correlation identifier.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the terminal audit persistence state.
    #[must_use]
    pub const fn completion_audit(&self) -> &CompletionAuditStatus {
        &self.completion_audit
    }

    /// Accepted non-idempotent outcomes are never safe to retry automatically.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        false
    }

    /// Consumes the application outcome and returns its execution receipt.
    #[must_use]
    pub fn into_receipt(self) -> R {
        self.receipt
    }
}

/// Accepted device-command outcome with terminal audit state.
pub type CommandAcceptance = AcceptedOutcome<CommandReceipt>;

impl AcceptedOutcome<CommandReceipt> {
    /// Returns the accepted command's end-to-end correlation identifier.
    #[must_use]
    pub const fn command_id(&self) -> CommandId {
        self.receipt.command_id()
    }

    /// Returns when the local command transport accepted the command.
    #[must_use]
    pub const fn completed_at(&self) -> TimestampMs {
        self.receipt.completed_at()
    }
}

/// Accepted manual rule-execution outcome with terminal audit state.
pub type RuleExecutionAcceptance = AcceptedOutcome<RuleExecutionReceipt>;

impl AcceptedOutcome<RuleExecutionReceipt> {
    /// Returns the executed rule identifier.
    #[must_use]
    pub const fn rule_id(&self) -> RuleId {
        self.receipt.rule_id()
    }

    /// Returns when deterministic rule execution completed.
    #[must_use]
    pub const fn completed_at(&self) -> TimestampMs {
        self.receipt.completed_at()
    }

    /// Returns how many action branches attempted dispatch.
    #[must_use]
    pub const fn actions_attempted(&self) -> u32 {
        self.receipt.actions_attempted()
    }

    /// Returns how many action branches were accepted by their command plane.
    #[must_use]
    pub const fn actions_succeeded(&self) -> u32 {
        self.receipt.actions_succeeded()
    }
}

/// Accepted rule-management outcome with terminal audit state.
pub type RuleMutationAcceptance = AcceptedOutcome<RuleMutationReceipt>;

impl AcceptedOutcome<RuleMutationReceipt> {
    /// Returns the affected rule, or `None` for an explicit scheduler reload.
    #[must_use]
    pub const fn rule_id(&self) -> Option<RuleId> {
        self.receipt.rule_id()
    }

    /// Returns the applied rule-management operation.
    #[must_use]
    pub const fn kind(&self) -> aether_ports::RuleMutationKind {
        self.receipt.kind()
    }

    /// Returns the authoritative automation-rules revision after commit.
    #[must_use]
    pub const fn resulting_revision(&self) -> aether_ports::AutomationRulesRevision {
        self.receipt.resulting_revision()
    }

    /// Returns whether the active scheduler was refreshed or stopped
    /// fail-closed after the accepted mutation.
    #[must_use]
    pub const fn scheduler_refresh(&self) -> &aether_ports::RuleSchedulerRefreshStatus {
        self.receipt.scheduler_refresh()
    }

    /// Returns the complete runtime state, including PointWatch degradation.
    #[must_use]
    pub const fn runtime_status(&self) -> &aether_ports::RuleRuntimeStatus {
        self.receipt.runtime_status()
    }
}

/// Accepted action-routing management outcome with terminal audit state.
pub type ActionRoutingMutationAcceptance = AcceptedOutcome<ActionRoutingMutationReceipt>;

impl AcceptedOutcome<ActionRoutingMutationReceipt> {
    /// Returns the applied action-routing operation.
    #[must_use]
    pub const fn kind(&self) -> ActionRoutingMutationKind {
        self.receipt.kind()
    }

    /// Returns the affected action-route scope.
    #[must_use]
    pub const fn target(&self) -> ActionRoutingTarget {
        self.receipt.target()
    }

    /// Returns the route key when exactly one action route was targeted.
    #[must_use]
    pub const fn route_key(&self) -> Option<ActionRouteKey> {
        self.receipt.route_key()
    }

    /// Returns the number of routes inserted, updated, toggled, or removed.
    #[must_use]
    pub const fn affected_routes(&self) -> u64 {
        self.receipt.affected_routes()
    }

    /// Returns the authoritative shared logical-routing revision after commit.
    #[must_use]
    pub const fn resulting_revision(&self) -> LogicalRoutingRevision {
        self.receipt.resulting_revision()
    }

    /// Returns whether the committed routes were published or commands were
    /// revoked fail-closed pending reconciliation.
    #[must_use]
    pub const fn runtime_status(&self) -> &aether_ports::ActionRoutingRuntimeStatus {
        self.receipt.runtime_status()
    }
}

/// Accepted measurement-routing management outcome with terminal audit state.
pub type MeasurementRoutingMutationAcceptance = AcceptedOutcome<MeasurementRoutingMutationReceipt>;

impl AcceptedOutcome<MeasurementRoutingMutationReceipt> {
    /// Returns the applied mutation kind.
    #[must_use]
    pub const fn kind(&self) -> MeasurementRoutingMutationKind {
        self.receipt.kind()
    }

    /// Returns the affected logical route key.
    #[must_use]
    pub const fn route_key(&self) -> Option<MeasurementRouteKey> {
        self.receipt.route_key()
    }

    /// Returns the typed affected scope.
    #[must_use]
    pub const fn target(&self) -> MeasurementRoutingTarget {
        self.receipt.target()
    }

    /// Returns the number of affected routes.
    #[must_use]
    pub const fn affected_routes(&self) -> u64 {
        self.receipt.affected_routes()
    }

    /// Returns the authoritative revision after commit.
    #[must_use]
    pub const fn resulting_revision(&self) -> LogicalRoutingRevision {
        self.receipt.resulting_revision()
    }

    /// Returns whether runtime publication succeeded or measurement routes were revoked.
    #[must_use]
    pub const fn runtime_status(&self) -> &aether_ports::MeasurementRoutingRuntimeStatus {
        self.receipt.runtime_status()
    }
}

/// Accepted I/O channel-management outcome with terminal audit state.
pub type ChannelMutationAcceptance = AcceptedOutcome<ChannelMutationReceipt>;

impl AcceptedOutcome<ChannelMutationReceipt> {
    /// Returns the resulting channel identifier.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.receipt.channel_id()
    }

    /// Returns the applied channel-management operation.
    #[must_use]
    pub const fn kind(&self) -> ChannelMutationKind {
        self.receipt.kind()
    }

    /// Returns the authoritative desired-state revision after the mutation.
    #[must_use]
    pub const fn resulting_revision(&self) -> ChannelRevision {
        self.receipt.resulting_revision()
    }

    /// Returns the desired persistent enabled state.
    #[must_use]
    pub const fn desired_enabled(&self) -> bool {
        self.receipt.desired_enabled()
    }

    /// Returns the rebuildable runtime projection observed after commit.
    #[must_use]
    pub const fn runtime_projection(&self) -> ChannelRuntimeProjection {
        self.receipt.runtime_projection()
    }

    /// Returns whether desired state still needs runtime reconciliation.
    #[must_use]
    pub const fn reconciliation_required(&self) -> bool {
        self.receipt.reconciliation_required()
    }
}

/// Accepted I/O channel runtime-reconciliation outcome with terminal audit state.
pub type ChannelReconciliationAcceptance = AcceptedOutcome<ChannelReconciliationReceipt>;

impl AcceptedOutcome<ChannelReconciliationReceipt> {
    /// Returns the requested reconciliation scope.
    #[must_use]
    pub const fn scope(&self) -> ChannelReconciliationScope {
        self.receipt.scope()
    }

    /// Returns deterministic sanitized per-channel reconciliation results.
    #[must_use]
    pub fn items(&self) -> &[ChannelReconciliationItem] {
        self.receipt.items()
    }

    /// Returns the number of channels whose runtime projection is degraded.
    #[must_use]
    pub fn degraded_count(&self) -> usize {
        self.receipt.degraded_count()
    }

    /// Returns whether any channel still needs runtime convergence.
    #[must_use]
    pub fn reconciliation_required(&self) -> bool {
        self.receipt.reconciliation_required()
    }
}

/// Accepted alarm-rule management outcome with terminal audit state.
pub type AlarmRuleMutationAcceptance = AcceptedOutcome<AlarmRuleMutationReceipt>;

impl AcceptedOutcome<AlarmRuleMutationReceipt> {
    /// Returns the affected alarm rule.
    #[must_use]
    pub const fn rule_id(&self) -> AlarmRuleId {
        self.receipt.rule_id()
    }

    /// Returns the applied alarm-rule operation.
    #[must_use]
    pub const fn kind(&self) -> aether_ports::AlarmRuleMutationKind {
        self.receipt.kind()
    }
}

/// Accepted manual alert-resolution outcome with terminal audit state.
pub type AlertResolutionAcceptance = AcceptedOutcome<AlertResolutionReceipt>;

impl AcceptedOutcome<AlertResolutionReceipt> {
    /// Returns the resolved alert identity.
    #[must_use]
    pub const fn alert_id(&self) -> AlertId {
        self.receipt.alert_id()
    }

    /// Returns the alarm policy that produced the alert.
    #[must_use]
    pub const fn rule_id(&self) -> AlarmRuleId {
        self.receipt.rule_id()
    }

    /// Returns when local alarm storage accepted the resolution.
    #[must_use]
    pub const fn resolved_at(&self) -> TimestampMs {
        self.receipt.resolved_at()
    }
}
