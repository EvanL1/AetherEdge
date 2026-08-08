//! `AetherEdge` basic library (basic library)
//!
//! Provides basic functions shared by all services, including:
//! - monitoring and health checking
//! - logging functions
//! - service configuration types

#[cfg(feature = "sqlite")]
pub mod sqlite;

pub mod service_config;
pub mod service_ports;

// Common modules
pub mod admin_api;
pub mod api_types;
pub mod audit_api;
pub mod log_rotation;
pub mod logging;
pub mod serde_helpers;
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
    LOCALHOST_HOST,
    LogRotationConfig,
    LoggingConfig,
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
    // Listener address construction
    bind_address,
    // Environment fallback helper
    env_or,
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
    TimeRange,
    // Helpers
    openapi_operation_count,
};

// Re-export AppError when axum feature is enabled
#[cfg(feature = "axum")]
pub use api_types::{AppError, app_error_from, status_for_error_category};

// Bootstrap modules
pub mod bootstrap_args;
pub mod bootstrap_database;
pub mod bootstrap_system;

// Canonical SQLite schema (DDL constants, triggers and bootstrap helpers).
// Production startup paths depend on this; it is not test-only.
#[cfg(feature = "sqlite")]
pub mod schema;

// Re-export common dependencies
pub use anyhow;
pub use serde;
pub use serde_json;
pub use tokio;
