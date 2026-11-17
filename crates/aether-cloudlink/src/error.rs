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
    /// The Integration extension was not enabled by both Edge and Cloud.
    #[error(
        "CloudLink Integration requires the runtime declaration and explicit Cloud consumer enablement"
    )]
    IntegrationExtensionNotEnabled,
    /// Configured Integration streams or payload identity changed their binding.
    #[error("CloudLink Integration stream binding conflicts with its configured identity")]
    IntegrationStreamBindingConflict,
    /// An outer delivery batch identity disagrees with the Integration payload.
    #[error("CloudLink delivery batch identity does not match the Integration payload")]
    IntegrationBatchIdMismatch,
    /// The embedded provider-neutral Integration object is invalid.
    #[error("invalid Aether Integration payload: {source}")]
    IntegrationContract {
        /// Strict Integration boundary failure.
        #[source]
        source: aether_integration_contract::IntegrationContractError,
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
    /// An explicit protocol deadline has been reached or passed.
    #[error("CloudLink message deadline has expired")]
    MessageExpired,
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

/// Contractual failure identifiers this crate reports for its own errors.
///
/// A variant exists only for a code the pinned AetherContracts failure
/// taxonomy defines; `tests/failure_taxonomy_contract.rs` checks every entry
/// of [`CloudLinkFailureCode::ALL`] against the pinned file. Adding a code
/// here that the contract does not define fails that test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkFailureCode {
    /// Input exceeds a declared bound.
    FieldBound,
    /// The declared protocol version, schema, or message kind is unsupported.
    UnsupportedVersion,
    /// An Integration stream is bound to a different session stream.
    StreamBindingConflict,
    /// An Integration batch identity differs from its envelope.
    BatchIdMismatch,
    /// A closed object contains an undeclared field.
    UnknownField,
    /// Raw JSON is malformed.
    JsonSyntaxError,
    /// Raw JSON repeats an object member name.
    DuplicateJsonKey,
    /// A JSON string contains invalid Unicode.
    JsonInvalidUnicode,
    /// A value has no canonical JSON representation.
    JsonUnsupportedValue,
    /// Authentication material is required and absent.
    AuthenticationRequired,
    /// Authentication material violates the selected profile.
    AuthenticationInvalid,
    /// A version is not strict SemVer.
    SemverInvalid,
    /// A digest has an invalid representation.
    InvalidDigest,
    /// A cursor identity is repeated or conflicts.
    CursorConflict,
    /// A decimal integer string is not canonical.
    IntegerNonCanonical,
    /// A canonical decimal integer exceeds its range.
    IntegerOutOfRange,
    /// A digest does not match the content it covers.
    DigestMismatch,
    /// A message does not belong to the current session.
    StaleSession,
    /// A message is outside its validity window.
    MessageExpired,
}

impl CloudLinkFailureCode {
    /// Every code this crate can report for its own errors.
    pub const ALL: [Self; 19] = [
        Self::FieldBound,
        Self::UnsupportedVersion,
        Self::StreamBindingConflict,
        Self::BatchIdMismatch,
        Self::UnknownField,
        Self::JsonSyntaxError,
        Self::DuplicateJsonKey,
        Self::JsonInvalidUnicode,
        Self::JsonUnsupportedValue,
        Self::AuthenticationRequired,
        Self::AuthenticationInvalid,
        Self::SemverInvalid,
        Self::InvalidDigest,
        Self::CursorConflict,
        Self::IntegerNonCanonical,
        Self::IntegerOutOfRange,
        Self::DigestMismatch,
        Self::StaleSession,
        Self::MessageExpired,
    ];

    /// Returns the AetherContracts failure spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FieldBound => "FIELD_BOUND",
            Self::UnsupportedVersion => "UNSUPPORTED_VERSION",
            Self::StreamBindingConflict => "STREAM_BINDING_CONFLICT",
            Self::BatchIdMismatch => "BATCH_ID_MISMATCH",
            Self::UnknownField => "UNKNOWN_FIELD",
            Self::JsonSyntaxError => "JSON_SYNTAX_ERROR",
            Self::DuplicateJsonKey => "DUPLICATE_JSON_KEY",
            Self::JsonInvalidUnicode => "JSON_INVALID_UNICODE",
            Self::JsonUnsupportedValue => "JSON_UNSUPPORTED_VALUE",
            Self::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            Self::AuthenticationInvalid => "AUTHENTICATION_INVALID",
            Self::SemverInvalid => "SEMVER_INVALID",
            Self::InvalidDigest => "INVALID_DIGEST",
            Self::CursorConflict => "CURSOR_CONFLICT",
            Self::IntegerNonCanonical => "INTEGER_NON_CANONICAL",
            Self::IntegerOutOfRange => "INTEGER_OUT_OF_RANGE",
            Self::DigestMismatch => "DIGEST_MISMATCH",
            Self::StaleSession => "STALE_SESSION",
            Self::MessageExpired => "MESSAGE_EXPIRED",
        }
    }
}

