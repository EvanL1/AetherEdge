//! SunSpec model catalog, expansion, and Modbus discovery helpers.
//!
//! SunSpec is a protocol-specific compatibility concern owned by the I/O
//! adapter. It must not leak into `aether-domain` or a generic model crate.

mod expand;
mod model;
mod types;

#[cfg(feature = "modbus")]
mod discovery;

#[cfg(feature = "modbus")]
pub use discovery::{CANDIDATE_BASES, connect_modbus, discover_models};
pub use expand::{DiscoveredModel, ExpandConfig, ExpandFilter, ExpandedPoint, expand_model};
pub use model::{SunSpecModelError, list_model_ids, load_model, model_exists};
pub use types::{SunSpecGroup, SunSpecModel, SunSpecPoint};
