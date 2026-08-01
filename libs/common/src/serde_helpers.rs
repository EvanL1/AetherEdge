//! Shared Serde deserializers
//!
//! Custom deserializers for handling optional fields in API requests.
//! Supports multiple input formats:
//! - `null` → None
//! - `""` (empty string) → None
//! - String number `"123"` → Some(123)
//! - Native number `123` → Some(123)

use serde::{Deserialize, Deserializer};

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

// ============================================================================
// Custom Deserializers (for CSV/JSON parsing)
// ============================================================================

/// Custom deserializer for boolean fields that supports multiple input formats
///
/// Supports native JSON booleans, integers, and string values:
/// - JSON boolean: true, false
/// - JSON integer: 0 (false), 1 (true)
/// - CSV string: "1"/"0", "true"/"false", "yes"/"no" (case-insensitive)
pub fn deserialize_bool_flexible<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrStringOrInt {
        Bool(bool),
        Int(i64),
        String(String),
    }

    match BoolOrStringOrInt::deserialize(deserializer)? {
        BoolOrStringOrInt::Bool(b) => Ok(b),
        BoolOrStringOrInt::Int(i) => match i {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(D::Error::custom(format!(
                "Invalid integer value {}, expected 0 or 1",
                i
            ))),
        },
        // Optimization: trim first, then use eq_ignore_ascii_case (zero allocation)
        BoolOrStringOrInt::String(s) => {
            let t = s.trim();
            if t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes") {
                Ok(true)
            } else if t.is_empty()
                || t == "0"
                || t.eq_ignore_ascii_case("false")
                || t.eq_ignore_ascii_case("no")
            {
                Ok(false)
            } else {
                Err(D::Error::custom(format!(
                    "Invalid boolean value '{}', expected: 1/0, true/false, yes/no, or boolean",
                    s
                )))
            }
        },
    }
}

/// Custom deserializer for f64 that treats empty strings as default value (0.0)
pub fn deserialize_f64_or_default<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrFloat {
        String(String),
        Float(f64),
    }

    match StringOrFloat::deserialize(deserializer)? {
        StringOrFloat::Float(f) => Ok(f),
        StringOrFloat::String(s) => {
            if s.trim().is_empty() {
                Ok(0.0) // Empty string => 0.0 (offset default)
            } else {
                s.trim().parse::<f64>().map_err(serde::de::Error::custom)
            }
        },
    }
}

/// Deserialize scale with default 1.0 for empty strings
pub fn deserialize_scale<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_f64_or_default(deserializer).map(|v| if v == 0.0 { 1.0 } else { v })
}

/// Deserialize offset with default 0.0 for empty strings
pub fn deserialize_offset<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_f64_or_default(deserializer)
}
