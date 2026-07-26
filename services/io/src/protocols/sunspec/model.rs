//! SunSpec model loading from the compile-time embedded protocol catalog.

use thiserror::Error;

use super::types::SunSpecModel;

include!(concat!(env!("OUT_DIR"), "/sunspec_models.rs"));

/// Fail-closed error while resolving an embedded SunSpec model.
#[derive(Debug, Error)]
pub enum SunSpecModelError {
    /// No embedded JSON document exists for the requested model identifier.
    #[error("SunSpec model {0} is not embedded")]
    NotFound(u16),
    /// An embedded model document failed its typed JSON contract.
    #[error("SunSpec model {model_id} is invalid: {source}")]
    InvalidJson {
        /// Requested SunSpec model identifier.
        model_id: u16,
        /// JSON decoding failure.
        #[source]
        source: serde_json::Error,
    },
}

/// Load a SunSpec model definition by ID.
pub fn load_model(model_id: u16) -> Result<SunSpecModel, SunSpecModelError> {
    let json = SUNSPEC_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, json)| *json)
        .ok_or(SunSpecModelError::NotFound(model_id))?;

    serde_json::from_str(json).map_err(|source| SunSpecModelError::InvalidJson { model_id, source })
}

/// List all embedded SunSpec model IDs.
pub fn list_model_ids() -> Vec<u16> {
    SUNSPEC_MODELS.iter().map(|(id, _)| *id).collect()
}

/// Check whether a model JSON exists in the embedded library.
pub fn model_exists(model_id: u16) -> bool {
    SUNSPEC_MODELS.iter().any(|(id, _)| *id == model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_complete_sorted_and_unique() {
        let model_ids = list_model_ids();

        assert_eq!(model_ids.len(), 112);
        assert!(model_ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn load_model_103() {
        let model = load_model(103).expect("model 103");
        assert_eq!(model.id, 103);
        assert_eq!(model.group.name, "inverter_three_phase");
    }

    #[test]
    fn load_model_701() {
        let model = load_model(701).expect("model 701");
        assert_eq!(model.id, 701);
    }

    #[test]
    fn missing_model_returns_error() {
        assert!(load_model(65_535).is_err());
    }
}
