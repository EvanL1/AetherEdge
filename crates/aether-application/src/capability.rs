//! Machine-discoverable application capabilities.

/// Whether a capability reads state or may mutate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// Read-only operation.
    Query,
    /// State-changing operation.
    Command,
}

/// Operational risk used by authorization and confirmation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    /// Observation or local computation with no state mutation.
    Low,
    /// Bounded processing/egress or a reversible configuration mutation.
    Medium,
    /// Device control, restart, upgrade, or another high-impact mutation.
    High,
}

/// Human confirmation requirement for a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    /// Confirmation is not required.
    Never,
    /// Deployment policy decides whether confirmation is required.
    Policy,
    /// Explicit confirmation is always required.
    Always,
}

/// Whether the application must durably record an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditPolicy {
    /// The read-only operation does not require an audit record.
    NotRequired,
    /// The operation fails closed when its mandatory audit cannot be recorded.
    Required,
}

/// Static metadata shared by CLI, MCP, and optional HTTP transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    name: &'static str,
    kind: OperationKind,
    risk: RiskLevel,
    required_permission: &'static str,
    confirmation: ConfirmationPolicy,
    audit: AuditPolicy,
    idempotent: bool,
}

impl CapabilityDescriptor {
    /// Creates a capability descriptor.
    #[must_use]
    pub const fn new(
        name: &'static str,
        kind: OperationKind,
        risk: RiskLevel,
        required_permission: &'static str,
        confirmation: ConfirmationPolicy,
        audit: AuditPolicy,
        idempotent: bool,
    ) -> Self {
        Self {
            name,
            kind,
            risk,
            required_permission,
            confirmation,
            audit,
            idempotent,
        }
    }

    /// Returns the globally unique capability name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns whether the operation is a query or command.
    #[must_use]
    pub const fn kind(self) -> OperationKind {
        self.kind
    }

    /// Returns the risk classification.
    #[must_use]
    pub const fn risk(self) -> RiskLevel {
        self.risk
    }

    /// Returns the permission required to invoke the capability.
    #[must_use]
    pub const fn required_permission(self) -> &'static str {
        self.required_permission
    }

    /// Returns the confirmation rule.
    #[must_use]
    pub const fn confirmation(self) -> ConfirmationPolicy {
        self.confirmation
    }

    /// Returns whether an explicit confirmation is always required.
    #[must_use]
    pub const fn requires_confirmation(self) -> bool {
        matches!(self.confirmation, ConfirmationPolicy::Always)
    }

    /// Returns whether durable auditing is mandatory for the capability.
    #[must_use]
    pub const fn audit_policy(self) -> AuditPolicy {
        self.audit
    }

    /// Returns whether retrying with the same request identity is safe.
    #[must_use]
    pub const fn is_idempotent(self) -> bool {
        self.idempotent
    }
}

/// Read one current point value.
pub const READ_POINT_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "device.read_point",
    OperationKind::Query,
    RiskLevel::Low,
    "device.read",
    ConfirmationPolicy::Never,
    AuditPolicy::NotRequired,
    true,
);

/// Write one command/action point.
pub const WRITE_POINT_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "device.write_point",
    OperationKind::Command,
    RiskLevel::High,
    "device.control",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Manually execute a commissioned automation rule.
///
/// A rule may dispatch one or more device commands, so this capability uses
/// the same fail-closed confirmation and audit posture as direct control.
pub const EXECUTE_RULE_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "automation.rule.execute",
    OperationKind::Command,
    RiskLevel::High,
    "automation.rule.execute",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Create, edit, enable, disable, or delete an automation rule.
///
/// Rule definitions are control policy: changing even a currently-disabled
/// rule can alter later device behavior. Every mutation therefore uses the
/// high-risk, explicitly-confirmed, durably-audited command posture.
pub const MANAGE_RULE_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "automation.rule.manage",
    OperationKind::Command,
    RiskLevel::High,
    "automation.rule.manage",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Create, replace, toggle, or delete commissioned action routes.
