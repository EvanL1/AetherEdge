//! Aether Infrastructure Layer
//!
//! This library provides database infrastructure for AetherEMS:
//! - Redis client with connection pooling
//! - SQLite client with optimized settings
//!
//! # Features
//!
//! - `redis` - Enable Redis client (default)
//! - `sqlite` - Enable SQLite client (default)

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "sqlite")]
pub mod sqlite;
