//! SQLite client module
//!
//! Provides SQLite client with optimized settings for edge deployment.

pub mod client;
pub mod service_config;

pub use client::{SqliteClient, SqlitePool};
pub use service_config::{ServiceConfig, ServiceConfigLoader, migrate_yaml_to_db};
