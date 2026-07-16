//! Typed fail-closed errors for the candidate codec.

use thiserror::Error;

/// CloudLink candidate validation or encoding failure.
#[derive(Debug, Error)]
pub enum CloudLinkCodecError {
    /// A transport frame exceeded the hard byte limit.
    #[error("CloudLink message has {found} bytes; maximum is {maximum}")]
    MessageTooLarge {
        /// Observed bytes.
        found: usize,
        /// Contract limit.
        maximum: usize,
    },
    /// JSON is malformed or violates a closed object shape.
    #[error("invalid strict CloudLink JSON: {source}")]
    InvalidJson {
        /// Serde failure, including unknown fields.
        #[source]
        source: serde_json::Error,
    },
    /// The message does not declare a supported schema/kind.
    #[error("unsupported CloudLink message schema or kind {found:?}")]
    UnsupportedMessage {
        /// Rejected discriminator.
        found: String,
    },
    /// The candidate protocol version is not supported.
    #[error("unsupported CloudLink protocol version {found:?}; supported version is {supported}")]
    UnsupportedProtocolVersion {
        /// Rejected version.
        found: String,
        /// Implemented version.
        supported: &'static str,
    },
    /// One closed field violated a semantic bound.
    #[error("invalid CloudLink field {field}: {message}")]
    InvalidField {
        /// Stable field path.
        field: &'static str,
        /// Human-readable rejection reason without secret content.
        message: &'static str,
    },
    /// A protocol integer is not a safe canonical decimal string.
    #[error("invalid canonical uint64 in CloudLink field {field}")]
    InvalidCanonicalUint64 {
        /// Stable field path.
        field: &'static str,
    },
    /// A canonical decimal integer exceeds the uint64 range.
    #[error("CloudLink uint64 is out of range in field {field}")]
    IntegerOutOfRange {
        /// Stable field path.
        field: &'static str,
    },
    /// A telemetry batch exceeded its fixed record bound.
    #[error("CloudLink telemetry contains {found} samples; maximum is {maximum}")]
    TooManySamples {
        /// Observed count.
        found: usize,
        /// Contract limit.
        maximum: usize,
    },
    /// JSON cannot represent a non-finite business value.
    #[error("CloudLink point values must be finite")]
    NonFinitePointValue,
    /// Command/action points cannot become a control channel via telemetry.
    #[error("CloudLink v1 business telemetry cannot contain command or action points")]
    ControlPointForbidden,
    /// Canonical content disagrees with its sealed digest.
    #[error("CloudLink business digest does not match canonical versioned content")]
    DigestMismatch,
    /// A response belongs to another or stale session.
    #[error("CloudLink session epoch or identity does not match the current verified session")]
    SessionMismatch,
    /// A session acceptance did not select an offered version.
    #[error("CloudLink session selected a protocol version the edge did not offer")]
    VersionNegotiationFailed,
    /// Runtime Manifest bytes do not match their existing canonical checksum.
    #[error("CloudLink Runtime Manifest checksum is invalid")]
    RuntimeManifestChecksum,
    /// Canonical JSON serialization failed.
    #[error("cannot canonicalize CloudLink business content: {source}")]
    CanonicalJson {
        /// Serialization failure.
        #[source]
        source: serde_json::Error,
    },
}

impl CloudLinkCodecError {
    /// Returns the stable public AetherContracts failure taxonomy code.
    #[must_use]
    pub fn failure_code(&self) -> &'static str {
        match self {
            Self::MessageTooLarge { .. } | Self::TooManySamples { .. } => "FIELD_BOUND",
            Self::InvalidJson { source } if source.to_string().contains("unknown field") => {
                "UNKNOWN_FIELD"
            },
            Self::InvalidJson { .. } | Self::CanonicalJson { .. } => "INVALID_JSON",
            Self::UnsupportedMessage { .. } => "UNSUPPORTED_MESSAGE",
            Self::UnsupportedProtocolVersion { .. } | Self::VersionNegotiationFailed => {
                "UNSUPPORTED_VERSION"
            },
            Self::InvalidField { field, message }
                if matches!(*field, "gateway_key_id" | "gateway_signature")
                    && message.contains("required") =>
            {
                "AUTHENTICATION_REQUIRED"
            },
            Self::InvalidField { field, .. }
                if matches!(
                    *field,
                    "message_authentication"
                        | "gateway_signature.key_id"
                        | "credential_binding.origin_model"
                ) =>
            {
                "AUTHENTICATION_INVALID"
            },
            Self::InvalidField { field, .. } if *field == "payload.manifest.aether_version" => {
                "SEMVER_INVALID"
            },
            Self::InvalidField { field, .. } if matches!(*field, "delivery.digest" | "digest") => {
                "INVALID_DIGEST"
            },
            Self::InvalidField { field, .. } if *field == "resume" => "CURSOR_CONFLICT",
            Self::InvalidField { .. } | Self::NonFinitePointValue | Self::ControlPointForbidden => {
                "FIELD_BOUND"
            },
            Self::InvalidCanonicalUint64 { .. } => "INTEGER_NON_CANONICAL",
            Self::IntegerOutOfRange { .. } => "INTEGER_OUT_OF_RANGE",
            Self::DigestMismatch => "DIGEST_MISMATCH",
            Self::SessionMismatch => "STALE_SESSION",
            Self::RuntimeManifestChecksum => "MANIFEST_INVALID",
        }
    }
}

impl From<serde_json::Error> for CloudLinkCodecError {
    fn from(source: serde_json::Error) -> Self {
        Self::InvalidJson { source }
    }
}
