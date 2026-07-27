//! Read-only embedded SQLite implementation of Aether's logical history port.
//!
//! The adapter reads the existing `aether-history` `history` table through
//! commissioned semantic mappings. It applies the task-owned aggregation to
//! bounded raw observations on a cadence-aligned interval-end grid and never
//! creates or migrates schema.

mod adapter;
mod config;

pub use adapter::SqliteHistoryQuery;
pub use config::{
    CalendarFeature, SqliteHistoryFeatureRoute, SqliteHistoryFeatureSource,
    SqliteHistoryQueryConfig,
};
