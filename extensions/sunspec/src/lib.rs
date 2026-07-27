//! Optional SunSpec information-model catalog and Modbus point expansion.
//!
//! Transport and runtime ownership remain in the composition root. This crate
//! contains only protocol-specific model data and deterministic expansion.

mod expand;
mod model;
mod types;

pub use expand::{DiscoveredModel, ExpandConfig, ExpandFilter, ExpandedPoint, expand_model};
pub use model::{list_model_ids, load_model, model_exists};
pub use types::{SunSpecGroup, SunSpecModel, SunSpecPoint};

/// Errors produced while resolving an embedded SunSpec model.
#[derive(Debug, thiserror::Error)]
pub enum SunSpecError {
    /// The requested model is absent from the optional catalog.
    #[error("SunSpec model {0} was not found")]
    ModelNotFound(u16),
    /// One vendored model failed to deserialize.
    #[error("failed to parse SunSpec model {model_id}: {source}")]
    ModelParsing {
        model_id: u16,
        #[source]
        source: serde_json::Error,
    },
}

/// Result type for SunSpec model operations.
pub type Result<T> = std::result::Result<T, SunSpecError>;
