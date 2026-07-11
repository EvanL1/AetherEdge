//! SunSpec model loading (compile-time embedded JSON).

use crate::error::{ModelError, Result};
use crate::sunspec::types::SunSpecModel;

include!(concat!(env!("OUT_DIR"), "/sunspec_models.rs"));

/// Load a SunSpec model definition by ID.
pub fn load_model(model_id: u16) -> Result<SunSpecModel> {
    let json = SUNSPEC_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, json)| *json)
        .ok_or_else(|| ModelError::ProductNotFound(format!("SunSpec model {model_id}")))?;

    serde_json::from_str(json)
        .map_err(|e| ModelError::ProductParsing(format!("SunSpec model {model_id}: {e}")))
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
