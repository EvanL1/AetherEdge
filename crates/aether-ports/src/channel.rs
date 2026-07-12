//! Transport-neutral I/O channel commissioning capability.

use std::{collections::BTreeMap, fmt};

use aether_domain::ChannelId;
use async_trait::async_trait;

use crate::PortResult;

/// Deterministically ordered protocol parameter map.
///
/// The ordered representation gives audit hashing and persistence adapters a
/// stable traversal order without binding this port to JSON or another wire
/// encoding.
pub type ChannelParameters = BTreeMap<String, ChannelParameterValue>;

/// Transport-neutral recursive value accepted by protocol parameter schemas.
#[derive(Clone, PartialEq)]
pub enum ChannelParameterValue {
    /// Explicit absence.
    Null,
    /// Boolean parameter.
    Bool(bool),
    /// Signed integer parameter.
    Integer(i64),
    /// Floating-point parameter.
    Float(f64),
    /// UTF-8 string parameter.
    String(String),
    /// Ordered sequence of parameter values.
    Array(Vec<Self>),
    /// Deterministically ordered parameter object.
    Object(ChannelParameters),
}

impl fmt::Debug for ChannelParameterValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::Bool(_) => formatter.write_str("Bool([redacted])"),
            Self::Integer(_) => formatter.write_str("Integer([redacted])"),
            Self::Float(_) => formatter.write_str("Float([redacted])"),
            Self::String(_) => formatter.write_str("String([redacted])"),
            Self::Array(values) => formatter
                .debug_struct("Array")
                .field("length", &values.len())
                .finish(),
            Self::Object(values) => formatter
                .debug_struct("Object")
                .field("length", &values.len())
                .finish(),
        }
    }
}

/// Transport-neutral per-channel logging policy.
///
/// This remains separate from protocol parameters so transports can preserve
/// the built-in API's `logging` object without coupling the core port to a
/// serialization format.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChannelLoggingPolicy {
    enabled: bool,
    level: Option<String>,
    file: Option<String>,
}

impl ChannelLoggingPolicy {
    /// Selects whether channel-specific logging is enabled.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Selects the channel-specific logging level.
    #[must_use]
    pub fn with_level(mut self, level: impl Into<String>) -> Self {
        self.level = Some(level.into());
        self
    }

    /// Selects the channel-specific log file.
    #[must_use]
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Returns whether channel-specific logging is enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the optional logging level.
    #[must_use]
    pub fn level(&self) -> Option<&str> {
        self.level.as_deref()
    }

    /// Returns the optional log file.
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }
}

