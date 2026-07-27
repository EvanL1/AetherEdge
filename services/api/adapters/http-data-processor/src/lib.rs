//! Bounded HTTP implementation of the Aether [`aether_ports::DataProcessor`] port.
//!
//! The adapter sends one complete, immutable processing request. It exposes no
//! callback through which a processor can read Aether live state, history, or
//! configuration.

mod adapter;
mod config;

pub use adapter::HttpDataProcessor;
pub use config::{BearerSecret, HttpDataProcessorConfig};

/// Data Processing v1 vendor JSON media type.
pub const JSON_MEDIA_TYPE: &str = aether_data_processing::MEDIA_TYPE;
