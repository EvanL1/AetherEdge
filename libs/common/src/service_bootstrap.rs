//! Unified service bootstrap utilities
//!
//! Provides common initialization functionality for all AetherEMS services,
//! including startup banners, logging initialization, and environment setup.

use crate::logging::{self, LogConfig};
use tracing::{Level, debug, info};

/// Service metadata for startup
pub struct ServiceInfo {
    /// Canonical service name (for example, `aether-io`).
    pub name: String,
    /// Service version from Cargo.toml
    pub version: String,
    /// Service description
    pub description: String,
    /// Default port
    pub default_port: u16,
}

impl ServiceInfo {
    /// Create new service info
    pub fn new(name: impl Into<String>, description: impl Into<String>, default_port: u16) -> Self {
        Self {
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: description.into(),
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

/// Initialize logging for a service with standard configuration
///
/// # Arguments
/// * `service` - Service metadata
/// * `logging_config` - Optional logging configuration from SQLite config
///
/// Log root directory priority:
/// 1. AETHER_LOG_DIR environment variable
/// 2. logging_config.dir from SQLite config
/// 3. Default "logs"
pub fn init_logging(
    service: &ServiceInfo,
    logging_config: Option<&crate::LoggingConfig>,
) -> anyhow::Result<()> {
    // Initialize log root directory from config or environment
    let config_dir = logging_config.map(|c| c.dir.as_str());
    crate::logging::init_log_root(config_dir);

    // Check RUST_LOG environment variable for log level
    let console_level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<Level>().ok())
        .unwrap_or(Level::INFO);

    // Get log directory with service name subdirectory
    let log_dir = crate::logging::get_log_root().join(&service.name);

    let log_config = LogConfig {
        service_name: service.name.clone(),
        log_dir,
        file_level: Level::DEBUG,
        console_level,
        enable_json: false,
        max_log_files: 30,
        enable_api_log: true,
        api_log_level: Level::INFO,
    };

    // Initialize the logging system
    logging::init_with_config(log_config).map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
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

/// Get service configuration path from environment or default
pub fn get_config_path(service: &ServiceInfo) -> String {
    // Try service-specific environment variable first
    let env_var = format!("{}_DB_PATH", service.name.to_uppercase());

    if let Ok(path) = std::env::var(&env_var) {
        return path;
    }

    // Try DATABASE_DIR for all services
    if let Ok(dir) = std::env::var("DATABASE_DIR") {
        return format!("{}/{}.db", dir, service.name);
    }

    // Default path
    format!("data/{}.db", service.name)
}

/// Standard service startup sequence
pub async fn bootstrap_service(service: ServiceInfo) -> anyhow::Result<String> {
    // Load development environment
    load_development_env();

    // Initialize logging (config not loaded yet, use env/default)
    init_logging(&service, None)?;

    // Print startup banner
    print_startup_banner(&service);

    // Get configuration path
    let config_path = get_config_path(&service);

    // Check if database exists
    if !std::path::Path::new(&config_path).exists() {
        anyhow::bail!(
            "Configuration database not found at: {}\nPlease run: aether sync {}",
            config_path,
            service.name
        );
    }

    debug!("Config: {}", config_path);

    Ok(config_path)
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
        let service = ServiceInfo::new("test_service", "Test Service", 8080);
        assert_eq!(service.name, "test_service");
        assert_eq!(service.description, "Test Service");
        assert_eq!(service.default_port, 8080);
    }

    #[test]
    fn test_get_config_path_default() {
        let service = ServiceInfo::new("testservice", "Test", 8080);
        let path = get_config_path(&service);
        assert_eq!(path, "data/testservice.db");
    }
}
