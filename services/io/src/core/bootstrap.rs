//! Service Bootstrap and Initialization
//!
//! This module handles service initialization including:
//! - Logging configuration
//! - Configuration validation
//!
//! Uses common bootstrap utilities for shared functionality

use clap::Parser;
use tracing::{debug, info};

use crate::core::config::DEFAULT_PORT;
use common::DEFAULT_API_HOST;
use common::service_bootstrap::ServiceInfo;
use errors::{AetherError, AetherResult};

use crate::core::config::ConfigManager;

// Re-export common bootstrap functionality
pub use common::bootstrap_args::ServiceArgs;
pub use common::bootstrap_system::check_system_requirements;

/// Command-line arguments for io
#[derive(Parser, Clone)]
#[command(
    name = "aether-io",
    version = env!("CARGO_PKG_VERSION"),
    about = "Industrial Communication Service",
    long_about = None
)]
pub struct Args {
    /// Log level (trace, debug, info, warn, error)
    #[arg(short = 'l', long, default_value = "info")]
    pub log_level: String,

    /// Bind address for API server
    #[arg(short = 'b', long)]
    pub bind_address: Option<String>,

    /// Enable debug mode
    #[arg(short = 'd', long)]
    pub debug: bool,

    /// Disable colored output
    #[arg(long)]
    pub no_color: bool,

    /// Validation mode - only validate configuration without starting service
    #[arg(long)]
    pub validate: bool,
}

impl From<Args> for ServiceArgs {
    fn from(args: Args) -> Self {
        ServiceArgs {
            log_level: args.log_level,
            bind_address: args.bind_address,
            debug: args.debug,
            no_color: args.no_color,
            validate: args.validate,
            ..ServiceArgs::default()
        }
    }
}

/// Initialize logging system with command-line arguments
///
/// Wraps common functionality with service-specific configuration.
/// Log root directory priority:
/// 1. AETHER_LOG_DIR environment variable
/// 2. logging_config.dir from SQLite config
/// 3. Default "logs"
pub fn initialize_logging(
    args: &ServiceArgs,
    service_info: &ServiceInfo,
    logging_config: Option<&common::LoggingConfig>,
) -> AetherResult<()> {
    // Load environment variables from .env file in development mode
    common::service_bootstrap::load_development_env();

    // Initialize log root directory from config or environment
    let config_dir = logging_config.map(|c| c.dir.as_str());
    common::logging::init_log_root(config_dir);

    // Use common arg parsing
    let console_level = args.parse_log_level();

    // Get log directory with service name subdirectory
    let log_dir = common::logging::get_log_root().join(&service_info.name);

    let log_config = common::logging::LogConfig {
        service_name: service_info.name.clone(),
        log_dir,
        console_level,
        file_level: tracing::Level::DEBUG,
        enable_json: false,
        max_log_files: 30,
        enable_api_log: true,
        api_log_level: tracing::Level::INFO,
    };

    common::logging::init_with_config(log_config)
        .map_err(|e| AetherError::Configuration(format!("Failed to init logging: {}", e)))?;
    Ok(())
}

/// Validate configuration from SQLite database
pub async fn validate_configuration() -> AetherResult<()> {
    debug!("Validating configuration from SQLite database");

    // Load and validate configuration
    let config_manager = ConfigManager::load().await?;
    debug!("Configuration loaded successfully");

    // Validate service configuration
    let service_config = config_manager.service_config();
    info!("Service: {}", service_config.name);
    if let Some(desc) = &service_config.description {
        info!("Description: {}", desc);
    }

    // Validate channels
    let channels = config_manager.channels();
    info!("Found {} channel(s)", channels.len());

    for channel in channels {
        info!(
            "  Channel {}: {} (protocol: {})",
            channel.id(),
            channel.name(),
            channel.protocol()
        );

        // Note: Point counts will be loaded at runtime from SQLite
        info!("    Points will be loaded from SQLite at runtime");
    }

    info!("Configuration validation completed successfully");
    Ok(())
}

/// Determine bind address from multiple sources
/// Priority: CLI > Config > ENV > Default
pub fn determine_bind_address(
    cli_arg: Option<String>,
    config_host: &str,
    config_port: u16,
) -> String {
    if let Some(addr) = cli_arg {
        info!("Using bind address from command line: {}", addr);
        return addr;
    }

    // Check if configuration specifies port (non-default)
    let is_config_default = config_port == DEFAULT_PORT || config_port == 0;

    if !is_config_default {
        let config_addr = format!("{}:{}", config_host, config_port);
        info!("Using bind address from configuration: {}", config_addr);
        return config_addr;
    }

    // Try environment variables
    let port = common::config_loader::get_config_value(
        Some(config_port),
        is_config_default,
        "SERVICE_PORT",
        DEFAULT_PORT,
    );

    let host = common::config_loader::get_string_config(
        Some(config_host.to_string()),
        config_host.is_empty(),
        "SERVICE_HOST",
        DEFAULT_API_HOST.to_string(),
    );

    format!("{}:{}", host, port)
}
