//! JSON payload mapping owned by the MQTT/HTTP adapters.
//!
//! Extracts data points from JSON payloads using JSONPath expressions.
//! Mappings come from the canonical inline `protocol_mappings` fields in one
//! immutable runtime snapshot, so protocol runtimes never own SQLite.
//!
//! Features:
//! - JSONPath-based value extraction (RFC 9535 via `serde_json_path`)
//! - Timestamp format conversion (Unix seconds/millis, ISO 8601)
//! - Data type conversion and linear scaling (scale * value + offset)

use aether_core::PointType;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json_path::JsonPath;
use tracing::{debug, trace};

use crate::core::channels::RuntimeChannelConfig;

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::error::{GatewayError, Result};

// ============================================================================
// Configuration enums
// ============================================================================

/// Timestamp format in JSON payload
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TimestampFormat {
    UnixSeconds,
    #[default]
    UnixMillis,
    Iso8601,
    Now,
}

/// Data type for JSON value extraction
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum JsonDataType {
    #[default]
    Float,
    Int,
    Bool,
}

// ============================================================================
// Compiled mapping
// ============================================================================

/// A pre-compiled point mapping with JSONPath expression and scaling parameters.
///
/// The JSONPath is compiled once at startup and reused for every incoming message,
/// avoiding the overhead of re-parsing the expression on each invocation.
#[derive(Debug)]
struct CompiledMapping {
    point_id: u32,
    point_type: PointType,
    json_path: JsonPath,
    data_type: JsonDataType,
    scale: f64,
    offset: f64,
    reverse: bool,
}

/// JSON mapping configuration for a channel (from channel parameters)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonMappingConfig {
    #[serde(default)]
    timestamp_path: Option<String>,
    #[serde(default)]
    timestamp_format: TimestampFormat,
}

// ============================================================================
// JsonMapper
// ============================================================================

/// JSON payload mapper for a channel.
///
/// Holds pre-compiled JSONPath mappings and optional channel-level paths
/// for timestamp and device ID extraction.
#[derive(Debug)]
pub(crate) struct JsonMapper {
    channel_id: u32,
    mappings: Vec<CompiledMapping>,
    timestamp_path: Option<JsonPath>,
    timestamp_format: TimestampFormat,
}

impl JsonMapper {
    /// Create a new empty mapper.
    fn new(channel_id: u32) -> Self {
        Self {
            channel_id,
            mappings: Vec::new(),
            timestamp_path: None,
            timestamp_format: TimestampFormat::default(),
        }
    }

    /// Compile one complete mapping generation from an immutable runtime snapshot.
    ///
    /// The SQLite loader already loaded all four point planes in one
    /// transaction. Compiling from that snapshot keeps one configuration
    /// generation and makes malformed non-empty mappings fail before the
    /// channel runtime is created.
    pub(crate) fn from_runtime_config(runtime: &RuntimeChannelConfig) -> Result<Self> {
        let mut mappings =
            Vec::with_capacity(runtime.telemetry_points.len() + runtime.signal_points.len());
        for point in &runtime.telemetry_points {
            if let Some(mapping) = Self::compile_mapping(
                point.base.point_id,
                point.base.protocol_mappings.as_deref(),
                PointType::Telemetry,
                point.scale,
                point.offset,
                false,
            )? {
                mappings.push(mapping);
            }
        }
        for point in &runtime.signal_points {
            if let Some(mapping) = Self::compile_mapping(
                point.base.point_id,
                point.base.protocol_mappings.as_deref(),
                PointType::Signal,
                1.0,
                0.0,
                point.reverse,
            )? {
                mappings.push(mapping);
            }
        }
        Self::reject_write_plane_mappings(
            PointType::Control,
            runtime.control_points.iter().map(|point| &point.base),
        )?;
        Self::reject_write_plane_mappings(
            PointType::Adjustment,
            runtime.adjustment_points.iter().map(|point| &point.base),
        )?;

        debug!(
            channel_id = runtime.id(),
            count = mappings.len(),
            "Compiled JSON point mappings from runtime snapshot"
        );

        let mut mapper = Self::new(runtime.id());
        mapper.mappings = mappings;
        Ok(mapper)
    }

