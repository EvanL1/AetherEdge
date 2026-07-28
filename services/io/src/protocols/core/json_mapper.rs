//! JSON Payload Mapping Engine for MQTT/HTTP Protocols
//!
//! Extracts data points from JSON payloads using JSONPath expressions.
//! Mappings are loaded from the canonical inline `protocol_mappings` column
//! on the four physical point tables, so new devices can be integrated
//! without a second mapping authority.
//!
//! Features:
//! - JSONPath-based value extraction (RFC 9535 via `serde_json_path`)
//! - Timestamp format conversion (Unix seconds/millis, ISO 8601)
//! - Data type conversion and linear scaling (scale * value + offset)

use chrono::{DateTime, TimeZone, Utc};
use common::PointType;
use serde::{Deserialize, Serialize};
use serde_json_path::JsonPath;
use tracing::{debug, trace};

use super::data::{DataBatch, DataPoint, Value};
use super::error::{GatewayError, Result};

// ============================================================================
// Configuration enums
// ============================================================================

/// Timestamp format in JSON payload
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimestampFormat {
    UnixSeconds,
    #[default]
    UnixMillis,
    Iso8601,
    Now,
}

/// Data type for JSON value extraction
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JsonDataType {
    #[default]
    Float,
    Int,
    Bool,
    String,
}

// ============================================================================
// Compiled mapping
// ============================================================================

/// A pre-compiled point mapping with JSONPath expression and scaling parameters.
///
/// The JSONPath is compiled once at startup and reused for every incoming message,
/// avoiding the overhead of re-parsing the expression on each invocation.
#[derive(Debug)]
pub struct CompiledMapping {
    pub point_id: u32,
    pub point_type: PointType,
    pub json_path: JsonPath,
    pub data_type: JsonDataType,
    pub scale: f64,
    pub offset: f64,
}

/// JSON mapping configuration for a channel (from channel parameters)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonMappingConfig {
    #[serde(default)]
    pub timestamp_path: Option<String>,
    #[serde(default)]
    pub timestamp_format: TimestampFormat,
}

// ============================================================================
// JsonMapper
// ============================================================================

/// JSON payload mapper for a channel.
///
/// Holds pre-compiled JSONPath mappings and optional channel-level paths
/// for timestamp and device ID extraction.
#[derive(Debug)]
pub struct JsonMapper {
    pub channel_id: u32,
    pub mappings: Vec<CompiledMapping>,
    timestamp_path: Option<JsonPath>,
    timestamp_format: TimestampFormat,
}

impl JsonMapper {
    /// Create a new empty mapper.
    pub fn new(channel_id: u32) -> Self {
        Self {
            channel_id,
            mappings: Vec::new(),
            timestamp_path: None,
            timestamp_format: TimestampFormat::default(),
        }
    }

    /// Compile mappings from the already loaded physical topology generation.
    ///
    /// The composition root supplies mappings from one `RuntimeChannelConfig`;
    /// protocol adapters never perform a second SQLite read that could observe a
    /// different generation.
    pub fn from_inline_mappings<'a>(
        channel_id: u32,
        rows: impl IntoIterator<Item = (u32, PointType, Option<&'a str>)>,
    ) -> Result<Self> {
        let mut mappings = Vec::new();
        for (point_id, point_type, raw_mapping) in rows {
            if let Some(mapping) = Self::compile_mapping(point_id, point_type, raw_mapping)? {
                mappings.push(mapping);
            }
        }

        debug!(
            channel_id,
            count = mappings.len(),
            "Compiled JSON point mappings from runtime topology generation"
        );

        let mut mapper = Self::new(channel_id);
        mapper.mappings = mappings;
        Ok(mapper)
    }

    /// Apply channel-level configuration (timestamp/device-id paths).
    pub fn with_config(mut self, config: &JsonMappingConfig) -> Result<Self> {
        if let Some(ref path_str) = config.timestamp_path {
            self.timestamp_path = Some(compile_path(path_str)?);
        }
        self.timestamp_format = config.timestamp_format;

        Ok(self)
    }