impl fmt::Debug for ChannelLoggingPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelLoggingPolicy")
            .field("enabled", &self.enabled)
            .field("level", &self.level.as_ref().map(|_| "[redacted]"))
            .field("file", &self.file.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Monotonic revision of the authoritative desired channel configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelRevision(u64);

impl ChannelRevision {
    /// Creates a revision value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying revision number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next revision, or `None` when the counter is exhausted.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Complete definition used to commission one I/O channel.
///
/// Parameter values are recursively typed without choosing a transport
/// encoding. Callers must not emit their contents to logs or audit records
/// because they may contain device credentials.
#[derive(Clone, PartialEq)]
pub struct ChannelDefinition {
    requested_channel_id: Option<ChannelId>,
    name: String,
    description: Option<String>,
    protocol: String,
    parameters: ChannelParameters,
    logging: ChannelLoggingPolicy,
    enabled: bool,
}

impl ChannelDefinition {
    /// Creates a disabled channel definition.
    ///
    /// A missing identifier asks the concrete commissioning adapter to assign
    /// one. Protocol-specific schema validation remains an adapter concern.
    #[must_use]
    pub fn new(
        requested_channel_id: Option<ChannelId>,
        name: impl Into<String>,
        protocol: impl Into<String>,
        parameters: ChannelParameters,
    ) -> Self {
        Self {
            requested_channel_id,
            name: name.into(),
            description: None,
            protocol: protocol.into(),
            parameters,
            logging: ChannelLoggingPolicy::default(),
            enabled: false,
        }
    }

    /// Adds a human-readable description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Selects whether the adapter should activate the channel immediately.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Replaces the channel-specific logging policy.
    #[must_use]
    pub fn with_logging(mut self, logging: ChannelLoggingPolicy) -> Self {
        self.logging = logging;
        self
    }

    /// Returns the requested identifier, or `None` when it should be assigned.
    #[must_use]
    pub const fn requested_channel_id(&self) -> Option<ChannelId> {
        self.requested_channel_id
    }

    /// Returns the unique human-readable channel name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the optional channel description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Returns the protocol adapter identifier.
    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// Returns the recursively typed protocol parameters.
    #[must_use]
    pub const fn parameters(&self) -> &ChannelParameters {
        &self.parameters
    }

    /// Returns the channel-specific logging policy.
    #[must_use]
    pub const fn logging(&self) -> &ChannelLoggingPolicy {
        &self.logging
    }

    /// Returns whether the commissioned channel should be active immediately.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
}

impl fmt::Debug for ChannelDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelDefinition")
            .field("requested_channel_id", &self.requested_channel_id)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("protocol", &self.protocol)
            .field("parameters", &"[redacted]")
            .field("logging", &self.logging)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// Partial replacement of an existing channel definition.
///
/// Enabled state is intentionally absent. Runtime activation has dedicated
/// enable and disable mutations so transports cannot hide a lifecycle change
/// inside an ordinary configuration update.
#[derive(Clone, Default, PartialEq)]
pub struct ChannelPatch {
    name: Option<String>,
    description: Option<String>,
    protocol: Option<String>,
    parameters: Option<ChannelParameters>,
    logging: Option<ChannelLoggingPolicy>,
}

impl ChannelPatch {
    /// Creates an empty patch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            name: None,
            description: None,
            protocol: None,
            parameters: None,
            logging: None,
        }
    }

    /// Replaces the channel name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Replaces the channel description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Replaces the protocol adapter identifier.
    #[must_use]
    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = Some(protocol.into());
        self
    }

    /// Merges the supplied top-level protocol parameter keys.
    ///
    /// Existing keys omitted from this map remain unchanged. This preserves
    /// the staged HTTP compatibility contract while keeping the values
    /// transport-neutral.
    #[must_use]
    pub fn with_parameters(mut self, parameters: ChannelParameters) -> Self {
        self.parameters = Some(parameters);
        self
    }

    /// Replaces the channel-specific logging policy.
    #[must_use]
    pub fn with_logging(mut self, logging: ChannelLoggingPolicy) -> Self {
        self.logging = Some(logging);
        self
    }

    /// Returns the replacement name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the replacement description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Returns the replacement protocol adapter identifier.
    #[must_use]
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// Returns the top-level protocol parameter updates to merge.
    #[must_use]
    pub const fn parameters(&self) -> Option<&ChannelParameters> {
        self.parameters.as_ref()
    }

    /// Returns the replacement channel-specific logging policy.
    #[must_use]
    pub const fn logging(&self) -> Option<&ChannelLoggingPolicy> {
        self.logging.as_ref()
    }

    /// Returns whether the patch leaves every field unchanged.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.protocol.is_none()
            && self.parameters.is_none()
            && self.logging.is_none()
    }
}

impl fmt::Debug for ChannelPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelPatch")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("protocol", &self.protocol)
            .field(
                "parameters",
                &self.parameters.as_ref().map(|_| "[redacted]"),
            )
            .field("logging", &self.logging)
            .finish()
    }
}