    /// Apply channel-level source timestamp configuration.
    pub(crate) fn with_config(mut self, config: &JsonMappingConfig) -> Result<Self> {
        if let Some(ref path_str) = config.timestamp_path {
            self.timestamp_path = Some(compile_path(path_str)?);
        }
        self.timestamp_format = config.timestamp_format;

        Ok(self)
    }

    /// Parse a raw JSON payload (bytes) using deterministic JSONPath mappings.
    pub(crate) fn parse(&self, payload: &[u8]) -> Result<DataBatch> {
        if self.mappings.is_empty() {
            return Ok(DataBatch::new());
        }

        let json: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|e| GatewayError::InvalidData(format!("Invalid JSON: {e}")))?;

        self.parse_value(&json)
    }

    /// Parse from an already-deserialized JSON value.
    fn parse_value(&self, json: &serde_json::Value) -> Result<DataBatch> {
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

    pub(crate) fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.mappings.len()
    }

    // === Private helpers ===

    fn reject_write_plane_mappings<'a>(
        point_type: PointType,
        points: impl IntoIterator<Item = &'a aether_config::io::Point>,
    ) -> Result<()> {
        for point in points {
            let Some(raw_mapping) = point.protocol_mappings.as_deref() else {
                continue;
            };
            if raw_mapping.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(raw_mapping).map_err(|error| {
                invalid_stored_mapping(point.point_id, format!("invalid JSON: {error}"))
            })?;
            validate_point_mapping(point_type, point.point_id, &value)?;
        }
        Ok(())
    }

    /// Compile one inline point mapping.
    fn compile_mapping(
        point_id: u32,
        raw_mapping: Option<&str>,
        point_type: PointType,
        scale: f64,
        offset: f64,
        reverse: bool,
    ) -> Result<Option<CompiledMapping>> {
        let Some(raw_mapping) = raw_mapping else {
            return Ok(None);
        };
        if raw_mapping.trim().is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(raw_mapping)
            .map_err(|error| invalid_stored_mapping(point_id, format!("invalid JSON: {error}")))?;
        let Some(validated) = compile_point_mapping(point_type, point_id, &value)? else {
            return Ok(None);
        };
        if !scale.is_finite() || !offset.is_finite() {
            return Err(invalid_stored_mapping(
                point_id,
                "point scale and offset must be finite",
            ));
        }
        Ok(Some(CompiledMapping {
            point_id,
            point_type,
            json_path: validated.json_path,
            data_type: validated.data_type,
            scale,
            offset,
            reverse,
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

        let value = convert_value(
            raw,
            mapping.data_type,
            mapping.scale,
            mapping.offset,
            mapping.reverse,
        )?;

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

struct ValidatedPointMapping {
    json_path: JsonPath,
    data_type: JsonDataType,
}

/// Validate one point-owned JSON mapping through the adapter's canonical schema.
pub(crate) fn validate_point_mapping(
    point_type: PointType,
    point_id: u32,
    value: &serde_json::Value,
) -> Result<()> {
    compile_point_mapping(point_type, point_id, value).map(|_| ())
}

fn compile_point_mapping(
    point_type: PointType,
    point_id: u32,
    value: &serde_json::Value,
) -> Result<Option<ValidatedPointMapping>> {
    if value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(None);
    }
    let default_data_type = match point_type {
        PointType::Telemetry => JsonDataType::Float,
        PointType::Signal => JsonDataType::Bool,
        PointType::Control | PointType::Adjustment => {
            return Err(invalid_stored_mapping(
                point_id,
                format!(
                    "read-only JSON channels do not accept {} mappings",
                    point_type.as_str()
                ),
            ));
        },
    };
    let values = value
        .as_object()
        .ok_or_else(|| invalid_stored_mapping(point_id, "mapping must be an object or null"))?;
    for field in values.keys() {
        if !matches!(field.as_str(), "json_path" | "data_type") {
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
    let json_path =
        compile_path(path).map_err(|error| invalid_stored_mapping(point_id, error.to_string()))?;
    let data_type = match values.get("data_type") {
        None => default_data_type,
        Some(serde_json::Value::String(value)) if value == "float" => JsonDataType::Float,
        Some(serde_json::Value::String(value)) if matches!(value.as_str(), "int" | "integer") => {
            JsonDataType::Int
        },
        Some(serde_json::Value::String(value)) if matches!(value.as_str(), "bool" | "boolean") => {
            JsonDataType::Bool
        },
        _ => {
            return Err(invalid_stored_mapping(
                point_id,
                "data_type must be float, int, or bool",
            ));
        },
    };
    match (point_type, data_type) {
        (PointType::Telemetry, JsonDataType::Float | JsonDataType::Int)
        | (PointType::Signal, JsonDataType::Bool) => {},
        (PointType::Telemetry, JsonDataType::Bool) => {
            return Err(invalid_stored_mapping(
                point_id,
                "telemetry JSON mappings require float or int data_type",
            ));
        },
        (PointType::Signal, JsonDataType::Float | JsonDataType::Int) => {
            return Err(invalid_stored_mapping(
                point_id,
                "signal JSON mappings require bool data_type",
            ));
        },
        (PointType::Control | PointType::Adjustment, _) => {
            return Err(invalid_stored_mapping(
                point_id,
                "read-only JSON channels accept only telemetry and signal mappings",
            ));
        },
    }
    Ok(Some(ValidatedPointMapping {
        json_path,
        data_type,
    }))
}

/// Compile a JSONPath string, wrapping parse errors.
fn compile_path(path_str: &str) -> Result<JsonPath> {
    JsonPath::parse(path_str)
        .map_err(|e| GatewayError::Config(format!("Invalid JSONPath '{path_str}': {e}")))
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
    reverse: bool,
) -> Result<Value> {
    match data_type {
        JsonDataType::Float => {
            let v = json_to_f64(raw).ok_or_else(|| {
                GatewayError::DataConversion(format!("Cannot convert {raw} to a finite float"))
            })?;
            let value = v * scale + offset;
            if !value.is_finite() {
                return Err(GatewayError::DataConversion(format!(
                    "Scaled value for {raw} is not finite"
                )));
            }
            Ok(Value::Float(if reverse {
                if value == 0.0 { 1.0 } else { 0.0 }
            } else {
                value
            }))
        },
        JsonDataType::Int => {
            let v = json_to_f64(raw).ok_or_else(|| {
                GatewayError::DataConversion(format!("Cannot convert {raw} to a finite integer"))
            })?;
            let scaled = v * scale + offset;
            let value = checked_trunc_i64(scaled).ok_or_else(|| {
                GatewayError::DataConversion(format!(
                    "Scaled value for {raw} is outside the i64 range"
                ))
            })?;
            Ok(Value::Integer(if reverse {
                if value == 0 { 1 } else { 0 }
            } else {
                value
            }))
        },
        JsonDataType::Bool => {
            let v = json_to_bool(raw)?;
            Ok(Value::Bool(if reverse { !v } else { v }))
        },
    }
}

/// Try to extract an f64 from a JSON value.
fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
    let value = match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }?;
    value.is_finite().then_some(value)
}

fn json_to_bool(value: &serde_json::Value) -> Result<bool> {
    match value {
        serde_json::Value::Bool(value) => Ok(*value),
        serde_json::Value::Number(value) => match value.as_f64() {
            Some(0.0) => Ok(false),
            Some(1.0) => Ok(true),
            _ => Err(GatewayError::DataConversion(format!(
                "Cannot convert {value} to bool; expected 0 or 1"
            ))),
        },
        serde_json::Value::String(value) => {
            let normalized = value.trim();
            if normalized.eq_ignore_ascii_case("true") || normalized == "1" {
                Ok(true)
            } else if normalized.eq_ignore_ascii_case("false") || normalized == "0" {
                Ok(false)
            } else {
                Err(GatewayError::DataConversion(format!(
                    "Cannot convert {value:?} to bool; expected true, false, 1, or 0"
                )))
            }
        },
        _ => Err(GatewayError::DataConversion(format!(
            "Cannot convert {value} to bool; expected true, false, 1, or 0"
        ))),
    }
}

fn checked_trunc_i64(value: f64) -> Option<i64> {
    const I64_EXCLUSIVE_MAX: f64 = 9_223_372_036_854_775_808.0;
    if value.is_finite() && value >= i64::MIN as f64 && value < I64_EXCLUSIVE_MAX {
        #[allow(clippy::cast_possible_truncation)]
        Some(value as i64)
    } else {
        None
    }
}

/// Parse a timestamp from a raw JSON value using the given format.
fn parse_timestamp(raw: &serde_json::Value, format: TimestampFormat) -> Option<DateTime<Utc>> {
    match format {
        TimestampFormat::UnixSeconds => {
            let secs = checked_trunc_i64(json_to_f64(raw)?)?;
            Utc.timestamp_opt(secs, 0).single()
        },
        TimestampFormat::UnixMillis => {
            let millis = checked_trunc_i64(json_to_f64(raw)?)?;
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
    use crate::core::config::{
        AdjustmentPoint, ChannelConfig, ChannelCore, ChannelLoggingConfig, ControlPoint, Point,
        SignalPoint, TelemetryPoint,
    };
    use serde_json::json;
    use std::collections::HashMap;

    fn make_mapping(point_id: u32, path: &str, data_type: JsonDataType) -> CompiledMapping {
        CompiledMapping {
            point_id,
            point_type: PointType::Telemetry,
            json_path: JsonPath::parse(path).unwrap(),
            data_type,
            scale: 1.0,
            offset: 0.0,
            reverse: false,
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

    fn runtime_snapshot() -> RuntimeChannelConfig {
        RuntimeChannelConfig::from_base(ChannelConfig {
            core: ChannelCore {
                id: 7,
                name: "json-source".to_string(),
                description: None,
                protocol: "mqtt".to_string(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: ChannelLoggingConfig::default(),
        })
    }

    fn point(point_id: u32, protocol_mappings: Option<&str>) -> Point {
        Point {
            point_id,
            signal_name: format!("point-{point_id}"),
            description: None,
            unit: None,
            protocol_mappings: protocol_mappings.map(str::to_owned),
        }
    }

    #[test]
    fn snapshot_compile_rejects_the_complete_generation_when_any_mapping_is_invalid() {
        let mut runtime = runtime_snapshot();
        runtime.telemetry_points = vec![
            TelemetryPoint {
                base: point(1, Some(r#"{"json_path":"$.valid","data_type":"float"}"#)),
                scale: 1.0,
                offset: 0.0,
                data_type: "float64".to_string(),
                reverse: false,
            },
            TelemetryPoint {
                base: point(2, Some(r#"{"json_path":"invalid[[[","data_type":"float"}"#)),
                scale: 1.0,
                offset: 0.0,
                data_type: "float64".to_string(),
                reverse: false,
            },
        ];

        let error = JsonMapper::from_runtime_config(&runtime)
            .expect_err("a partial mapping generation must fail closed");

        assert!(error.to_string().contains("point 2"));
        assert!(error.to_string().contains("Invalid JSONPath"));
    }

    #[test]
    fn snapshot_compile_reads_only_acquisition_point_planes() {
        let mut runtime = runtime_snapshot();
        runtime.telemetry_points.push(TelemetryPoint {
            base: point(1, Some(r#"{"json_path":"$.value1"}"#)),
            scale: 1.0,
            offset: 0.0,
            data_type: "float64".to_string(),
            reverse: false,
        });
        runtime.signal_points.push(SignalPoint {
            base: point(2, Some(r#"{"json_path":"$.value2"}"#)),
            reverse: false,
        });
        let mapper = JsonMapper::from_runtime_config(&runtime).expect("complete inline generation");

        assert_eq!(mapper.len(), 2);
        assert_eq!(mapper.mappings[0].point_type, PointType::Telemetry);
        assert_eq!(mapper.mappings[1].point_type, PointType::Signal);
    }

    #[test]
    fn snapshot_compile_rejects_control_and_adjustment_mappings() {
        let mut control_runtime = runtime_snapshot();
        control_runtime.control_points.push(ControlPoint {
            base: point(3, Some(r#"{"json_path":"$.command"}"#)),
            reverse: false,
            control_type: "latching".to_string(),
            on_value: 1,
            off_value: 0,
            pulse_duration_ms: None,
        });
        let control_error = JsonMapper::from_runtime_config(&control_runtime)
            .expect_err("read-only JSON channels cannot acquire control points");
        assert!(control_error.to_string().contains("C mappings"));

        let mut adjustment_runtime = runtime_snapshot();
        adjustment_runtime.adjustment_points.push(AdjustmentPoint {
            base: point(4, Some(r#"{"json_path":"$.setpoint"}"#)),
            min_value: None,
            max_value: None,
            step: 1.0,
            data_type: "float64".to_string(),
            scale: 1.0,
            offset: 0.0,
        });
        let adjustment_error = JsonMapper::from_runtime_config(&adjustment_runtime)
            .expect_err("read-only JSON channels cannot acquire adjustment points");
        assert!(adjustment_error.to_string().contains("A mappings"));
    }

    #[test]
    fn snapshot_compile_skips_absent_and_empty_mappings() {
        let mut runtime = runtime_snapshot();
        runtime.signal_points = vec![
            SignalPoint {
                base: point(1, None),
                reverse: false,
            },
            SignalPoint {
                base: point(2, Some("  ")),
                reverse: false,
            },
            SignalPoint {
                base: point(3, Some("{}")),
                reverse: false,
            },
            SignalPoint {
                base: point(4, Some("null")),
                reverse: false,
            },
        ];

        let mapper =
            JsonMapper::from_runtime_config(&runtime).expect("empty mappings are optional");

        assert!(mapper.is_empty());
    }

    #[test]
    fn snapshot_compile_rejects_string_values_that_cannot_enter_live_state() {
        let mut runtime = runtime_snapshot();
        runtime.telemetry_points.push(TelemetryPoint {
            base: point(5, Some(r#"{"json_path":"$.label","data_type":"string"}"#)),
            scale: 1.0,
            offset: 0.0,
            data_type: "string".to_string(),
            reverse: false,
        });

        let error = JsonMapper::from_runtime_config(&runtime)
            .expect_err("live JSON mappings must produce numeric or bool samples");

        assert!(error.to_string().contains("point 5"));
        assert!(error.to_string().contains("float, int, or bool"));
    }

    #[test]
    fn snapshot_compile_rejects_cross_plane_data_types() {
        let mut telemetry_runtime = runtime_snapshot();
        telemetry_runtime.telemetry_points.push(TelemetryPoint {
            base: point(6, Some(r#"{"json_path":"$.active","data_type":"bool"}"#)),
            scale: 1.0,
            offset: 0.0,
            data_type: "bool".to_string(),
            reverse: false,
        });
        let telemetry_error = JsonMapper::from_runtime_config(&telemetry_runtime)
            .expect_err("telemetry mappings cannot declare bool");
        assert!(telemetry_error.to_string().contains("require float or int"));

        let mut signal_runtime = runtime_snapshot();
        signal_runtime.signal_points.push(SignalPoint {
            base: point(7, Some(r#"{"json_path":"$.state","data_type":"int"}"#)),
            reverse: false,
        });
        let signal_error = JsonMapper::from_runtime_config(&signal_runtime)
            .expect_err("signal mappings cannot declare int");
        assert!(signal_error.to_string().contains("require bool"));
    }

    #[test]
    fn snapshot_compile_rejects_non_finite_point_transform() {
        let mut runtime = runtime_snapshot();
        runtime.telemetry_points.push(TelemetryPoint {
            base: point(8, Some(r#"{"json_path":"$.value"}"#)),
            scale: f64::NAN,
            offset: 0.0,
            data_type: "float64".to_string(),
            reverse: false,
        });

        let error =
            JsonMapper::from_runtime_config(&runtime).expect_err("NaN scale must fail closed");
        assert!(
            error
                .to_string()
                .contains("scale and offset must be finite")
        );
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
    fn telemetry_transform_comes_from_the_point_row() {
        let mut runtime = runtime_snapshot();
        runtime.telemetry_points.push(TelemetryPoint {
            base: point(102, Some(r#"{"json_path":"$.sensor.temp"}"#)),
            scale: 0.1,
            offset: -10.0,
            data_type: "float64".to_string(),
            reverse: false,
        });
        let mapper = JsonMapper::from_runtime_config(&runtime).expect("point row transform");

        let payload = br#"{"sensor": {"temp": 250}}"#;
        let batch = mapper.parse(payload).unwrap();
        assert_eq!(batch.len(), 1);
        let point = batch.iter().next().unwrap();
        // 250 * 0.1 + (-10) = 15.0
        assert!((point.value.as_f64().unwrap() - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn signal_reverse_comes_from_the_point_row() {
        let mut runtime = runtime_snapshot();
        runtime.signal_points.push(SignalPoint {
            base: point(103, Some(r#"{"json_path":"$.open"}"#)),
            reverse: true,
        });
        let mapper = JsonMapper::from_runtime_config(&runtime).expect("signal reverse transform");

        let batch = mapper.parse(br#"{"open":true}"#).unwrap();

        assert_eq!(batch.iter().next().unwrap().value.as_bool(), Some(false));
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
    fn numeric_conversion_rejects_non_finite_and_out_of_range_values() {
        assert!(convert_value(&json!("NaN"), JsonDataType::Float, 1.0, 0.0, false).is_err());
        assert!(convert_value(&json!(2.0), JsonDataType::Float, f64::MAX, 0.0, false).is_err());
        assert!(convert_value(&json!("1e30"), JsonDataType::Int, 1.0, 0.0, false).is_err());
    }

    #[test]
    fn bool_conversion_accepts_only_explicit_boolean_values() {
        for (raw, expected) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
            (json!("TRUE"), true),
            (json!("false"), false),
        ] {
            let converted =
                convert_value(&raw, JsonDataType::Bool, 1.0, 0.0, false).expect("explicit bool");
            assert_eq!(converted.as_bool(), Some(expected));
        }
        for raw in [json!(2), json!(-1), json!("yes"), json!(""), json!(null)] {
            assert!(
                convert_value(&raw, JsonDataType::Bool, 1.0, 0.0, false).is_err(),
                "unexpectedly accepted {raw}"
            );
        }
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
    fn retired_unused_channel_mapping_fields_are_rejected() {
        for (field, value) in [
            ("transform_script", json!("/tmp/transform.py")),
            ("device_id_path", json!("$.device.id")),
        ] {
            let error = serde_json::from_value::<JsonMappingConfig>(json!({(field): value}))
                .expect_err("retired JSON mapping configuration must fail closed");

            assert!(error.to_string().contains(field));
        }
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
