//! Alarm rule persistence and runtime-refresh capability.

use aether_domain::{
    AlarmComparator, AlarmRuleDefinition, AlarmRuleId, AlarmRuleTarget, AlarmSeverity, AlertId,
    DomainError, TimestampMs,
};
use async_trait::async_trait;

use crate::PortResult;

/// Validated partial update for one alarm rule.
#[derive(Debug, Clone, PartialEq)]
pub struct AlarmRulePatch {
    target: Option<AlarmRuleTarget>,
    name: Option<String>,
    severity: Option<AlarmSeverity>,
    comparator: Option<AlarmComparator>,
    threshold: Option<f64>,
    enabled: Option<bool>,
    description: Option<Option<String>>,
}

impl AlarmRulePatch {
    /// Creates a validated partial alarm-rule update.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: Option<AlarmRuleTarget>,
        name: Option<String>,
        severity: Option<AlarmSeverity>,
        comparator: Option<AlarmComparator>,
        threshold: Option<f64>,
        enabled: Option<bool>,
        description: Option<Option<String>>,
    ) -> Result<Self, DomainError> {
        if name.as_ref().is_some_and(|value| value.trim().is_empty()) {
            return Err(DomainError::InvalidAlarmRuleName);
        }
        if threshold.is_some_and(|value| !value.is_finite()) {
            return Err(DomainError::NonFiniteAlarmThreshold);
        }
        Ok(Self {
            target,
            name,
            severity,
            comparator,
            threshold,
            enabled,
            description,
        })
    }

    /// Returns the replacement target, if present.
    #[must_use]
    pub const fn target(&self) -> Option<&AlarmRuleTarget> {
        self.target.as_ref()
    }

    /// Returns the replacement rule name, if present.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the replacement severity, if present.
    #[must_use]
    pub const fn severity(&self) -> Option<AlarmSeverity> {
        self.severity
    }

    /// Returns the replacement comparator, if present.
    #[must_use]
    pub const fn comparator(&self) -> Option<AlarmComparator> {
        self.comparator
    }

    /// Returns the replacement threshold, if present.
    #[must_use]
    pub const fn threshold(&self) -> Option<f64> {
        self.threshold
    }

    /// Returns the replacement enabled state, if present.
    #[must_use]
    pub const fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    /// Returns an optional replacement description. `Some(None)` clears it.
    #[must_use]
    pub fn description(&self) -> Option<Option<&str>> {
        self.description.as_ref().map(|value| value.as_deref())
    }

    /// Returns whether the patch changes no fields.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.target.is_none()
            && self.name.is_none()
            && self.severity.is_none()
            && self.comparator.is_none()
            && self.threshold.is_none()
            && self.enabled.is_none()
            && self.description.is_none()
    }
}

/// Durable alarm-rule mutation selected by the application layer.
#[derive(Debug, Clone, PartialEq)]
pub enum AlarmRuleMutation {
    /// Create one rule from a validated definition.
    Create {
        /// New rule definition.
        definition: AlarmRuleDefinition,
    },
    /// Partially update one existing rule.
    Update {
        /// Rule identity.
        rule_id: AlarmRuleId,
        /// Validated replacement fields.
        patch: AlarmRulePatch,
    },
    /// Change monitoring participation.
    SetEnabled {
        /// Rule identity.
        rule_id: AlarmRuleId,
        /// Desired enabled state.
        enabled: bool,
    },
    /// Permanently remove one rule.
    Delete {
        /// Rule identity.
        rule_id: AlarmRuleId,
    },
}

impl AlarmRuleMutation {
    /// Creates a rule mutation.
    #[must_use]
    pub const fn create(definition: AlarmRuleDefinition) -> Self {
        Self::Create { definition }
    }

    /// Creates an update mutation.
    #[must_use]
    pub const fn update(rule_id: AlarmRuleId, patch: AlarmRulePatch) -> Self {
        Self::Update { rule_id, patch }
    }

    /// Creates an enabled-state mutation.
    #[must_use]
    pub const fn set_enabled(rule_id: AlarmRuleId, enabled: bool) -> Self {
        Self::SetEnabled { rule_id, enabled }
    }