/// Transport-neutral mutation of a commissioned I/O channel.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelMutation {
    /// Create one channel definition, with optional immediate activation.
    Create {
        /// Complete commissioned definition.
        definition: ChannelDefinition,
    },
    /// Partially update an existing channel definition.
    Update {
        /// Current channel identity.
        channel_id: ChannelId,
        /// Optional revision that must still be authoritative when committing.
        expected_revision: Option<ChannelRevision>,
        /// Requested field replacements.
        patch: ChannelPatch,
    },
    /// Permanently remove one channel definition and its runtime.
    Delete {
        /// Channel to remove.
        channel_id: ChannelId,
        /// Optional revision that must still be authoritative when committing.
        expected_revision: Option<ChannelRevision>,
    },
    /// Change whether one channel participates in acquisition and control.
    SetEnabled {
        /// Channel whose runtime lifecycle changes.
        channel_id: ChannelId,
        /// Optional revision that must still be authoritative when committing.
        expected_revision: Option<ChannelRevision>,
        /// Desired activation state.
        enabled: bool,
    },
}

impl ChannelMutation {
    /// Creates a channel commissioning mutation.
    #[must_use]
    pub const fn create(definition: ChannelDefinition) -> Self {
        Self::Create { definition }
    }

    /// Creates a compatibility update without an explicit revision.
    ///
    /// Ordinary updates cannot migrate the channel identity. Identity
    /// migration requires a separate use case that can coordinate every
    /// referencing boundary. Implementations must serialize revisionless
    /// mutations by channel identity; they must not perform a blind concurrent
    /// overwrite.
    #[must_use]
    pub const fn update(channel_id: ChannelId, patch: ChannelPatch) -> Self {
        Self::Update {
            channel_id,
            expected_revision: None,
            patch,
        }
    }

    /// Creates a partial update guarded by an explicit revision compare-and-set.
    #[must_use]
    pub const fn update_with_revision(
        channel_id: ChannelId,
        expected_revision: ChannelRevision,
        patch: ChannelPatch,
    ) -> Self {
        Self::Update {
            channel_id,
            expected_revision: Some(expected_revision),
            patch,
        }
    }

    /// Creates a deletion without a caller-supplied revision.
    ///
    /// Implementations must return [`crate::PortErrorKind::Conflict`] when an
    /// action route still references the channel. They must not cascade that
    /// routing deletion around its separately governed boundary. They must
    /// serialize the revisionless mutation by channel identity.
    #[must_use]
    pub const fn delete(channel_id: ChannelId) -> Self {
        Self::Delete {
            channel_id,
            expected_revision: None,
        }
    }

    /// Creates a channel deletion guarded by an explicit revision.
    #[must_use]
    pub const fn delete_with_revision(
        channel_id: ChannelId,
        expected_revision: ChannelRevision,
    ) -> Self {
        Self::Delete {
            channel_id,
            expected_revision: Some(expected_revision),
        }
    }

    /// Creates an enablement without a caller-supplied revision.
    ///
    /// Implementations must serialize the revisionless mutation by channel
    /// identity.
    #[must_use]
    pub const fn enable(channel_id: ChannelId) -> Self {
        Self::SetEnabled {
            channel_id,
            expected_revision: None,
            enabled: true,
        }
    }

    /// Creates an enablement guarded by an explicit revision.
    #[must_use]
    pub const fn enable_with_revision(
        channel_id: ChannelId,
        expected_revision: ChannelRevision,
    ) -> Self {
        Self::SetEnabled {
            channel_id,
            expected_revision: Some(expected_revision),
            enabled: true,
        }
    }

    /// Creates a disablement without a caller-supplied revision.
    ///
    /// Implementations must serialize the revisionless mutation by channel
    /// identity.
    #[must_use]
    pub const fn disable(channel_id: ChannelId) -> Self {
        Self::SetEnabled {
            channel_id,
            expected_revision: None,
            enabled: false,
        }
    }

    /// Creates a disablement guarded by an explicit revision.
    #[must_use]
    pub const fn disable_with_revision(
        channel_id: ChannelId,
        expected_revision: ChannelRevision,
    ) -> Self {
        Self::SetEnabled {
            channel_id,
            expected_revision: Some(expected_revision),
            enabled: false,
        }
    }

