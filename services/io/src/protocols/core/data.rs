//! Data types for the Industrial Gateway.
//!
//! This module defines the core data model for protocol-agnostic data representation.
//!
//! ## Point Type Design
//!
//! Each `DataPoint` carries an explicit `point_type` field to identify its SCADA category:
//! - **Telemetry (T)**: Analog measurements (e.g., voltage, current, temperature)
//! - **Signal (S)**: Digital status (e.g., switch state, alarm)
//! - **Control (C)**: Digital commands (e.g., on/off)
//! - **Adjustment (A)**: Analog setpoints (e.g., target power)
//!
//! This explicit field replaces the previous u32/4 encoding scheme where point types
//! were encoded into the `id` field via offset arithmetic.

use aether_model::PointType;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocols::core::quality::Quality;

/// A protocol-agnostic value representation.
///
/// This enum provides a unified way to represent values from different protocols.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// Floating-point number (most common for analog values)
    Float(f64),

    /// Integer value
    Integer(i64),

    /// Boolean value (common for digital I/O)
    Bool(bool),

    /// String value
    String(String),

    /// Raw bytes
    Bytes(Vec<u8>),

    /// Null/missing value
    #[default]
    Null,
}

impl Value {
    /// Try to get the value as f64.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Integer(v) => Some(*v as f64),
            Self::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// Try to get the value as i64.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            Self::Float(v) => Some(*v as i64),
            Self::Bool(v) => Some(if *v { 1 } else { 0 }),
            _ => None,
        }
    }

    /// Try to get the value as bool.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            Self::Integer(v) => Some(*v != 0),
            Self::Float(v) => Some(*v != 0.0),
            _ => None,
        }
    }

    /// Try to get the value as string.
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Check if this is a null value.
    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

// Convenient From implementations
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Self::Float(v as f64)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<i16> for Value {
    fn from(v: i16) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<u16> for Value {
    fn from(v: u16) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::String(v.to_string())
    }
}

/// A single data point with timestamp and quality.
///
/// Each point carries its SCADA category (Telemetry/Signal/Control/Adjustment)
/// explicitly via the `point_type` field. This replaces the previous u32/4
/// encoding scheme.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPoint {
    /// Point identifier (original point_id, no encoding)
    pub id: u32,

    /// SCADA point type (T/S/C/A)
    pub point_type: PointType,

    /// The value
    pub value: Value,

    /// Data quality indicator (protocol-layer concept)
    #[serde(default)]
    pub quality: Quality,

    /// Server timestamp (when gateway received the data)
    pub timestamp: DateTime<Utc>,

    /// Source timestamp (when device generated the data, if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_timestamp: Option<DateTime<Utc>>,
}

impl DataPoint {
    /// Create a new data point with explicit point type.
    ///
    /// # Arguments
    /// * `id` - Original point identifier (no encoding needed)
    /// * `point_type` - SCADA category (Telemetry/Signal/Control/Adjustment)
    /// * `value` - The point value
    pub fn new(id: u32, point_type: PointType, value: impl Into<Value>) -> Self {
        Self {
            id,
            point_type,
            value: value.into(),
            quality: Quality::Good,
            timestamp: Utc::now(),
            source_timestamp: None,
        }
    }

    /// Create a Telemetry point (convenience for the most common case).
    ///
    /// Equivalent to `DataPoint::new(id, PointType::Telemetry, value)`.
    pub fn telemetry(id: u32, value: impl Into<Value>) -> Self {
        Self::new(id, PointType::Telemetry, value)
    }

    /// Create a Signal point (convenience for digital status).
    pub fn signal(id: u32, value: impl Into<Value>) -> Self {
        Self::new(id, PointType::Signal, value)
    }

    /// Create a Control point (convenience for digital commands).
    pub fn control(id: u32, value: impl Into<Value>) -> Self {
        Self::new(id, PointType::Control, value)
    }

