//! # SQLite-backed IO configuration
//!
//! The composition root creates the process-wide SQLite pool. `IoSqliteLoader`
//! reads process settings or complete immutable channel snapshots from it;
//! protocol runtimes never reopen the database.
//!
//! ## Architecture
//!
//! ```text
//! IoSqliteLoader
//!   ├── Service/API Configuration
//!   └── Complete Runtime Channel Snapshots
//! ```

pub mod sqlite_loader;

// Re-export from modules
pub use sqlite_loader::IoSqliteLoader;

// Re-export io configuration types
pub use aether_config::io::{
    // Table SQL constants
    ADJUSTMENT_POINTS_TABLE,
    AdjustmentPoint,
    CHANNEL_ID_INSERT_GUARD_TRIGGER,
    CHANNEL_ID_UPDATE_GUARD_TRIGGER,
    CHANNEL_REVISION_DELETE_GUARD_TRIGGER,
    CHANNEL_REVISION_INSERT_GUARD_TRIGGER,
    CHANNEL_REVISION_STEP_GUARD_TRIGGER,
    CHANNEL_REVISION_UPDATE_GUARD_TRIGGER,
    CHANNEL_ROUTING_TABLE,
    CHANNELS_TABLE,
    CONTROL_POINTS_TABLE,
    ChannelConfig,
    ChannelCore,
    ChannelLoggingConfig,
    ControlPoint,
    DEFAULT_PORT,
    Point,
    SERVICE_CONFIG_TABLE,
    SIGNAL_POINTS_TABLE,
    SYNC_METADATA_TABLE,
    SignalPoint,
    SqlInsertablePoint,
    TELEMETRY_POINTS_TABLE,
    TelemetryPoint,
    install_channel_revision_guards,
};

// Re-export common configuration types
pub use common::{ApiConfig, BaseServiceConfig, FourRemote, LoggingConfig};

pub type ServiceConfig = BaseServiceConfig;