    /// Returns the stable mutation classification.
    #[must_use]
    pub const fn kind(&self) -> ChannelMutationKind {
        match self {
            Self::Create { .. } => ChannelMutationKind::Create,
            Self::Update { .. } => ChannelMutationKind::Update,
            Self::Delete { .. } => ChannelMutationKind::Delete,
            Self::SetEnabled { enabled: true, .. } => ChannelMutationKind::Enable,
            Self::SetEnabled { enabled: false, .. } => ChannelMutationKind::Disable,
        }
    }

    /// Returns the current or requested channel identity when known.
    #[must_use]
    pub const fn channel_id(&self) -> Option<ChannelId> {
        match self {
            Self::Create { definition } => definition.requested_channel_id(),
            Self::Update { channel_id, .. }
            | Self::Delete { channel_id, .. }
            | Self::SetEnabled { channel_id, .. } => Some(*channel_id),
        }
    }

    /// Returns the expected revision when the caller supplied one.
    #[must_use]
    pub const fn expected_revision(&self) -> Option<ChannelRevision> {
        match self {
            Self::Create { .. } => None,
            Self::Update {
                expected_revision, ..
            }
            | Self::Delete {
                expected_revision, ..
            }
            | Self::SetEnabled {
                expected_revision, ..
            } => *expected_revision,
        }
    }

    /// Returns the complete definition for a create mutation.
    #[must_use]
    pub const fn definition(&self) -> Option<&ChannelDefinition> {
        match self {
            Self::Create { definition } => Some(definition),
            Self::Update { .. } | Self::Delete { .. } | Self::SetEnabled { .. } => None,
        }
    }

    /// Returns the field patch for an update mutation.
    #[must_use]
    pub const fn patch(&self) -> Option<&ChannelPatch> {
        match self {
            Self::Update { patch, .. } => Some(patch),
            Self::Create { .. } | Self::Delete { .. } | Self::SetEnabled { .. } => None,
        }
    }
}

/// Stable channel-mutation classification for receipts and audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMutationKind {
    /// Channel creation.
    Create,
    /// Channel definition update.
    Update,
    /// Channel deletion.
    Delete,
    /// Runtime enablement.
    Enable,
    /// Runtime disablement.
    Disable,
}

impl ChannelMutationKind {
    /// Returns a stable audit-friendly operation name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

/// Runtime projection of the authoritative desired channel configuration.
///
/// The durable desired configuration (SQLite in the default composition) is
/// authoritative. This status describes a rebuildable runtime projection and
/// never replaces that authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRuntimeProjection {
    /// Desired state is disabled and no runtime is active.
    Stopped,
    /// Desired state is enabled and activation is still converging.
    ActivationPending,
    /// Desired state is enabled and the commissioned runtime is active.
    Active,
    /// Desired state was committed, but runtime projection needs repair.
    Degraded,
    /// Desired configuration and runtime projection were removed.
    Removed,
}

impl ChannelRuntimeProjection {
    /// Returns a stable audit-friendly projection name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::ActivationPending => "activation_pending",
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Removed => "removed",
        }
    }

    /// Returns whether the runtime must still converge to desired state.
    #[must_use]
    pub const fn reconciliation_required(self) -> bool {
        matches!(self, Self::ActivationPending | Self::Degraded)
    }
}

/// Receipt returned after the desired configuration mutation is committed.
///
/// A pending or degraded runtime projection is still an accepted mutation:
/// callers reconcile it from authoritative desired state and must not retry
/// the non-idempotent command automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMutationReceipt {
    channel_id: ChannelId,
    kind: ChannelMutationKind,
    resulting_revision: ChannelRevision,
    desired_enabled: bool,
    runtime_projection: ChannelRuntimeProjection,
    reconciliation_required: bool,
}

impl ChannelMutationReceipt {
    /// Creates an accepted channel-mutation receipt.
    #[must_use]
    pub const fn new(
        channel_id: ChannelId,
        kind: ChannelMutationKind,
        resulting_revision: ChannelRevision,
        desired_enabled: bool,
        runtime_projection: ChannelRuntimeProjection,
    ) -> Self {
        Self {
            channel_id,
            kind,
            resulting_revision,
            desired_enabled,
            runtime_projection,
            reconciliation_required: runtime_projection.reconciliation_required(),
        }
    }