///
/// Action routes decide which physical command-owned point receives a logical
/// automation action. Mutating them can redirect later device control, so the
/// operation is always confirmed, durably audited, and non-idempotent.
pub const MANAGE_ROUTING_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "automation.routing.manage",
    OperationKind::Command,
    RiskLevel::High,
    "automation.routing.manage",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Commission, rename, configure, or delete a logical device instance.
///
/// Instance hierarchy changes alter desired logical topology and can
/// invalidate later automation assumptions. They therefore use a distinct,
/// explicitly confirmed and durably audited high-risk permission.
pub const MANAGE_INSTANCE_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "automation.instance.manage",
    OperationKind::Command,
    RiskLevel::High,
    "automation.instance.manage",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Create, edit, enable, disable, or delete an I/O channel.
///
/// Channel commissioning changes acquisition and device-control connectivity.
/// Every mutation therefore requires explicit confirmation and durable audit,
/// and is never advertised as safely retryable.
pub const MANAGE_CHANNEL_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "io.channel.manage",
    OperationKind::Command,
    RiskLevel::High,
    "io.channel.manage",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Reconcile commissioned I/O channels into their rebuildable runtimes.
///
/// Reconciliation can disconnect and reconnect protocol sessions. It therefore
/// uses the same permission and fail-closed posture as channel commissioning,
/// while remaining a separately discoverable application capability.
pub const RECONCILE_CHANNELS_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "io.channel.reconcile",
    OperationKind::Command,
    RiskLevel::High,
    "io.channel.manage",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Create, edit, enable, disable, or delete an alarm policy.
///
/// Alarm rules affect operator-facing safety signals and may resolve active
/// alarms when disabled or removed, so changes are explicitly confirmed and
/// durably audited.
pub const MANAGE_ALARM_RULE_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "alarm.rule.manage",
    OperationKind::Command,
    RiskLevel::High,
    "alarm.rule.manage",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Manually resolve an active alarm indication.
///
/// Resolution can temporarily hide a condition that remains true until the
/// monitor evaluates it again, so it uses the same fail-closed posture as
/// changing alarm policy.
pub const RESOLVE_ALERT_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "alarm.alert.resolve",
    OperationKind::Command,
    RiskLevel::High,
    "alarm.alert.resolve",
    ConfirmationPolicy::Always,
    AuditPolicy::Required,
    false,
);

/// Discover configured data-processing tasks and bindings.
pub const TASKS_LIST_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "data_processing.tasks.list",
    OperationKind::Query,
    RiskLevel::Low,
    "data_processing.read",
    ConfirmationPolicy::Never,
    AuditPolicy::NotRequired,
    true,
);

/// Discover current processor readiness without sending task data.
pub const PROCESSOR_HEALTH_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "data_processing.processors.health",
    OperationKind::Query,
    RiskLevel::Low,
    "data_processing.read",
    ConfirmationPolicy::Never,
    AuditPolicy::NotRequired,
    true,
);

/// Assemble a governed frame and request derived data from a processor.
pub const PROCESS_DATA_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "data_processing.process",
    OperationKind::Query,
    RiskLevel::Medium,
    "data_processing.run",
    ConfirmationPolicy::Policy,
    AuditPolicy::Required,
    false,
);

const CAPABILITY_CATALOG: [CapabilityDescriptor; 13] = [
    READ_POINT_CAPABILITY,
    WRITE_POINT_CAPABILITY,
    EXECUTE_RULE_CAPABILITY,
    MANAGE_RULE_CAPABILITY,
    MANAGE_ROUTING_CAPABILITY,
    MANAGE_INSTANCE_CAPABILITY,
    MANAGE_CHANNEL_CAPABILITY,
    RECONCILE_CHANNELS_CAPABILITY,
    MANAGE_ALARM_RULE_CAPABILITY,
    RESOLVE_ALERT_CAPABILITY,
    TASKS_LIST_CAPABILITY,
    PROCESSOR_HEALTH_CAPABILITY,
    PROCESS_DATA_CAPABILITY,
];

/// Returns the transport-neutral capability catalog.
#[must_use]
pub const fn capability_catalog() -> &'static [CapabilityDescriptor] {
    &CAPABILITY_CATALOG
}
