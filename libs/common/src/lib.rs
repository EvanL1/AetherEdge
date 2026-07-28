//! Shared service bootstrap, configuration, logging, and API utilities.

pub mod point_type;
pub mod service_config;
pub mod service_ports;
#[cfg(feature = "sqlite")]
pub mod site_schema;
#[cfg(feature = "sqlite")]
pub mod sqlite;

// Common modules
pub mod admin_api;
pub mod api_types;
pub mod automation_config;
pub mod config_loader;
pub mod io_config;
pub mod logging;
mod serde_helpers;
pub mod service_bootstrap;
pub mod shutdown;
pub mod system_metrics;
pub mod validation;

// Re-export commonly used csv types (previously in csv.rs module)
pub use csv::{Reader, ReaderBuilder, StringRecord, Writer, WriterBuilder};

// Re-export commonly used service_config types at crate root for convenience
pub use service_config::{
    // Config types
    ApiConfig,
    BaseServiceConfig,
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
    LOCALHOST_HOST,
    PointRole,
    PointType,
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

// Re-export commonly used API types
pub use api_types::{
    // Response types
    ComponentHealth,
    ErrorInfo,
    ErrorResponse,
    HealthStatus,
    PaginatedResponse,
    ServiceStatus,
    SuccessResponse,
};

// Re-export AppError when axum feature is enabled
#[cfg(feature = "axum")]
pub use api_types::AppError;

// Startup dependency checker
#[cfg(feature = "dependency")]
pub mod dependency;

// Bootstrap modules
pub mod bootstrap_args;
pub mod bootstrap_database;
pub mod bootstrap_system;

// Re-export common dependencies
pub use anyhow;
pub use serde;
pub use serde_json;
pub use tokio;