    /// Returns the resulting channel identifier.
    #[must_use]
    pub const fn channel_id(self) -> ChannelId {
        self.channel_id
    }

    /// Returns the applied operation.
    #[must_use]
    pub const fn kind(self) -> ChannelMutationKind {
        self.kind
    }

    /// Returns the authoritative revision after the accepted mutation.
    #[must_use]
    pub const fn resulting_revision(self) -> ChannelRevision {
        self.resulting_revision
    }

    /// Returns the desired persistent enabled state.
    #[must_use]
    pub const fn desired_enabled(self) -> bool {
        self.desired_enabled
    }

    /// Returns the rebuildable runtime projection after the mutation attempt.
    #[must_use]
    pub const fn runtime_projection(self) -> ChannelRuntimeProjection {
        self.runtime_projection
    }

    /// Returns whether runtime reconciliation remains necessary.
    #[must_use]
    pub const fn reconciliation_required(self) -> bool {
        self.reconciliation_required
    }
}

/// Scope selected for one runtime reconciliation command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelReconciliationScope {
    /// Reconcile every authoritative desired channel plus every orphan runtime.
    All,
    /// Reconcile exactly one channel identity.
    One(ChannelId),
}

impl ChannelReconciliationScope {
    /// Returns the selected channel for a single-channel reconciliation.
    #[must_use]
    pub const fn channel_id(self) -> Option<ChannelId> {
        match self {
            Self::All => None,
            Self::One(channel_id) => Some(channel_id),
        }
    }

    /// Returns whether every desired/runtime channel is in scope.
    #[must_use]
    pub const fn is_all(self) -> bool {
        matches!(self, Self::All)
    }
}

/// Authoritative desired-state fact observed after runtime reconciliation.
///
/// Configuration values are deliberately absent because protocol parameters
/// and logging policy can contain credentials. A deleted identity may retain
/// only its monotonic revision tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelDesiredStateObservation {
    /// A desired channel definition is still authoritative.
    Present {
        /// Desired-state compare-and-set revision.
        revision: ChannelRevision,
        /// Whether the runtime should be active.
        enabled: bool,
    },
    /// No desired channel definition exists.
    Absent {
        /// Last durable tombstone revision, when one is available.
        last_revision: Option<ChannelRevision>,
    },
}

impl ChannelDesiredStateObservation {
    /// Creates an observation for an existing desired channel.
    #[must_use]
    pub const fn present(revision: ChannelRevision, enabled: bool) -> Self {
        Self::Present { revision, enabled }
    }

    /// Creates an observation for an absent desired channel.
    #[must_use]
    pub const fn absent(last_revision: Option<ChannelRevision>) -> Self {
        Self::Absent { last_revision }
    }

    /// Returns whether an authoritative desired definition exists.
    #[must_use]
    pub const fn is_present(self) -> bool {
        matches!(self, Self::Present { .. })
    }

    /// Returns the desired revision or deletion high-water mark.
    #[must_use]
    pub const fn revision(self) -> Option<ChannelRevision> {
        match self {
            Self::Present { revision, .. } => Some(revision),
            Self::Absent { last_revision } => last_revision,
        }
    }

    /// Returns desired enabled state, or `None` when the definition is absent.
    #[must_use]
    pub const fn enabled(self) -> Option<bool> {
        match self {
            Self::Present { enabled, .. } => Some(enabled),
            Self::Absent { .. } => None,
        }
    }
}

/// Per-channel runtime projection observed by a reconciliation command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelReconciliationItem {
    channel_id: ChannelId,
    desired: ChannelDesiredStateObservation,
    runtime_projection: ChannelRuntimeProjection,
}

impl ChannelReconciliationItem {
    /// Creates one sanitized per-channel reconciliation result.
    #[must_use]
    pub const fn new(
        channel_id: ChannelId,
        desired: ChannelDesiredStateObservation,
        runtime_projection: ChannelRuntimeProjection,
    ) -> Self {
        Self {
            channel_id,
            desired,
            runtime_projection,
        }
    }