/// The contractual failure a codec error maps to.
///
/// Integration Contract failures keep the code their own crate assigned;
/// everything else is one of this crate's codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkFailure {
    /// A failure this crate classifies.
    Codec(CloudLinkFailureCode),
    /// A failure classified by the Integration Contract crate.
    Integration(aether_integration_contract::IntegrationContractErrorCode),
}

impl CloudLinkFailure {
    /// Returns the AetherContracts failure spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codec(code) => code.as_str(),
            Self::Integration(code) => code.as_str(),
        }
    }
}

impl CloudLinkCodecError {
    /// Returns the contractual failure this error maps to.
    #[must_use]
    pub fn failure(&self) -> CloudLinkFailure {
        use CloudLinkFailureCode as Code;

        let code = match self {
            Self::IntegrationContract { source } => {
                return CloudLinkFailure::Integration(source.code());
            },
            Self::MessageTooLarge { .. } | Self::TooManySamples { .. } => Code::FieldBound,
            Self::IntegrationExtensionNotEnabled => Code::UnsupportedVersion,
            Self::IntegrationStreamBindingConflict => Code::StreamBindingConflict,
            Self::IntegrationBatchIdMismatch => Code::BatchIdMismatch,
            Self::InvalidJson { source } => json_failure(source),
            // Our own content could not be written as canonical JSON.
            Self::CanonicalJson { .. } => Code::JsonUnsupportedValue,
            // The contract has no separate "unsupported message" failure; its
            // reference binding reports an unknown schema or kind this way.
            Self::UnsupportedMessage { .. }
            | Self::UnsupportedProtocolVersion { .. }
            | Self::VersionNegotiationFailed => Code::UnsupportedVersion,
            Self::InvalidField { field, message }
                if matches!(*field, "gateway_key_id" | "gateway_signature")
                    && message.contains("required") =>
            {
                Code::AuthenticationRequired
            },
            Self::InvalidField { field, .. }
                if matches!(
                    *field,
                    "cloud_signature"
                        | "message_authentication"
                        | "session_authentication_key_material"
                        | "gateway_signature.key_id"
                        | "credential_binding.origin_model"
                ) =>
            {
                Code::AuthenticationInvalid
            },
            Self::InvalidField { field, .. } if *field == "payload.manifest.aether_version" => {
                Code::SemverInvalid
            },
            Self::InvalidField { field, .. } if matches!(*field, "delivery.digest" | "digest") => {
                Code::InvalidDigest
            },
            Self::InvalidField { field, .. } if *field == "resume" => Code::CursorConflict,
            Self::InvalidField { .. } | Self::NonFinitePointValue | Self::ControlPointForbidden => {
                Code::FieldBound
            },
            Self::InvalidCanonicalUint64 { .. } => Code::IntegerNonCanonical,
            Self::IntegerOutOfRange { .. } => Code::IntegerOutOfRange,
            Self::DigestMismatch => Code::DigestMismatch,
            Self::SessionMismatch => Code::StaleSession,
            Self::MessageExpired => Code::MessageExpired,
            // `MANIFEST_INVALID` is the release tooling's code for a
            // contract release manifest; a Runtime Manifest whose bytes do
            // not match their checksum is a digest mismatch.
            Self::RuntimeManifestChecksum => Code::DigestMismatch,
        };
        CloudLinkFailure::Codec(code)
    }

    /// Returns the stable public AetherContracts failure taxonomy code.
    #[must_use]
    pub fn failure_code(&self) -> &'static str {
        self.failure().as_str()
    }
}

/// Classifies a strict-JSON decoding failure.
///
/// `serde_json` reports these only as text, so the closed-object violations
/// are recognised by the phrases it uses for them. Anything unrecognised is a
/// syntax error, which is also what a truncated or malformed document is.
fn json_failure(source: &serde_json::Error) -> CloudLinkFailureCode {
    let message = source.to_string();
    if message.contains("unknown field") {
        CloudLinkFailureCode::UnknownField
    } else if message.contains("duplicate field") {
        CloudLinkFailureCode::DuplicateJsonKey
    } else if message.contains("surrogate") || message.contains("unicode code point") {
        CloudLinkFailureCode::JsonInvalidUnicode
    } else {
        CloudLinkFailureCode::JsonSyntaxError
    }
}

impl From<aether_integration_contract::IntegrationContractError> for CloudLinkCodecError {
    fn from(source: aether_integration_contract::IntegrationContractError) -> Self {
        Self::IntegrationContract { source }
    }
}

impl From<serde_json::Error> for CloudLinkCodecError {
    fn from(source: serde_json::Error) -> Self {
        Self::InvalidJson { source }
    }
}
