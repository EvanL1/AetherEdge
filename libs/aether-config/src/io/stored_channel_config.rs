//! Canonical JSON payload stored in `channels.config`.
//!
//! Channel identity and lifecycle fields live in dedicated SQLite columns. This
//! payload contains only the optional description, opaque protocol parameters,
//! and channel logging policy.

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;

use serde_json::Value;

use super::{ChannelConfig, ChannelLoggingConfig};

/// Protocol-independent payload stored in the `channels.config` JSON column.
#[derive(Debug, Clone, Default)]
pub struct StoredChannelConfig {
    /// Optional operator-facing channel description.
    pub description: Option<String>,
    /// Opaque adapter-owned parameters.
    pub parameters: HashMap<String, Value>,
    /// Channel diagnostic logging policy.
    pub logging: ChannelLoggingConfig,
}

impl StoredChannelConfig {
    /// Decode a nullable SQLite `channels.config` value.
    ///
    /// A SQL `NULL` payload is treated as an empty payload. Unknown top-level
    /// fields are ignored for compatibility with older writers.
    pub fn decode(raw: Option<&str>) -> Result<Self, StoredChannelConfigError> {
        match raw {
            Some(raw) => {
                let value =
                    serde_json::from_str(raw).map_err(StoredChannelConfigError::InvalidJson)?;
                Self::from_value(value)
            },
            None => Ok(Self::default()),
        }
    }

    /// Extract the persisted payload fields from a JSON object.
    ///
    /// This also accepts a complete serialized [`super::ChannelConfig`];
    /// identity fields are ignored because they are stored in dedicated
    /// columns.
    pub fn from_value(value: Value) -> Result<Self, StoredChannelConfigError> {
        let Value::Object(mut object) = value else {
            return Err(StoredChannelConfigError::ExpectedObject);
        };

        let description = match object.remove("description") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value),
            Some(_) => return Err(StoredChannelConfigError::InvalidDescription),
        };

        let parameters = match object.remove("parameters") {
            None => HashMap::new(),
            Some(Value::Object(values)) => {
                for value in values.values() {
                    validate_parameter_value(value)?;
                }
                values.into_iter().collect()
            },
            Some(_) => return Err(StoredChannelConfigError::InvalidParameters),
        };

        let logging = match object.remove("logging") {
            None => ChannelLoggingConfig::default(),
            Some(value) => {
                serde_json::from_value(value).map_err(StoredChannelConfigError::InvalidLogging)?
            },
        };

        Ok(Self {
            description,
            parameters,
            logging,
        })
    }

    /// Encode the payload with recursively sorted object keys.
    pub fn encode(&self) -> Result<String, StoredChannelConfigError> {
        for value in self.parameters.values() {
            validate_parameter_value(value)?;
        }

        let mut parameters = serde_json::Map::new();
        let mut parameter_keys = self.parameters.keys().collect::<Vec<_>>();
        parameter_keys.sort_unstable();
        for key in parameter_keys {
            if let Some(value) = self.parameters.get(key) {
                parameters.insert(key.clone(), canonicalize_json(value));
            }
        }

        let logging =
            serde_json::to_value(&self.logging).map_err(StoredChannelConfigError::Serialization)?;
        let mut payload = BTreeMap::new();
        if let Some(description) = &self.description {
            payload.insert("description", Value::String(description.clone()));
        }
        payload.insert("logging", canonicalize_json(&logging));
        payload.insert("parameters", Value::Object(parameters));

        serde_json::to_string(&payload).map_err(StoredChannelConfigError::Serialization)
    }
}

impl From<&ChannelConfig> for StoredChannelConfig {
    fn from(config: &ChannelConfig) -> Self {
        Self {
            description: config.core.description.clone(),
            parameters: config.parameters.clone(),
            logging: config.logging.clone(),
        }
    }
}

/// Failure to decode or encode a stored channel payload.
#[derive(Debug)]
pub enum StoredChannelConfigError {
    /// The SQLite text is not valid JSON.
    InvalidJson(serde_json::Error),
    /// The JSON root is not an object.
    ExpectedObject,
    /// `description` is neither a string nor null.
    InvalidDescription,
    /// `parameters` is not an object.
    InvalidParameters,
    /// A parameter integer cannot be represented by the governed channel API.
    ParameterIntegerOutOfRange,
    /// The logging policy does not match [`ChannelLoggingConfig`].
    InvalidLogging(serde_json::Error),
    /// A validated payload could not be serialized.
    Serialization(serde_json::Error),
}

impl fmt::Display for StoredChannelConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidJson(_) => "stored channel configuration is not valid JSON",
            Self::ExpectedObject => "stored channel configuration must be a JSON object",
            Self::InvalidDescription => "stored channel description must be a string or null",
            Self::InvalidParameters => "stored channel parameters must be a JSON object",
            Self::ParameterIntegerOutOfRange => {
                "stored channel parameter integer exceeds the supported range"
            },
            Self::InvalidLogging(_) => "stored channel logging policy is invalid",
            Self::Serialization(_) => "stored channel configuration cannot be serialized",
        };
        formatter.write_str(message)
    }
}