    /// Parse a raw JSON payload (bytes) using deterministic JSONPath mappings.
    pub fn parse(&self, payload: &[u8]) -> Result<DataBatch> {
        if self.mappings.is_empty() {
            return Ok(DataBatch::new());
        }

        let json: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|e| GatewayError::InvalidData(format!("Invalid JSON: {e}")))?;

        self.parse_value(&json)
    }

    /// Parse from an already-deserialized JSON value.
    pub fn parse_value(&self, json: &serde_json::Value) -> Result<DataBatch> {
        if self.mappings.is_empty() {
            return Ok(DataBatch::new());
        }

        let timestamp = self.extract_timestamp(json);
        let mut batch = DataBatch::with_capacity(self.mappings.len());

        for mapping in &self.mappings {
            match self.extract_point(json, mapping, timestamp) {
                Ok(point) => batch.add(point),
                Err(e) => {
                    trace!(
                        channel_id = self.channel_id,
                        point_id = mapping.point_id,
                        error = %e,
                        "Failed to extract point from JSON"
                    );
                },
            }
        }

        Ok(batch)
    }

    // === Private helpers ===

    /// Compile one inline point mapping into a runtime mapping.
    fn compile_mapping(
        point_id: u32,
        point_type: PointType,
        raw_mapping: Option<&str>,
    ) -> Result<Option<CompiledMapping>> {
        let Some(raw_mapping) = raw_mapping else {
            return Ok(None);
        };
        if raw_mapping.trim().is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(raw_mapping)
            .map_err(|error| invalid_stored_mapping(point_id, format!("invalid JSON: {error}")))?;
        if value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok(None);
        }
        let values = value
            .as_object()
            .ok_or_else(|| invalid_stored_mapping(point_id, "mapping must be an object or null"))?;
        for field in values.keys() {
            if !matches!(
                field.as_str(),
                "json_path" | "data_type" | "scale" | "offset" | "description"
            ) {
                return Err(invalid_stored_mapping(
                    point_id,
                    format!("unsupported field {field}"),
                ));
            }
        }
        let path = values
            .get("json_path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| invalid_stored_mapping(point_id, "json_path must be nonblank"))?;
        let json_path = compile_path(path)
            .map_err(|error| invalid_stored_mapping(point_id, error.to_string()))?;
        let data_type = match values.get("data_type") {
            None => JsonDataType::Float,
            Some(serde_json::Value::String(value)) if value == "float" => JsonDataType::Float,
            Some(serde_json::Value::String(value))
                if matches!(value.as_str(), "int" | "integer") =>
            {
                JsonDataType::Int
            },
            Some(serde_json::Value::String(value))
                if matches!(value.as_str(), "bool" | "boolean") =>
            {
                JsonDataType::Bool
            },
            Some(serde_json::Value::String(value))
                if matches!(value.as_str(), "string" | "str") =>
            {
                JsonDataType::String
            },
            _ => {
                return Err(invalid_stored_mapping(
                    point_id,
                    "data_type must be float, int, bool, or string",
                ));
            },
        };
        let scale = finite_mapping_number(values, point_id, "scale", 1.0)?;
        let offset = finite_mapping_number(values, point_id, "offset", 0.0)?;
        if values
            .get("description")
            .is_some_and(|description| !description.is_string())
        {
            return Err(invalid_stored_mapping(
                point_id,
                "description must be a string",
            ));
        }
        Ok(Some(CompiledMapping {
            point_id,
            point_type,
            json_path,
            data_type,
            scale,
            offset,
        }))
    }

    /// Extract a single data point from JSON using a compiled mapping.
    fn extract_point(
        &self,
        json: &serde_json::Value,
        mapping: &CompiledMapping,
        timestamp: DateTime<Utc>,
    ) -> Result<DataPoint> {
        let nodes = mapping.json_path.query(json);
        let raw = nodes.first().ok_or_else(|| {
            GatewayError::InvalidData(format!(
                "JSONPath matched no value for point_id={}",
                mapping.point_id
            ))
        })?;

        let value = convert_value(raw, mapping.data_type, mapping.scale, mapping.offset)?;

        let mut point = DataPoint::new(mapping.point_id, mapping.point_type, value);
        point.timestamp = timestamp;
        Ok(point)
    }

    /// Extract timestamp from the JSON payload using the configured path/format.
    fn extract_timestamp(&self, json: &serde_json::Value) -> DateTime<Utc> {
        if self.timestamp_format == TimestampFormat::Now {
            return Utc::now();
        }

        let Some(ref path) = self.timestamp_path else {
            return Utc::now();
        };

        let nodes = path.query(json);
        let Some(raw) = nodes.first() else {
            return Utc::now();
        };

        parse_timestamp(raw, self.timestamp_format).unwrap_or_else(Utc::now)
    }
}

