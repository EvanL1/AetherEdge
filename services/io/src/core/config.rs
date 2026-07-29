//! SQLite-backed IO configuration and runtime channel snapshots.

pub mod sqlite_loader;
pub mod types;

pub use sqlite_loader::IoSqliteLoader;

// Re-export io configuration types
pub use types::{
    // Table SQL constants
    ADJUSTMENT_POINTS_TABLE,
    AdjustmentPoint,
    CHANNEL_REVISION_BUMP_TRIGGER,
    CHANNEL_REVISION_EXHAUSTED_TRIGGER,
    CHANNELS_TABLE,
    CONTROL_POINTS_TABLE,
    ChannelConfig,
    ChannelCore,
    ChannelLoggingConfig,
    ControlPoint,
    DEFAULT_PORT,
    IoConfig,
    Point,
    RuntimeChannelConfig,
    SERVICE_CONFIG_TABLE,
    SIGNAL_POINTS_TABLE,
    SYNC_METADATA_TABLE,
    SignalPoint,
    TELEMETRY_POINTS_TABLE,
    TelemetryPoint,
    install_channel_revision_triggers,
};