    /// Creates a deletion mutation.
    #[must_use]
    pub const fn delete(rule_id: AlarmRuleId) -> Self {
        Self::Delete { rule_id }
    }

    /// Returns the target identity when the rule already exists.
    #[must_use]
    pub const fn rule_id(&self) -> Option<AlarmRuleId> {
        match self {
            Self::Create { .. } => None,
            Self::Update { rule_id, .. }
            | Self::SetEnabled { rule_id, .. }
            | Self::Delete { rule_id } => Some(*rule_id),
        }
    }

    /// Returns the stable mutation classification.
    #[must_use]
    pub const fn kind(&self) -> AlarmRuleMutationKind {
        match self {
            Self::Create { .. } => AlarmRuleMutationKind::Create,
            Self::Update { .. } => AlarmRuleMutationKind::Update,
            Self::SetEnabled { enabled: true, .. } => AlarmRuleMutationKind::Enable,
            Self::SetEnabled { enabled: false, .. } => AlarmRuleMutationKind::Disable,
            Self::Delete { .. } => AlarmRuleMutationKind::Delete,
        }
    }
}

/// Stable alarm-rule operation classification used by receipts and audits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmRuleMutationKind {
    /// Create a rule.
    Create,
    /// Update a rule definition.
    Update,
    /// Enable monitoring.
    Enable,
    /// Disable monitoring.
    Disable,
    /// Delete a rule.
    Delete,
}

impl AlarmRuleMutationKind {
    /// Returns the stable audit representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Delete => "delete",
        }
    }
}

/// Receipt returned after a durable alarm-rule mutation completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlarmRuleMutationReceipt {
    rule_id: AlarmRuleId,
    kind: AlarmRuleMutationKind,
}

impl AlarmRuleMutationReceipt {
    /// Creates a mutation receipt.
    #[must_use]
    pub const fn new(rule_id: AlarmRuleId, kind: AlarmRuleMutationKind) -> Self {
        Self { rule_id, kind }
    }

    /// Returns the affected rule identity.
    #[must_use]
    pub const fn rule_id(self) -> AlarmRuleId {
        self.rule_id
    }

    /// Returns the applied mutation classification.
    #[must_use]
    pub const fn kind(self) -> AlarmRuleMutationKind {
        self.kind
    }
}

/// Persists one alarm rule mutation and reconciles the active monitor.
///
/// Authorization, explicit confirmation, and audit are application-layer
/// responsibilities. Implementations must report a successful receipt only
/// after durable state and in-process monitoring state agree.
#[async_trait]
pub trait AlarmRuleMutator: Send + Sync + 'static {
    /// Applies one validated mutation.
    async fn mutate(&self, mutation: AlarmRuleMutation) -> PortResult<AlarmRuleMutationReceipt>;
}

/// Receipt returned after one active alert is durably resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertResolutionReceipt {
    alert_id: AlertId,
    rule_id: AlarmRuleId,
    resolved_at: TimestampMs,
}

impl AlertResolutionReceipt {
    /// Creates a resolution receipt with stable alert/rule correlation.
    #[must_use]
    pub const fn new(alert_id: AlertId, rule_id: AlarmRuleId, resolved_at: TimestampMs) -> Self {
        Self {
            alert_id,
            rule_id,
            resolved_at,
        }
    }

    /// Returns the resolved active-alert identity.
    #[must_use]
    pub const fn alert_id(self) -> AlertId {
        self.alert_id
    }

    /// Returns the alarm policy that produced the alert.
    #[must_use]
    pub const fn rule_id(self) -> AlarmRuleId {
        self.rule_id
    }

    /// Returns when local alarm storage accepted the resolution.
    #[must_use]
    pub const fn resolved_at(self) -> TimestampMs {
        self.resolved_at
    }
}

/// Resolves one active alert after application authorization and audit.
#[async_trait]
pub trait AlertResolver: Send + Sync + 'static {
    /// Moves one active alert into retained history.
    async fn resolve(&self, alert_id: AlertId) -> PortResult<AlertResolutionReceipt>;
}
