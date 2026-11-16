//! The single reading of the explicit-confirmation request header.

use http::{HeaderMap, HeaderName};
use thiserror::Error;

/// Request header through which a caller explicitly confirms a governed
/// mutation.
///
/// Interfaces do not read this header themselves: they call
/// [`Confirmation::from_headers`] or [`Confirmation::from_headers_or_decline`].
/// A second declaration is how the transports came to disagree about what the
/// header means.
pub const CONFIRMATION_HEADER: &str = "x-aether-confirmed";

/// Whether the caller explicitly confirmed the request.
///
/// This module is the only place the header is parsed, so every interface
/// agrees on what counts as a confirmation. How a malformed header is
/// *reported* still belongs to each interface: one that validates requests
/// itself rejects it, one that defers to the application layer declines it.
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

    /// Reads the confirmation header of an HTTP request, rejecting a malformed
    /// one.
    ///
    /// For interfaces that validate the request themselves and answer a
    /// malformed header with a client error.
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, MalformedConfirmation> {
        Self::from_header_values(
            headers
                .get_all(CONFIRMATION_HEADER)
                .iter()
                .map(|value| value.as_bytes()),
        )
    }

    /// Reads the confirmation header of an HTTP request, declining on a
    /// malformed one.
    ///
    /// For interfaces that hand every attempt to the application layer so it
    /// is audited there: malformed input is never a confirmation, and the
    /// application refuses the command as unconfirmed.
    #[must_use]
    pub fn from_headers_or_decline(headers: &HeaderMap) -> Self {
        Self::from_headers(headers).unwrap_or(Self::NotConfirmed)
    }

    /// Returns the header name for an interface that must list it, such as a
    /// CORS allow-list, without declaring the string itself.
    #[must_use]
    pub const fn header_name() -> HeaderName {
        HeaderName::from_static(CONFIRMATION_HEADER)
    }

    /// Returns whether the caller confirmed.
    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }
}