// ============================================================================
// Free functions
// ============================================================================

/// Compile a JSONPath string, wrapping parse errors.
fn compile_path(path_str: &str) -> Result<JsonPath> {
    JsonPath::parse(path_str)
        .map_err(|e| GatewayError::Config(format!("Invalid JSONPath '{path_str}': {e}")))
}

fn finite_mapping_number(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
    field: &str,
    default: f64,
) -> Result<f64> {
    let Some(value) = values.get(field) else {
        return Ok(default);
    };
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid_stored_mapping(point_id, format!("{field} must be a finite number")))
}

fn invalid_stored_mapping(point_id: u32, reason: impl std::fmt::Display) -> GatewayError {
    GatewayError::Config(format!(
        "Invalid JSON mapping for point {point_id}: {reason}"
    ))
}

/// Convert a raw JSON value to a `Value` with optional linear scaling.
fn convert_value(
    raw: &serde_json::Value,
    data_type: JsonDataType,
    scale: f64,
    offset: f64,
) -> Result<Value> {
    match data_type {
        JsonDataType::Float => {
            let v = json_to_f64(raw).ok_or_else(|| {
                GatewayError::DataConversion(format!("Cannot convert {raw} to float"))
            })?;
            Ok(Value::Float(v * scale + offset))
        },
        JsonDataType::Int => {
            let v = json_to_f64(raw).ok_or_else(|| {
                GatewayError::DataConversion(format!("Cannot convert {raw} to int"))
            })?;
            #[allow(clippy::cast_possible_truncation)]
            Ok(Value::Integer((v * scale + offset) as i64))
        },
        JsonDataType::Bool => {
            let v = match raw {
                serde_json::Value::Bool(b) => *b,
                serde_json::Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0),
                serde_json::Value::String(s) => !s.is_empty() && s != "0" && s != "false",
                _ => false,
            };
            Ok(Value::Bool(v))
        },
        JsonDataType::String => Ok(Value::String(json_value_to_string(raw))),
    }
}

