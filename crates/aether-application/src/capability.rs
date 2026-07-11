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
    /// Reversible or bounded configuration mutation.
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

/// Static metadata shared by CLI, MCP, and optional HTTP transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    name: &'static str,
    kind: OperationKind,
    risk: RiskLevel,
    required_permission: &'static str,
    confirmation: ConfirmationPolicy,
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
        idempotent: bool,
    ) -> Self {
        Self {
            name,
            kind,
            risk,
            required_permission,
            confirmation,
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
    true,
);

/// Write one command/action point.
pub const WRITE_POINT_CAPABILITY: CapabilityDescriptor = CapabilityDescriptor::new(
    "device.write_point",
    OperationKind::Command,
    RiskLevel::High,
    "device.control",
    ConfirmationPolicy::Always,
    false,
);

const CAPABILITY_CATALOG: [CapabilityDescriptor; 2] =
    [READ_POINT_CAPABILITY, WRITE_POINT_CAPABILITY];

/// Returns the transport-neutral capability catalog.
#[must_use]
pub const fn capability_catalog() -> &'static [CapabilityDescriptor] {
    &CAPABILITY_CATALOG
}
