//! `AetherEMS` basic library (basic library)
//!
//! Provides basic functions shared by all services, including:
//! - Redis client
//! - monitoring and health checking
//! - logging functions
//! - service configuration types

// Re-export from aether-infra for backward compatibility
#[cfg(feature = "redis")]
pub use aether_infra::redis;

#[cfg(feature = "sqlite")]
pub use aether_infra::sqlite;

pub mod service_config;

// Common modules
pub mod admin_api;
pub mod api_types;
pub mod config_loader;
pub mod log_rotation;
pub mod logging;
pub mod serde_helpers;
pub mod service_bootstrap;
pub mod shutdown;
pub mod system_metrics;
pub mod validation;
#[cfg(feature = "redis")]
pub mod warning_monitor;

// Re-export commonly used csv types (previously in csv.rs module)
pub use csv::{Reader, ReaderBuilder, StringRecord, Writer, WriterBuilder};

// Re-export commonly used service_config types at crate root for convenience
pub use service_config::{
    // Config types
    ApiConfig,
    BaseServiceConfig,
    // Reload
    ChannelReloadResult,
    // Enums
    ComparisonOperator,
    // Validation
    ConfigValidator,
    // Constants
    DEFAULT_API_HOST,
    DEFAULT_AUTOMATION_URL,
    DEFAULT_IO_URL,
    DEFAULT_RULES_URL,
    ENV_AUTOMATION_URL,
    ENV_IO_URL,
    ENV_RULES_URL,
    FourRemote,
    GenericValidator,
    InstanceReloadResult,
    LOCALHOST_HOST,
    LogRotationConfig,
    LoggingConfig,
    PointType,
    ReloadResult,
    ReloadableService,
    RuleReloadResult,
    SERVICE_CONFIG_TABLE,
    SYNC_METADATA_TABLE,
    // Database types
    ServiceConfigRecord,
    SyncMetadataRecord,
    ValidationLevel,
    ValidationResult,
    automation_url,
    // Helpers
    helpers,
    // URL resolver functions
    io_url,
};

#[cfg(feature = "redis")]
pub use service_config::{
    DEFAULT_REDIS_HOST, DEFAULT_REDIS_PORT, DEFAULT_REDIS_URL, RedisConfig, RedisRoutingKeys,
};

// Re-export commonly used API types
pub use api_types::{
    // Response types
    ComponentHealth,
    ErrorInfo,
    ErrorResponse,
    HealthStatus,
    PaginatedResponse,
    PaginationParams,
    ServiceStatus,
    SortOrder,
    SuccessResponse,
    TimeRange,
};

// Re-export AppError when axum feature is enabled
#[cfg(feature = "axum")]
pub use api_types::AppError;

// Re-export PointRole from aether-model (canonical location)
pub use aether_model::PointRole;

// Startup dependency checker
#[cfg(feature = "dependency")]
pub mod dependency;

// Bootstrap modules
pub mod bootstrap_args;
pub mod bootstrap_database;
pub mod bootstrap_system;

// Test utilities (for use in test code only)
pub mod test_utils;

// Re-export common dependencies
pub use anyhow;
pub use serde;
pub use serde_json;
pub use tokio;

// Re-export CLI dependencies when cli feature is enabled
#[cfg(feature = "cli")]
pub use clap;

// Re-export clap derive macros separately for proper macro resolution
#[cfg(feature = "cli")]
pub use clap::{Args, Parser, Subcommand, ValueEnum};

#[cfg(feature = "cli")]
pub use reqwest;

// Pre-import common types
pub mod prelude {
    #[cfg(feature = "redis")]
    pub use crate::redis::RedisClient;
}
