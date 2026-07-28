//! Shared Serde defaults and flexible physical-point deserializers.

use serde::{Deserialize, Deserializer};

pub fn bool_true() -> bool {
    true
}

pub fn bool_false() -> bool {
    false
}

pub fn scale_one() -> f64 {
    1.0
}

pub fn step_one() -> f64 {
    1.0
}

/// Deserialize native booleans, integer 0/1, and common CSV boolean strings.
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
        BoolOrStringOrInt::Bool(value) => Ok(value),
        BoolOrStringOrInt::Int(0) => Ok(false),
        BoolOrStringOrInt::Int(1) => Ok(true),
        BoolOrStringOrInt::Int(value) => Err(D::Error::custom(format!(
            "Invalid integer value {value}, expected 0 or 1"
        ))),
        BoolOrStringOrInt::String(value) => {
            let value = value.trim();
            if value == "1"
                || value.eq_ignore_ascii_case("true")
                || value.eq_ignore_ascii_case("yes")
            {
                Ok(true)
            } else if value.is_empty()
                || value == "0"
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("no")
            {
                Ok(false)
            } else {
                Err(D::Error::custom(format!(
                    "Invalid boolean value '{value}', expected: 1/0, true/false, yes/no, or boolean"
                )))
            }
        },
    }
}

fn deserialize_f64_or_default<'de, D>(deserializer: D) -> Result<f64, D::Error>
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
        StringOrFloat::Float(value) => Ok(value),
        StringOrFloat::String(value) if value.trim().is_empty() => Ok(0.0),
        StringOrFloat::String(value) => value
            .trim()
            .parse::<f64>()
            .map_err(serde::de::Error::custom),
    }
}

pub fn deserialize_scale<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_f64_or_default(deserializer).map(|value| if value == 0.0 { 1.0 } else { value })
}

pub fn deserialize_offset<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_f64_or_default(deserializer)
}