    /// Create an Adjustment point (convenience for analog setpoints).
    pub fn adjustment(id: u32, value: impl Into<Value>) -> Self {
        Self::new(id, PointType::Adjustment, value)
    }

    /// Set the quality.
    #[must_use]
    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }
}

/// A batch of data points.
///
/// Simple collection without SCADA-level categorization.
/// The application layer is responsible for routing/storing
/// points based on their type (determined by id lookup).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataBatch {
    /// All data points in this batch
    points: Vec<DataPoint>,
}

impl DataBatch {
    /// Create an empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty batch with pre-allocated capacity.
    ///
    /// Use this when you know the approximate number of points to avoid
    /// repeated reallocations during `add()` calls.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            points: Vec::with_capacity(capacity),
        }
    }

    /// Create a batch from a vector of points.
    pub fn from_points(points: Vec<DataPoint>) -> Self {
        Self { points }
    }

    /// Add a data point.
    pub fn add(&mut self, point: DataPoint) {
        self.points.push(point);
    }

    /// Get total number of points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Check if batch is empty.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Merge another batch into this one.
    pub fn merge(&mut self, other: DataBatch) {
        self.points.extend(other.points);
    }

    /// Iterate over all points.
    pub fn iter(&self) -> impl Iterator<Item = &DataPoint> {
        self.points.iter()
    }

    /// Get mutable iterator over all points.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut DataPoint> {
        self.points.iter_mut()
    }
}

impl IntoIterator for DataBatch {
    type Item = DataPoint;
    type IntoIter = std::vec::IntoIter<DataPoint>;

    fn into_iter(self) -> Self::IntoIter {
        self.points.into_iter()
    }
}

impl<'a> IntoIterator for &'a DataBatch {
    type Item = &'a DataPoint;
    type IntoIter = std::slice::Iter<'a, DataPoint>;

    fn into_iter(self) -> Self::IntoIter {
        self.points.iter()
    }
}

impl FromIterator<DataPoint> for DataBatch {
    fn from_iter<I: IntoIterator<Item = DataPoint>>(iter: I) -> Self {
        Self {
            points: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_conversions() {
        let v = Value::from(42.5);
        assert_eq!(v.as_f64(), Some(42.5));
        assert_eq!(v.as_i64(), Some(42));

        let v = Value::from(true);
        assert_eq!(v.as_bool(), Some(true));
        assert_eq!(v.as_f64(), Some(1.0));
    }

    #[test]
    fn test_data_point() {
        // Test with explicit point_type
        let point = DataPoint::new(1, PointType::Telemetry, 25.5);
        assert_eq!(point.id, 1);
        assert_eq!(point.point_type, PointType::Telemetry);
        assert_eq!(point.value.as_f64(), Some(25.5));
        assert_eq!(point.quality, Quality::Good);
    }

    #[test]
    fn test_data_point_convenience_constructors() {
        // Test convenience constructors
        let t = DataPoint::telemetry(1, 100.0);
        assert_eq!(t.point_type, PointType::Telemetry);

        let s = DataPoint::signal(2, true);
        assert_eq!(s.point_type, PointType::Signal);

        let c = DataPoint::control(3, false);
        assert_eq!(c.point_type, PointType::Control);

        let a = DataPoint::adjustment(4, 50.0);
        assert_eq!(a.point_type, PointType::Adjustment);
    }

    #[test]
    fn test_data_batch() {
        let mut batch = DataBatch::new();
        batch.add(DataPoint::telemetry(1, 25.5));
        batch.add(DataPoint::signal(2, true));

        assert_eq!(batch.len(), 2);
        assert!(!batch.is_empty());

        // Test iteration
        let ids: Vec<u32> = batch.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn test_data_batch_from_iter() {
        let points = vec![DataPoint::telemetry(1, 1.0), DataPoint::telemetry(2, 2.0)];
        let batch: DataBatch = points.into_iter().collect();
        assert_eq!(batch.len(), 2);
    }
}
