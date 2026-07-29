//! SQLite-backed IO configuration and runtime channel snapshots.

pub mod sqlite_loader;

pub use common::io_config::{
    AdjustmentPoint, ChannelConfig, ChannelCore, ChannelLoggingConfig, ControlPoint, DEFAULT_PORT,
    IoConfig, Point, RuntimeChannelConfig, SignalPoint, TelemetryPoint,
};
pub use sqlite_loader::IoSqliteLoader;
