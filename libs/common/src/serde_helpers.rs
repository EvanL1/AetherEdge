//! Shared Serde default-value helpers.

// ============================================================================
// Default Value Functions (for serde #[serde(default = "...")] attributes)
// ============================================================================

/// Default value: true
pub fn bool_true() -> bool {
    true
}

/// Default value: false
pub fn bool_false() -> bool {
    false
}

/// Default scale factor: 1.0
pub fn scale_one() -> f64 {
    1.0
}

/// Default step value: 1.0
pub fn step_one() -> f64 {
    1.0
}