impl Error for StoredChannelConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(source)
            | Self::InvalidLogging(source)
            | Self::Serialization(source) => Some(source),
            Self::ExpectedObject
            | Self::InvalidDescription
            | Self::InvalidParameters
            | Self::ParameterIntegerOutOfRange => None,
        }
    }
}

fn validate_parameter_value(value: &Value) -> Result<(), StoredChannelConfigError> {
    match value {
        Value::Number(number) => {
            if number.as_i64().is_some() {
                return Ok(());
            }
            if number.as_u64().is_some() {
                return Err(StoredChannelConfigError::ParameterIntegerOutOfRange);
            }
            if number.as_f64().is_some_and(f64::is_finite) {
                Ok(())
            } else {
                Err(StoredChannelConfigError::ParameterIntegerOutOfRange)
            }
        },
        Value::Array(values) => values.iter().try_for_each(validate_parameter_value),
        Value::Object(values) => values.values().try_for_each(validate_parameter_value),
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
    }
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                if let Some(value) = values.get(key) {
                    canonical.insert(key.clone(), canonicalize_json(value));
                }
            }
            Value::Object(canonical)
        },
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_and_missing_description_decode_as_none() {
        let missing = StoredChannelConfig::decode(None).expect("SQL NULL payload");
        assert!(missing.description.is_none());
        assert!(missing.parameters.is_empty());
        assert!(!missing.logging.enabled);

        let explicit_null =
            StoredChannelConfig::decode(Some(r#"{"description":null}"#)).expect("null description");
        assert!(explicit_null.description.is_none());
    }

    #[test]
    fn decode_is_strict_for_known_payload_fields() {
        assert!(matches!(
            StoredChannelConfig::decode(Some("[]")),
            Err(StoredChannelConfigError::ExpectedObject)
        ));
        assert!(matches!(
            StoredChannelConfig::decode(Some(r#"{"description":7}"#)),
            Err(StoredChannelConfigError::InvalidDescription)
        ));
        assert!(matches!(
            StoredChannelConfig::decode(Some(r#"{"parameters":null}"#)),
            Err(StoredChannelConfigError::InvalidParameters)
        ));
        assert!(matches!(
            StoredChannelConfig::decode(Some(r#"{"logging":"debug"}"#)),
            Err(StoredChannelConfigError::InvalidLogging(_))
        ));
        assert!(matches!(
            StoredChannelConfig::decode(Some("{")),
            Err(StoredChannelConfigError::InvalidJson(_))
        ));
    }

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        let decoded = StoredChannelConfig::decode(Some(
            r#"{
                "description":"edge",
                "parameters":{"port":502},
                "future":{"enabled":true}
            }"#,
        ))
        .expect("compatible payload");

        assert_eq!(decoded.description.as_deref(), Some("edge"));
        assert_eq!(decoded.parameters.get("port"), Some(&Value::from(502)));
        assert!(
            !decoded
                .encode()
                .expect("canonical payload")
                .contains("future")
        );
    }

    #[test]
    fn nested_parameter_integer_must_fit_i64() {
        let error = StoredChannelConfig::decode(Some(
            r#"{"parameters":{"nested":[{"value":9223372036854775808}]}}"#,
        ))
        .expect_err("out-of-range integer");
        assert!(matches!(
            error,
            StoredChannelConfigError::ParameterIntegerOutOfRange
        ));

        StoredChannelConfig::decode(Some(
            r#"{"parameters":{"min":-9223372036854775808,"max":9223372036854775807,"ratio":1.5}}"#,
        ))
        .expect("supported numeric domain");
    }

    #[test]
    fn encoding_is_deterministic_and_recursively_canonical() {
        let first = StoredChannelConfig {
            description: Some("edge".to_owned()),
            parameters: HashMap::from([
                ("z".to_owned(), serde_json::json!({"second": 2, "first": 1})),
                ("a".to_owned(), serde_json::json!([{"b": 2, "a": 1}])),
            ]),
            logging: ChannelLoggingConfig {
                enabled: true,
                level: Some("debug".to_owned()),
                file: None,
            },
        };
        let second = StoredChannelConfig {
            description: first.description.clone(),
            parameters: HashMap::from([
                ("a".to_owned(), serde_json::json!([{"a": 1, "b": 2}])),
                ("z".to_owned(), serde_json::json!({"first": 1, "second": 2})),
            ]),
            logging: first.logging.clone(),
        };

        let first_json = first.encode().expect("first payload");
        let second_json = second.encode().expect("second payload");
        assert_eq!(first_json, second_json);
        assert_eq!(
            first_json,
            r#"{"description":"edge","logging":{"enabled":true,"file":null,"level":"debug"},"parameters":{"a":[{"a":1,"b":2}],"z":{"first":1,"second":2}}}"#
        );
    }
}
