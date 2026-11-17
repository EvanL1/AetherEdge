use http::{HeaderMap, HeaderName};
use thiserror::Error;

pub const CONFIRMATION_HEADER: &str = "x-aether-confirmed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    Confirmed,
    NotConfirmed,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("x-aether-confirmed must carry exactly one value, `true` or `false`")]
pub struct MalformedConfirmation;

impl Confirmation {
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

    pub fn from_headers(headers: &HeaderMap) -> Result<Self, MalformedConfirmation> {
        Self::from_header_values(
            headers
                .get_all(CONFIRMATION_HEADER)
                .iter()
                .map(|value| value.as_bytes()),
        )
    }

    #[must_use]
    pub const fn header_name() -> HeaderName {
        HeaderName::from_static(CONFIRMATION_HEADER)
    }

    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }
}
