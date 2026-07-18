//! Environment-backed secret resolution for local commissioning.

use aether_ports::{
    PortError, PortErrorKind, PortResult, SecretMaterial, SecretRef, SecretResolver,
};
use async_trait::async_trait;

const ENVIRONMENT_PREFIX: &str = "env:";

/// Resolves Home Assistant credentials from process environment variables.
///
/// Only references in the form `env:VARIABLE_NAME` are accepted. Variable
/// names use the portable uppercase environment convention and resolved
/// material is returned through [`SecretMaterial`], whose debug output is
/// redacted.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvironmentSecretResolver;

#[async_trait]
impl SecretResolver for EnvironmentSecretResolver {
    async fn resolve(&self, reference: &SecretRef) -> PortResult<SecretMaterial> {
        let variable = reference
            .as_str()
            .strip_prefix(ENVIRONMENT_PREFIX)
            .filter(|variable| valid_variable_name(variable))
            .ok_or_else(invalid_reference)?;
        let material = std::env::var(variable).map_err(|_| unavailable_material())?;
        SecretMaterial::new(material).map_err(|_| invalid_material())
    }
}

fn valid_variable_name(variable: &str) -> bool {
    let mut bytes = variable.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

fn invalid_reference() -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        "Home Assistant secret reference must use a valid environment variable",
    )
}

fn unavailable_material() -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        "Home Assistant credential environment variable is unavailable",
    )
}

fn invalid_material() -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        "Home Assistant credential environment variable contains invalid material",
    )
}