/// Try to extract an f64 from a JSON value.
fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Convert any JSON value to a string representation.
fn json_value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse a timestamp from a raw JSON value using the given format.
fn parse_timestamp(raw: &serde_json::Value, format: TimestampFormat) -> Option<DateTime<Utc>> {
    match format {
        TimestampFormat::UnixSeconds => {
            let secs = json_to_f64(raw)? as i64;
            Utc.timestamp_opt(secs, 0).single()
        },
        TimestampFormat::UnixMillis => {
            let millis = json_to_f64(raw)? as i64;
            Utc.timestamp_millis_opt(millis).single()
        },
        TimestampFormat::Iso8601 => {
            let s = match raw {
                serde_json::Value::String(s) => s.as_str(),
                _ => return None,
            };
            DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.to_utc())
        },
        TimestampFormat::Now => Some(Utc::now()),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_mapping(point_id: u32, path: &str, data_type: JsonDataType) -> CompiledMapping {
        CompiledMapping {
            point_id,
            point_type: PointType::Telemetry,
            json_path: JsonPath::parse(path).unwrap(),
            data_type,
            scale: 1.0,
            offset: 0.0,
        }
    }

    fn make_mapper(mappings: Vec<CompiledMapping>) -> JsonMapper {
        JsonMapper {
            channel_id: 1,
            mappings,
            timestamp_path: None,
            timestamp_format: TimestampFormat::Now,
        }
    }

    #[test]
    fn inline_generation_rejects_every_mapping_when_one_is_invalid() {
        let error = JsonMapper::from_inline_mappings(
            7,
            [
                (1, PointType::Telemetry, Some(r#"{"json_path":"$.valid"}"#)),
                (2, PointType::Signal, Some(r#"{"json_path":"invalid[[["}"#)),
            ],
        )
        .expect_err("a partial mapping generation must fail closed");

        assert!(error.to_string().contains("point 2"));
        assert!(error.to_string().contains("Invalid JSONPath"));
    }

    #[test]
    fn inline_generation_compiles_all_four_point_planes() {
        let mapper = JsonMapper::from_inline_mappings(
            7,
            [
                (1, PointType::Telemetry, Some(r#"{"json_path":"$.value1"}"#)),
                (2, PointType::Signal, Some(r#"{"json_path":"$.value2"}"#)),
                (3, PointType::Control, Some(r#"{"json_path":"$.value3"}"#)),
                (
                    4,
                    PointType::Adjustment,
                    Some(r#"{"json_path":"$.value4"}"#),
                ),
            ],
        )
        .expect("complete inline generation");

        assert_eq!(mapper.mappings.len(), 4);
        assert_eq!(mapper.mappings[0].point_type, PointType::Telemetry);
        assert_eq!(mapper.mappings[1].point_type, PointType::Signal);
        assert_eq!(mapper.mappings[2].point_type, PointType::Control);
        assert_eq!(mapper.mappings[3].point_type, PointType::Adjustment);
    }

    #[test]
    fn test_parse_simple_float() {
        let mapper = make_mapper(vec![make_mapping(101, "$.data.power", JsonDataType::Float)]);
        let payload = br#"{"data": {"power": 42.5}}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(batch.len(), 1);
        let point = batch.iter().next().unwrap();
        assert_eq!(point.id, 101);
        assert_eq!(point.value.as_f64(), Some(42.5));
    }

    #[test]
    fn test_parse_with_scale_and_offset() {
        let mut mapping = make_mapping(102, "$.sensor.temp", JsonDataType::Float);
        mapping.scale = 0.1;
        mapping.offset = -10.0;
        let mapper = make_mapper(vec![mapping]);

        let payload = br#"{"sensor": {"temp": 250}}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(batch.len(), 1);
        let point = batch.iter().next().unwrap();
        // 250 * 0.1 + (-10) = 15.0
        assert!((point.value.as_f64().unwrap() - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_integer_type() {
        let mapper = make_mapper(vec![make_mapping(103, "$.count", JsonDataType::Int)]);
        let payload = br#"{"count": 42}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(batch.iter().next().unwrap().value.as_i64(), Some(42));
    }

    #[test]
    fn test_parse_bool_type() {
        let mapper = make_mapper(vec![make_mapping(104, "$.status", JsonDataType::Bool)]);
        let payload = br#"{"status": true}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(batch.iter().next().unwrap().value.as_bool(), Some(true));
    }

    #[test]
    fn test_parse_string_type() {
        let mapper = make_mapper(vec![make_mapping(105, "$.name", JsonDataType::String)]);
        let payload = br#"{"name": "inverter-1"}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(
            batch.iter().next().unwrap().value.as_string(),
            Some("inverter-1")
        );
    }

    #[test]
    fn test_parse_missing_path_skipped() {
        let mapper = make_mapper(vec![
            make_mapping(101, "$.data.power", JsonDataType::Float),
            make_mapping(102, "$.data.missing", JsonDataType::Float),
        ]);
        let payload = br#"{"data": {"power": 100.0}}"#;
        let batch = mapper.parse(payload).unwrap();
        // Only point 101 should be present; point 102's missing path is skipped
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.iter().next().unwrap().id, 101);
    }

    #[test]
    fn test_parse_multiple_points() {
        let mapper = make_mapper(vec![
            make_mapping(1, "$.voltage", JsonDataType::Float),
            make_mapping(2, "$.current", JsonDataType::Float),
            make_mapping(3, "$.online", JsonDataType::Bool),
        ]);
        let payload = br#"{"voltage": 220.5, "current": 10.2, "online": true}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(batch.len(), 3);
    }

    #[test]
    fn test_empty_mapper_returns_empty_batch() {
        let mapper = make_mapper(vec![]);
        let payload = br#"{"anything": 123}"#;
        let batch = mapper.parse(payload).unwrap();
        assert!(batch.is_empty());
    }

    #[test]
    fn test_invalid_json_returns_error() {
        let mapper = make_mapper(vec![make_mapping(1, "$.v", JsonDataType::Float)]);
        let result = mapper.parse(b"not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_timestamp_unix_seconds() {
        let ts = parse_timestamp(&json!(1_700_000_000), TimestampFormat::UnixSeconds);
        assert!(ts.is_some());
        assert_eq!(ts.unwrap().timestamp(), 1_700_000_000);
    }

    #[test]
    fn test_timestamp_unix_millis() {
        let ts = parse_timestamp(&json!(1_700_000_000_000_i64), TimestampFormat::UnixMillis);
        assert!(ts.is_some());
        assert_eq!(ts.unwrap().timestamp(), 1_700_000_000);
    }

    #[test]
    fn test_timestamp_iso8601() {
        let ts = parse_timestamp(&json!("2023-11-14T22:13:20Z"), TimestampFormat::Iso8601);
        assert!(ts.is_some());
        assert_eq!(ts.unwrap().timestamp(), 1_700_000_000);
    }

    #[test]
    fn test_string_to_float_conversion() {
        let mapper = make_mapper(vec![make_mapping(1, "$.val", JsonDataType::Float)]);
        let payload = br#"{"val": "3.25"}"#;
        let batch = mapper.parse(payload).unwrap();
        let v = batch.iter().next().unwrap().value.as_f64().unwrap();
        assert!((v - 3.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_nested_json_path() {
        let mapper = make_mapper(vec![make_mapping(
            1,
            "$.data.sensors[0].value",
            JsonDataType::Float,
        )]);
        let payload = br#"{"data": {"sensors": [{"value": 99.9}, {"value": 88.8}]}}"#;
        let batch = mapper.parse(payload).unwrap();
        let v = batch.iter().next().unwrap().value.as_f64().unwrap();
        assert!((v - 99.9).abs() < f64::EPSILON);
    }

    #[test]
    fn retired_python_transform_configuration_is_rejected() {
        let error = serde_json::from_value::<JsonMappingConfig>(json!({
            "transform_script": "/tmp/transform.py"
        }))
        .expect_err("the Rust-only IO runtime must reject Python transform configuration");

        assert!(
            error
                .to_string()
                .contains("unknown field `transform_script`")
        );
    }

    #[test]
    fn test_with_config_applies_paths() {
        let mapper = JsonMapper::new(1)
            .with_config(&JsonMappingConfig {
                timestamp_path: Some("$.ts".to_string()),
                timestamp_format: TimestampFormat::UnixSeconds,
            })
            .unwrap();

        assert!(mapper.timestamp_path.is_some());
        assert_eq!(mapper.timestamp_format, TimestampFormat::UnixSeconds);
    }

    #[test]
    fn test_with_config_invalid_path_returns_error() {
        let result = JsonMapper::new(1).with_config(&JsonMappingConfig {
            timestamp_path: Some("invalid[[[".to_string()),
            timestamp_format: TimestampFormat::Now,
        });
        assert!(result.is_err());
    }
}
