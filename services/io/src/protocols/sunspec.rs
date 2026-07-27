//! SunSpec Modbus discovery and client helpers.

#[cfg(feature = "modbus")]
mod discovery;

#[cfg(feature = "modbus")]
pub use discovery::{CANDIDATE_BASES, connect_modbus, discover_models};
