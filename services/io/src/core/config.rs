//! # Configuration Management Module
//!
//! This module provides configuration management for the communication service.
//!
//! ## Features
//!
//! - **Multi-format support**: YAML, TOML, JSON auto-detection
//! - **Type-safe**: Compile-time validation
//! - **CSV point tables**: Support for loading point definitions from CSV files
//! - **SQLite database**: Support for loading configuration from SQLite
//! - **Environment variables**: Override configuration with environment variables
//!
//! ## Architecture
//!
//! ```text
//! ConfigManager
//!   ├── Service Configuration
//!   ├── Channel Configuration
//!   └── Point Tables (CSV/SQLite)
//! ```

#![allow(ambiguous_glob_reexports)]

pub mod manager;
pub mod sqlite_loader;
pub mod types;

// Re-export from modules
pub use manager::*;
pub use sqlite_loader::IoSqliteLoader;

// Re-export io configuration types
pub use types::{
    // Table SQL constants
    ADJUSTMENT_POINTS_TABLE,
    AdjustmentPoint,
    CHANNEL_ROUTING_TABLE,
    CHANNELS_TABLE,
    CONTROL_POINTS_TABLE,
    CanMapping,
    ChannelConfig,
    ChannelCore,
    ChannelLoggingConfig,
    ControlPoint,
    DEFAULT_PORT,
    GpioMapping,
    GrpcMapping,
    IecMapping,
    IoConfig,
    IoValidator,
    JSON_POINT_MAPPINGS_TABLE,
    ModbusMapping,
    Point,
    RuntimeChannelConfig,
    SERVICE_CONFIG_TABLE,
    SIGNAL_POINTS_TABLE,
    SYNC_METADATA_TABLE,
    SignalPoint,
    SqlInsertablePoint,
    TELEMETRY_POINTS_TABLE,
    TelemetryPoint,
    VirtualMapping,
};

// Re-export common configuration types
pub use common::{ApiConfig, BaseServiceConfig, FourRemote, LoggingConfig};

// Legacy aliases for backward compatibility
pub type AppConfig = IoConfig;
pub type ServiceConfig = BaseServiceConfig;
