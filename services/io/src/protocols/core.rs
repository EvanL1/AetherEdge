//! Core abstractions for the Industrial Gateway.
//!
//! This module provides the foundational types and traits that all protocols implement.

pub mod data;
pub mod diagnostics;
pub mod error;
pub mod file_logging;
#[cfg(feature = "json-mapping")]
pub mod json_mapper;
pub mod log_handlers;
pub mod logging;
pub mod metadata;
pub mod point;
pub mod quality;
#[cfg(feature = "json-mapping")]
pub mod script_runner;
pub mod slot;
pub mod traits;

pub use data::*;
pub use diagnostics::{AtomicDiagnostics, DiagnosticsSnapshot};
pub use error::{GatewayError, Result};
pub use file_logging::{ChannelFileLogHandler, FileLogLevel};
#[cfg(feature = "json-mapping")]
pub use json_mapper::{JsonMapper, JsonMappingConfig, SharedJsonMapper};
pub use metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType, ProtocolMetadata,
    ProtocolRegistry, get_protocol_registry,
};
pub use point::*;
pub use quality::*;
pub use slot::{AtomicBoolStore, DataSlot, ShardedSlotStore, SlotStore};
pub use traits::*;
