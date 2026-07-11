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

/// Default page size for pagination: 20
pub fn page_size() -> usize {
    20
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

/// Custom deserializer for u8 fields that treats empty strings as 0
pub fn deserialize_u8_default_zero<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let s = String::deserialize(deserializer)?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        Ok(0)
    } else {
        trimmed.parse::<u8>().map_err(D::Error::custom)
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

/// Deserialize optional i32
///
/// Supports the following input formats:
/// - `null` → `None`
/// - `""` → `None`
/// - `123` or `"123"` → `Some(123)`
///
/// # Example
/// ```ignore
/// #[derive(Deserialize)]
/// struct Request {
///     #[serde(default, deserialize_with = "deserialize_optional_i32")]
///     channel_id: Option<i32>,
/// }
/// ```
pub fn deserialize_optional_i32<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i32),
        Null,
    }

    match Option::<StringOrInt>::deserialize(deserializer)? {
        None => Ok(None),
        Some(StringOrInt::Null) => Ok(None),
        Some(StringOrInt::String(s)) if s.is_empty() => Ok(None),
        Some(StringOrInt::String(s)) => s
            .parse::<i32>()
            .map(Some)
            .map_err(|_| D::Error::custom(format!("invalid integer: {}", s))),
        Some(StringOrInt::Int(i)) => Ok(Some(i)),
    }
}

/// Deserialize optional u32
///
/// Supports the following input formats:
/// - `null` → `None`
/// - `""` → `None`
/// - `123` or `"123"` → `Some(123)`
///
/// # Example
/// ```ignore
/// #[derive(Deserialize)]
/// struct Request {
///     #[serde(default, deserialize_with = "deserialize_optional_u32")]
///     point_id: Option<u32>,
/// }
/// ```
pub fn deserialize_optional_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(u32),
        Null,
    }

    match Option::<StringOrInt>::deserialize(deserializer)? {
        None => Ok(None),
        Some(StringOrInt::Null) => Ok(None),
        Some(StringOrInt::String(s)) if s.is_empty() => Ok(None),
        Some(StringOrInt::String(s)) => s
            .parse::<u32>()
            .map(Some)
            .map_err(|_| D::Error::custom(format!("invalid integer: {}", s))),
        Some(StringOrInt::Int(i)) => Ok(Some(i)),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap is acceptable in tests
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct TestI32 {
        #[serde(default, deserialize_with = "deserialize_optional_i32")]
        value: Option<i32>,
    }

    #[derive(Deserialize)]
    struct TestU32 {
        #[serde(default, deserialize_with = "deserialize_optional_u32")]
        value: Option<u32>,
    }

    #[test]
    fn test_optional_i32_null() {
        let json = r#"{"value": null}"#;
        let result: TestI32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, None);
    }

    #[test]
    fn test_optional_i32_empty_string() {
        let json = r#"{"value": ""}"#;
        let result: TestI32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, None);
    }

    #[test]
    fn test_optional_i32_string_number() {
        let json = r#"{"value": "123"}"#;
        let result: TestI32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, Some(123));
    }

    #[test]
    fn test_optional_i32_native_number() {
        let json = r#"{"value": 456}"#;
        let result: TestI32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, Some(456));
    }

    #[test]
    fn test_optional_i32_negative() {
        let json = r#"{"value": "-789"}"#;
        let result: TestI32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, Some(-789));
    }

    #[test]
    fn test_optional_u32_null() {
        let json = r#"{"value": null}"#;
        let result: TestU32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, None);
    }

    #[test]
    fn test_optional_u32_string_number() {
        let json = r#"{"value": "999"}"#;
        let result: TestU32 = serde_json::from_str(json).unwrap();
        assert_eq!(result.value, Some(999));
    }

    #[test]
    fn test_optional_i32_invalid_string() {
        let json = r#"{"value": "not_a_number"}"#;
        let result: Result<TestI32, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
