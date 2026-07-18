//! Secret lookup boundary for adapters that authenticate to external systems.

use std::fmt;

use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::{PortError, PortErrorKind, PortResult};

const MAX_SECRET_REF_BYTES: usize = 512;
const MAX_SECRET_MATERIAL_BYTES: usize = 65_536;

/// Opaque reference to secret material stored outside normal configuration.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    /// Creates a bounded non-empty reference.
    pub fn new(value: impl Into<String>) -> PortResult<Self> {
        let value = value.into();
        if value.trim().is_empty()
            || value.trim() != value
            || value.len() > MAX_SECRET_REF_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                "secret reference is invalid",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the non-secret reference identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SecretRef").field(&self.0).finish()
    }
}

/// Sensitive bytes resolved only inside the adapter that consumes them.
///
/// Debug output is always redacted. This type deliberately does not implement
/// `Clone`, `Display`, serialization, or conversion into ordinary configuration.
pub struct SecretMaterial(Zeroizing<String>);

impl SecretMaterial {
    /// Creates bounded, non-empty secret material.
    pub fn new(value: impl Into<String>) -> PortResult<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SECRET_MATERIAL_BYTES {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                "secret material is empty or exceeds the adapter bound",
            ));
        }
        Ok(Self(Zeroizing::new(value)))
    }

    /// Exposes the secret only at the final authentication boundary.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretMaterial([REDACTED])")
    }
}

/// Resolves a secret reference without placing credentials in channel parameters.
#[async_trait]
pub trait SecretResolver: Send + Sync + 'static {
    /// Resolves secret material or returns a typed unavailable/permanent failure.
    async fn resolve(&self, reference: &SecretRef) -> PortResult<SecretMaterial>;
}
