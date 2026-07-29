//! Service Bootstrap and Initialization
//!
//! This module handles service initialization including:
//! - Logging configuration
//! - Configuration validation
//!
//! Uses common bootstrap utilities for shared functionality

use clap::Parser;
use tracing::{debug, info, warn};

use crate::core::config::DEFAULT_PORT;
use anyhow::Result;
use common::DEFAULT_API_HOST;
use common::service_bootstrap::ServiceInfo;

use crate::core::config::IoSqliteLoader;

/// Command-line arguments for io
#[derive(Parser)]
#[command(
    name = "aether-io",
    version = env!("CARGO_PKG_VERSION"),
    about = "Industrial Communication Service",
    long_about = None
)]
pub struct Args {
    /// Bind address for API server
    #[arg(short = 'b', long)]
    pub bind_address: Option<String>,

    /// Disable colored output
    #[arg(long)]
    pub no_color: bool,

    /// Validation mode - only validate configuration without starting service
    #[arg(long)]
    pub validate: bool,
}

/// Initialize console-first service logging.
pub fn initialize_logging(service_info: &ServiceInfo) -> Result<()> {
    common::service_bootstrap::load_development_env();
    common::service_bootstrap::init_logging(service_info)
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))
}

/// Validate configuration from SQLite database
pub async fn validate_configuration() -> Result<()> {
    debug!("Validating configuration from SQLite database");

    let db_path = common::bootstrap_args::database_path();
    let config = IoSqliteLoader::new(&db_path).await?.load_config().await?;
    debug!("Configuration loaded successfully");

    let service_config = &config.service;
    info!("Service: {}", service_config.name);
    if let Some(desc) = &service_config.description {
        info!("Description: {}", desc);
    }

    // Validate channels
    info!("Found {} channel(s)", config.channels.len());

    for channel in &config.channels {
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

    let port = std::env::var("SERVICE_PORT")
        .ok()
        .and_then(|value| match value.parse::<u16>() {
            Ok(port) => Some(port),
            Err(error) => {
                warn!(%error, "ignoring invalid SERVICE_PORT");
                None
            },
        })
        .unwrap_or(DEFAULT_PORT);
    let host = if config_host.is_empty() {
        std::env::var("SERVICE_HOST")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_API_HOST.to_string())
    } else {
        config_host.to_string()
    };

    format!("{host}:{port}")
}
