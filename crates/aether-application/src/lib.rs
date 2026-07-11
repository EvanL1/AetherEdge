//! Transport-neutral Aether use cases and safety policy.

mod capability;
mod context;
mod control;
mod edge;
mod error;
mod outbox_forwarder;
mod policy;

pub use aether_domain::DEFAULT_COMMAND_TTL_MS;
pub use capability::{
    CapabilityDescriptor, ConfirmationPolicy, OperationKind, READ_POINT_CAPABILITY, RiskLevel,
    WRITE_POINT_CAPABILITY, capability_catalog,
};
pub use context::{Actor, RequestContext};
pub use control::ControlApplication;
pub use edge::EdgeApplication;
pub use error::ApplicationError;
pub use outbox_forwarder::{DrainReport, OutboxForwarder};
pub use policy::SafetyPolicy;
