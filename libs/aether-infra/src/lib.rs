//! Aether Infrastructure Layer
//!
//! This library provides database infrastructure for AetherEdge:
//! - SQLite client with optimized settings
//!
//! # Features
//!
//! - `sqlite` - Enable SQLite client (default)

#[cfg(feature = "sqlite")]
pub mod sqlite;