    /// Returns the reconciled channel identity.
    #[must_use]
    pub const fn channel_id(self) -> ChannelId {
        self.channel_id
    }

    /// Returns the authoritative desired-state observation.
    #[must_use]
    pub const fn desired(self) -> ChannelDesiredStateObservation {
        self.desired
    }

    /// Returns the desired revision or deletion high-water mark.
    #[must_use]
    pub const fn desired_revision(self) -> Option<ChannelRevision> {
        self.desired.revision()
    }

    /// Returns desired enabled state, or `None` when the definition is absent.
    #[must_use]
    pub const fn desired_enabled(self) -> Option<bool> {
        self.desired.enabled()
    }

    /// Returns the rebuildable runtime projection after this attempt.
    #[must_use]
    pub const fn runtime_projection(self) -> ChannelRuntimeProjection {
        self.runtime_projection
    }

    /// Returns whether this channel still needs runtime convergence.
    #[must_use]
    pub const fn reconciliation_required(self) -> bool {
        self.runtime_projection.reconciliation_required()
    }
}

/// Sanitized result of one single-channel or full runtime reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelReconciliationReceipt {
    scope: ChannelReconciliationScope,
    items: Vec<ChannelReconciliationItem>,
}

impl ChannelReconciliationReceipt {
    /// Creates a deterministic receipt ordered by channel identity.
    #[must_use]
    pub fn new(
        scope: ChannelReconciliationScope,
        mut items: Vec<ChannelReconciliationItem>,
    ) -> Self {
        items.sort_unstable_by_key(|item| item.channel_id());
        Self { scope, items }
    }

    /// Returns the requested reconciliation scope.
    #[must_use]
    pub const fn scope(&self) -> ChannelReconciliationScope {
        self.scope
    }

    /// Returns deterministic per-channel outcomes.
    #[must_use]
    pub fn items(&self) -> &[ChannelReconciliationItem] {
        &self.items
    }

    /// Returns the number of channels whose projection is degraded.
    #[must_use]
    pub fn degraded_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.runtime_projection() == ChannelRuntimeProjection::Degraded)
            .count()
    }

    /// Returns whether any channel still needs runtime convergence.
    #[must_use]
    pub fn reconciliation_required(&self) -> bool {
        self.items.iter().any(|item| item.reconciliation_required())
    }
}

/// Reconciles rebuildable channel runtimes from authoritative desired state.
///
/// Implementations must share the same per-channel lifecycle serialization as
/// [`ChannelMutator`]. Per-channel activation, validation, or fencing failures
/// are accepted as degraded items so a caller never retries an already
/// disruptive bulk command blindly. A port error means the reconciliation
/// could not establish its authoritative scope or begin safely.
#[async_trait]
pub trait ChannelReconciler: Send + Sync + 'static {
    /// Reconciles the selected channel runtime scope.
    async fn reconcile(
        &self,
        scope: ChannelReconciliationScope,
    ) -> PortResult<ChannelReconciliationReceipt>;
}

/// Applies one channel mutation and reconciles its durable and runtime state.
///
/// The durable desired configuration is authoritative and runtime state is a
/// rebuildable projection. Implementations return `Conflict` for stale
/// explicit revisions and for deletion while an action route references the
/// channel. Revisionless compatibility mutations must be serialized by
/// channel identity. Implementations must not cascade action-route deletion.
///
/// Once desired state commits, a runtime activation/reconciliation failure is
/// returned as an accepted receipt with [`ChannelRuntimeProjection::Degraded`]
/// (or `ActivationPending`) and `reconciliation_required = true`, not as a
/// retryable port error. A port error therefore means desired state did not
/// commit.
#[async_trait]
pub trait ChannelMutator: Send + Sync + 'static {
    /// Applies one commissioned channel mutation.
    async fn mutate(&self, mutation: ChannelMutation) -> PortResult<ChannelMutationReceipt>;
}
