//! Admin API handlers for automation service management
//!
//! Re-exports shared admin handlers from common crate.

pub use common::admin_api::{LogLevelResponse, SetLogLevelRequest, get_log_level, set_log_level};
