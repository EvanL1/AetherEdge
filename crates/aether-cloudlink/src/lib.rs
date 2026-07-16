//! Transport-neutral experimental CloudLink protocol foundation.
//!
//! The wire contract implemented here is the digest-pinned experimental public
//! AetherContracts subset. It is not a production release or a complete
//! conformance claim. Concrete MQTT code lives in an extension and the business
//! protocol does not depend on broker features.

mod codec;
mod error;
mod session;
mod telemetry;
mod validation;

pub use codec::{
    CandidateMessage, CloudLinkCodec, DataLossPayload, DeliveryDescriptor, DeliveryEnvelope,
    DurableAckMessage, HeartbeatMessage, ReplayRequest, RuntimeManifestReport,
};
pub use error::CloudLinkCodecError;
pub use session::{
    CredentialOriginModel, MessageAuthentication, ResumeCursor, SessionAccepted, SessionBinding,
    SessionChallenge, SessionHello,
};
pub use telemetry::{PointFact, PointModelBinding, TelemetryBatch, TopologyBinding};

/// Maximum encoded CloudLink message accepted at any transport boundary.
pub const MAX_CLOUDLINK_MESSAGE_BYTES: usize = 256 * 1024;

/// Maximum number of business point facts in one telemetry batch.
pub const MAX_POINT_SAMPLES: usize = 256;

/// Candidate application protocol version.
pub const CLOUDLINK_PROTOCOL_VERSION: &str = "1.0";

/// Stable protocol family marker.
pub const CLOUDLINK_PROTOCOL: &str = "aether.cloudlink";
