//! Default-off governed control of delegated Integration entities.
//!
//! CloudLink carries one closed semantic capability, while current topology,
//! commissioning, delegation, policy, confirmation, provider credentials, and
//! the final execution decision remain authoritative at the edge. Provider
//! acceptance never claims physical completion or job success.

mod codec;
mod error;
mod memory;
mod ports;
mod processor;
mod projection;
mod validation;
mod wire;

pub use codec::IntegrationControlCodec;
pub use error::{IntegrationControlError, IntegrationControlErrorCode};
pub use memory::MemoryIntegrationControlLedger;
pub use ports::{
    AuditEvent, AuditEventKind, AuditRecord, CloudOfferVerifier, ControlClock,
    ControlDependencyError, ControlFailureCode, ControlIdGenerator, ControlSession,
    ControlStoreError, ControllableEntityKind, DenyAllLocalControlAuthority,
    IntegrationActionExecutor, IntegrationControlAudit, IntegrationControlLedger,
    IntegrationPowerAction, LedgerClaim, LedgerClaimOutcome, LedgerEntry, LedgerEntryState,
    LedgerJobKey, LocalAuthorityDecision, LocalAuthorityRequest, LocalControlAuthority,
    ProviderAcceptance, ProviderExecutionResult, ResolvedControlTarget, SpooledActionReceipt,
    SystemControlClock, TargetResolver, UuidControlIdGenerator,
};
pub use processor::{
    IntegrationControlConfig, IntegrationControlProcessor, ProcessDisposition, ProcessedReceipt,
};
pub use projection::ProjectionTargetResolver;
pub use wire::{
    ActionDecision, ActionIntent, ActionOffer, ActionReceiptEnvelope, ActionReceiptPayload,
    ActionReceiptStage, ActionTarget, AuditStatus, CloudAuthorization, CloudConfirmation,
    MessageAuthentication, PhysicalOutcome, ReceiptAudit, ReceiptDelivery,
};

/// Exact AetherContracts release against which the binding is locked.
pub const AETHER_CONTRACTS_RELEASE: &str = "0.1.0-alpha.4";

/// Experimental runtime protocol token; activation is explicitly default-off.
pub const INTEGRATION_CONTROL_EXTENSION: &str = "aether.cloudlink.integration-control.v1alpha1";

/// Frozen action-offer schema discriminator.
pub const ACTION_OFFER_SCHEMA: &str = "aether.cloudlink.integration-action-offer.v1alpha1";

/// Frozen closed semantic intent schema discriminator.
pub const ACTION_INTENT_SCHEMA: &str = "aether.integration-control.action-intent.v1alpha1";

/// Frozen receipt payload schema discriminator.
pub const ACTION_RECEIPT_SCHEMA: &str = "aether.integration-control.action-receipt.v1alpha1";

/// Existing CloudLink uplink envelope discriminator.
pub const CLOUDLINK_ENVELOPE_SCHEMA: &str = "aether.cloudlink.envelope.v1";

/// Stable CloudLink protocol family.
pub const CLOUDLINK_PROTOCOL: &str = "aether.cloudlink";

/// Frozen base protocol version.
pub const CLOUDLINK_PROTOCOL_VERSION: &str = "1.0";

/// Only accepted downlink kind.
pub const MESSAGE_KIND_OFFER: &str = "integration-action-offer";

/// Only emitted business receipt kind.
pub const MESSAGE_KIND_RECEIPT: &str = "integration-action-receipt";

/// Only supported semantic capability.
pub const CAPABILITY_ID: &str = "device.power.set.v1";

/// Edge-final permission required by both cloud evidence and local policy.
pub const PERMISSION: &str = "integration.device.control";

/// Frozen transport safety bound.
pub const MAX_INTEGRATION_CONTROL_MESSAGE_BYTES: usize = 256 * 1024;
