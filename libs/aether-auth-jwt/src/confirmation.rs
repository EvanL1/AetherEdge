//! The single reading of the explicit-confirmation request header.

use thiserror::Error;

/// Request header through which a caller explicitly confirms a governed
/// mutation.
///
/// Services must take the name from here. A second declaration is how the
/// transports came to disagree about what the header means.
pub const CONFIRMATION_HEADER: &str = "x-aether-confirmed";

/// Whether the caller explicitly confirmed the request.
///
/// The only way to obtain [`Confirmation::Confirmed`] from transport input is
/// [`Confirmation::from_header_values`], so every interface agrees on what
/// counts as a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Exactly one header value was received and it was the literal `true`.
    Confirmed,
    /// The header was absent, or its single value was the literal `false`.
    NotConfirmed,
}

/// The confirmation header was present but was not one literal boolean.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("the confirmation header must carry exactly one value, `true` or `false`")]
pub struct MalformedConfirmation;

impl Confirmation {
    /// Reads every received value of [`CONFIRMATION_HEADER`].
    ///
    /// No value declines. One value must be the exact bytes `true` or
    /// `false`: surrounding whitespace, other casing, other spellings, and
    /// repeated headers are malformed rather than guessed at, because a
    /// lenient reading in one service and a strict one in another is a
    /// confirmation that only some of the system agreed was given.
    pub fn from_header_values<'a>(
        values: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Self, MalformedConfirmation> {
        let mut values = values.into_iter();
        let Some(value) = values.next() else {
            return Ok(Self::NotConfirmed);
        };
        if values.next().is_some() {
            return Err(MalformedConfirmation);
        }
        match value {
            b"true" => Ok(Self::Confirmed),
            b"false" => Ok(Self::NotConfirmed),
            _ => Err(MalformedConfirmation),
        }
    }

    /// Reads the header for an interface that reports a missing confirmation
    /// through the application layer instead of rejecting the request itself.
    ///
    /// Malformed input is treated as no confirmation, so the application
    /// still records and refuses the attempt.
    #[must_use]
    pub fn from_header_values_fail_closed<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Self {
        Self::from_header_values(values).unwrap_or(Self::NotConfirmed)
    }

    /// Returns whether the caller confirmed.
    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }
}
