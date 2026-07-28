//! Unified service bootstrap utilities
//!
//! Provides common initialization functionality for all AetherEdge services,
//! including startup banners, logging initialization, and environment setup.

use crate::logging;
use tracing::info;

/// Service metadata for startup
pub struct ServiceInfo {
    /// Canonical service name (for example, `aether-io`).
    pub name: String,
    /// Service version from Cargo.toml
    pub version: String,
    /// Default port
    pub default_port: u16,
}

impl ServiceInfo {
    /// Create new service info
    pub fn new(name: impl Into<String>, default_port: u16) -> Self {
        Self {
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            default_port,
        }
    }
}

/// Print unified startup banner for any service
pub fn print_startup_banner(service: &ServiceInfo) {
    info!("Aether Edge Runtime");
    info!(
        "{} v{} | Port: {}",
        service.name.to_uppercase(),
        service.version,
        service.default_port
    );
}

/// Initialize console-first tracing for a service.
pub fn init_logging(service: &ServiceInfo) -> anyhow::Result<()> {
    logging::init(&service.name).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Load environment variables in development mode
///
/// In debug builds, reads .env file and sets environment variables.
/// In release builds, this is a no-op (production environments should set variables externally).
pub fn load_development_env() {
    #[cfg(debug_assertions)]
    {
        if let Ok(content) = std::fs::read_to_string(".env") {
            for line in content.lines() {
                // Skip comments and empty lines
                let trimmed = line.trim();
                if trimmed.starts_with('#') || trimmed.is_empty() {
                    continue;
                }

                // Parse KEY=VALUE format
                if let Some((key, value)) = trimmed.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();

                    // Only set if not already set
                    if std::env::var(key).is_err() {
                        // TODO: Audit that the environment access only happens in single-threaded code.
                        unsafe { std::env::set_var(key, value) };
                    }
                }
            }
        }
    }

    // No-op in release builds - production environments should set environment variables externally
}

/// Helper to get service port from configuration or environment
pub fn get_service_port(config_port: u16, service: &ServiceInfo) -> u16 {
    // Check if config port is default
    let is_default = config_port == 0 || config_port == service.default_port;

    if is_default {
        // Try SERVICE_PORT first (unified across all services)
        if let Ok(port) = std::env::var("SERVICE_PORT")
            && let Ok(p) = port.parse::<u16>()
        {
            return p;
        }

        // Fallback to service-specific environment variable
        let env_var = format!("{}_PORT", service.name.to_uppercase());
        if let Ok(port) = std::env::var(&env_var)
            && let Ok(p) = port.parse::<u16>()
        {
            return p;
        }
    }

    // Return config port or default
    if config_port > 0 {
        config_port
    } else {
        service.default_port
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_service_info_creation() {
        let service = ServiceInfo::new("test_service", 8080);
        assert_eq!(service.name, "test_service");
        assert_eq!(service.default_port, 8080);
    }
}
