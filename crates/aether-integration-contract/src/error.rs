//! Stable failures for the Integration v1alpha1 product binding.

use std::error::Error;
use std::fmt;

/// Language-neutral failure identifier surfaced by the strict binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntegrationContractErrorCode {
    /// Raw JSON syntax or a duplicate field is invalid.
    JsonSyntaxError,
    /// A closed object contains an undeclared field.
    UnknownField,
    /// A schema discriminator is not the supported Integration v1alpha1 value.
    UnsupportedSchema,
    /// A portable collection or text bound was exceeded.
    FieldBound,
    /// Display or evidence text is blank or contains a forbidden control.
    TextInvalid,
    /// A constrained public identifier is invalid.
    IdentifierInvalid,
    /// An integer string is not canonical.
    IntegerNonCanonical,
    /// An integer string exceeds its declared range.
    IntegerOutOfRange,
    /// A decimal or byte string is not canonically encoded.
    ValueEncodingInvalid,
    /// A JSON number violates the Foundation binary64 safe-number semantics.
    JsonUnsafeNumber,
    /// A JSON number does not represent a finite binary64 value.
    JsonNonFiniteNumber,
    /// Stable identities conflict inside a topology.
    IdentityConflict,
    /// A topology, generation, entity, point, or parent reference is absent.
    ReferenceNotFound,
    /// An observation value discriminant differs from its point descriptor.
    ValueTypeMismatch,
    /// Observation quality and value presence disagree.
    ObservationValueInvalid,
}

impl IntegrationContractErrorCode {
    /// Returns the AetherContracts failure spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonSyntaxError => "JSON_SYNTAX_ERROR",
            Self::UnknownField => "UNKNOWN_FIELD",
            Self::UnsupportedSchema => "UNSUPPORTED_VERSION",
            Self::FieldBound => "FIELD_BOUND",
            Self::TextInvalid => "TEXT_INVALID",
            Self::IdentifierInvalid => "IDENTIFIER_INVALID",
            Self::IntegerNonCanonical => "INTEGER_NON_CANONICAL",
            Self::IntegerOutOfRange => "INTEGER_OUT_OF_RANGE",
            Self::ValueEncodingInvalid => "VALUE_ENCODING_INVALID",
            Self::JsonUnsafeNumber => "JSON_UNSAFE_NUMBER",
            Self::JsonNonFiniteNumber => "JSON_NON_FINITE_NUMBER",
            Self::IdentityConflict => "IDENTITY_CONFLICT",
            Self::ReferenceNotFound => "REFERENCE_NOT_FOUND",
            Self::ValueTypeMismatch => "VALUE_TYPE_MISMATCH",
            Self::ObservationValueInvalid => "OBSERVATION_VALUE_INVALID",
        }
    }
}

/// Typed strict-binding failure without retaining untrusted payload data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationContractError {
    code: IntegrationContractErrorCode,
    message: &'static str,
}

impl IntegrationContractError {
    pub(crate) const fn new(code: IntegrationContractErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    pub(crate) fn from_json(source: &serde_json::Error) -> Self {
        let message = source.to_string();
        let code = if message.contains("unknown field") {
            IntegrationContractErrorCode::UnknownField
        } else if message.contains(IntegrationContractErrorCode::JsonUnsafeNumber.as_str()) {
            IntegrationContractErrorCode::JsonUnsafeNumber
        } else if message.contains(IntegrationContractErrorCode::JsonNonFiniteNumber.as_str()) {
            IntegrationContractErrorCode::JsonNonFiniteNumber
        } else {
            IntegrationContractErrorCode::JsonSyntaxError
        };
        Self::new(code, "integration JSON decoding failed")
    }

    /// Returns the stable language-neutral failure identifier.
    #[must_use]
    pub const fn code(&self) -> IntegrationContractErrorCode {
        self.code
    }
}

impl fmt::Display for IntegrationContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for IntegrationContractError {}

pub(crate) type ContractResult<T> = Result<T, IntegrationContractError>;
