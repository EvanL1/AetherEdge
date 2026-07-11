//! JSON Payload Mapping Engine for MQTT/HTTP Protocols
//!
//! Extracts data points from JSON payloads using JSONPath expressions.
//! Mappings are loaded from the `json_point_mappings` database table,
//! so new devices can be integrated without code changes.
//!
//! Features:
//! - JSONPath-based value extraction (RFC 9535 via `serde_json_path`)
//! - Timestamp format conversion (Unix seconds/millis, ISO 8601)
//! - Data type conversion and linear scaling (scale * value + offset)
//! - Optional Python script fallback for complex transformations

use aether_model::PointType;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json_path::JsonPath;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::{debug, trace, warn};

use super::data::{DataBatch, DataPoint, Value};
use super::error::{GatewayError, Result};
use super::script_runner::ScriptRunner;

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
pub struct JsonMappingConfig {
    #[serde(default)]
    pub device_id_path: Option<String>,
    #[serde(default)]
    pub timestamp_path: Option<String>,
    #[serde(default)]
    pub timestamp_format: TimestampFormat,
    #[serde(default)]
    pub transform_script: Option<String>,
}

// ============================================================================
// Database row for SQLx (runtime query)
// ============================================================================

/// Raw row from `json_point_mappings` table.
#[derive(Debug, sqlx::FromRow)]
struct MappingRow {
    point_id: i64,
    point_type: String,
    json_path: String,
    data_type: Option<String>,
    scale: Option<f64>,
    offset: Option<f64>,
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
    device_id_path: Option<JsonPath>,
    script_runner: Option<ScriptRunner>,
}

impl JsonMapper {
    /// Create a new empty mapper.
    pub fn new(channel_id: u32) -> Self {
        Self {
            channel_id,
            mappings: Vec::new(),
            timestamp_path: None,
            timestamp_format: TimestampFormat::default(),
            device_id_path: None,
            script_runner: None,
        }
    }

    /// Load mapper from the `json_point_mappings` database table.
    ///
    /// Queries all mapping rows for the given channel and pre-compiles
    /// their JSONPath expressions. Invalid rows are logged and skipped
    /// to avoid one bad mapping breaking the entire channel.
    pub async fn from_database(pool: &SqlitePool, channel_id: u32) -> Result<Self> {
        let rows = sqlx::query_as::<_, MappingRow>(
            "SELECT point_id, point_type, json_path, data_type, scale, offset \
             FROM json_point_mappings WHERE channel_id = ?",
        )
        .bind(channel_id)
        .fetch_all(pool)
        .await
        .map_err(|e| GatewayError::Config(format!("Failed to load JSON mappings: {e}")))?;

        let mut mappings = Vec::with_capacity(rows.len());
        for row in &rows {
            match Self::compile_row(row) {
                Ok(m) => mappings.push(m),
                Err(e) => {
                    warn!(
                        channel_id,
                        point_id = row.point_id,
                        error = %e,
                        "Skipping invalid JSON mapping"
                    );
                },
            }
        }

        debug!(
            channel_id,
            count = mappings.len(),
            "Loaded JSON point mappings from database"
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

        if let Some(ref path_str) = config.device_id_path {
            self.device_id_path = Some(compile_path(path_str)?);
        }

        if let Some(ref script) = config.transform_script {
            self.script_runner = Some(ScriptRunner::new(self.channel_id, script.clone()));
        }

        Ok(self)
    }

    /// Parse a raw JSON payload (bytes) and extract data points.
    ///
    /// If a Python transform script is configured, delegates to the script runner.
    /// Otherwise, uses JSONPath mappings to extract values.
    pub fn parse(&self, payload: &[u8]) -> Result<DataBatch> {
        // Python script path: parse JSON, send to subprocess
        if let Some(ref runner) = self.script_runner {
            let json: serde_json::Value = serde_json::from_slice(payload)
                .map_err(|e| GatewayError::InvalidData(format!("Invalid JSON: {e}")))?;
            return runner.transform(&json);
        }

        // JSONPath mapping path
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

    /// Extract device ID from the JSON payload using the configured JSONPath.
    pub fn extract_device_id(&self, json: &serde_json::Value) -> Option<String> {
        let path = self.device_id_path.as_ref()?;
        let nodes = path.query(json);
        let first = nodes.first()?;
        Some(json_value_to_string(first))
    }

    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    pub fn len(&self) -> usize {
        self.mappings.len()
    }

    // === Private helpers ===

    /// Compile a single database row into a `CompiledMapping`.
    fn compile_row(row: &MappingRow) -> Result<CompiledMapping> {
        let point_type = PointType::from_str(&row.point_type).ok_or_else(|| {
            GatewayError::Config(format!("Invalid point_type: {}", row.point_type))
        })?;

        let json_path = compile_path(&row.json_path)?;

        let data_type = match row.data_type.as_deref() {
            Some("int") | Some("integer") => JsonDataType::Int,
            Some("bool") | Some("boolean") => JsonDataType::Bool,
            Some("string") | Some("str") => JsonDataType::String,
            _ => JsonDataType::Float,
        };

        #[allow(clippy::cast_possible_truncation)]
        Ok(CompiledMapping {
            point_id: row.point_id as u32,
            point_type,
            json_path,
            data_type,
            scale: row.scale.unwrap_or(1.0),
            offset: row.offset.unwrap_or(0.0),
        })
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

/// Thread-safe shared mapper reference
pub type SharedJsonMapper = Arc<JsonMapper>;

// ============================================================================
// Free functions
// ============================================================================

/// Compile a JSONPath string, wrapping parse errors.
fn compile_path(path_str: &str) -> Result<JsonPath> {
    JsonPath::parse(path_str)
        .map_err(|e| GatewayError::Config(format!("Invalid JSONPath '{path_str}': {e}")))
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
            device_id_path: None,
            script_runner: None,
        }
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
    fn test_extract_device_id() {
        let mapper = JsonMapper {
            channel_id: 1,
            mappings: vec![],
            timestamp_path: None,
            timestamp_format: TimestampFormat::Now,
            device_id_path: Some(JsonPath::parse("$.device.serial").unwrap()),
            script_runner: None,
        };
        let json = json!({"device": {"serial": "SN-12345"}});
        assert_eq!(
            mapper.extract_device_id(&json),
            Some("SN-12345".to_string())
        );
    }

    #[test]
    fn test_extract_device_id_none_when_no_path() {
        let mapper = make_mapper(vec![]);
        let json = json!({"device": {"serial": "SN-12345"}});
        assert_eq!(mapper.extract_device_id(&json), None);
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
    fn test_with_config_applies_paths() {
        let mapper = JsonMapper::new(1)
            .with_config(&JsonMappingConfig {
                timestamp_path: Some("$.ts".to_string()),
                timestamp_format: TimestampFormat::UnixSeconds,
                device_id_path: Some("$.dev".to_string()),
                transform_script: None,
            })
            .unwrap();

        assert!(mapper.timestamp_path.is_some());
        assert_eq!(mapper.timestamp_format, TimestampFormat::UnixSeconds);
        assert!(mapper.device_id_path.is_some());
    }

    #[test]
    fn test_with_config_invalid_path_returns_error() {
        let result = JsonMapper::new(1).with_config(&JsonMappingConfig {
            timestamp_path: Some("invalid[[[".to_string()),
            timestamp_format: TimestampFormat::Now,
            device_id_path: None,
            transform_script: None,
        });
        assert!(result.is_err());
    }
}
