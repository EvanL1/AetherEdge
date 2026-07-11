//! Domain validation failures.

use core::fmt;

use crate::PointKind;

/// Error returned when a domain invariant would be violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainError {
    /// A control command targeted a read-only point.
    PointNotWritable(PointKind),
    /// A command value was NaN or infinite.
    NonFiniteCommandValue,
    /// The command deadline was not later than its issue time.
    InvalidCommandWindow,
    /// A command reached a dispatch boundary at or after its deadline.
    CommandExpired,
    /// A point's configured bounds or step are internally inconsistent.
    InvalidCommandConstraints,
    /// A command value is outside the point's inclusive range.
    CommandValueOutOfRange,
    /// A command value is not aligned to the point's configured step.
    CommandValueOffStep,
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PointNotWritable(kind) => {
                write!(formatter, "point kind {kind:?} is not writable")
            },
            Self::NonFiniteCommandValue => formatter.write_str("command value must be finite"),
            Self::InvalidCommandWindow => {
                formatter.write_str("command expiry must be later than its issue time")
            },
            Self::CommandExpired => formatter.write_str("command has expired"),
            Self::InvalidCommandConstraints => {
                formatter.write_str("command constraints are invalid")
            },
            Self::CommandValueOutOfRange => {
                formatter.write_str("command value is outside the allowed range")
            },
            Self::CommandValueOffStep => {
                formatter.write_str("command value is not aligned to the allowed step")
            },
        }
    }
}
