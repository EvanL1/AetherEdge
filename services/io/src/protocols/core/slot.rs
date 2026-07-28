//! High-performance slot-based storage for real-time data.
//!
//! This module provides Vec+Index based storage structures that replace
//! HashMap/DashMap for better cache locality and reduced lock contention.
//!
//! # Design Philosophy
//!
//! Industrial gateway point configurations are fixed after startup.
//! Instead of using hash-based lookups at runtime:
//! - **Startup**: Build `point_id -> index` mapping once
//! - **Runtime**: Use `Vec<Slot>` for O(1) array indexing, cache-friendly access
//!
//! # Available Stores
//!
//! - [`AtomicBoolStore`]: Lock-free boolean storage for GPIO DO states
//! - [`SlotStore`]: Single-writer store for cached CAN data

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use common::PointType;
use tracing::warn;

use super::data::{DataBatch, DataPoint, Value};
use super::quality::Quality;

// ============================================================================
// AtomicBoolStore - Lock-free boolean storage for GPIO
// ============================================================================

/// Lock-free boolean storage for GPIO output states.
///
/// Uses `AtomicBool` array for completely lock-free read/write operations.
/// Ideal for GPIO DO (Digital Output) state tracking where values are simple booleans.
///
/// # Example
///
/// ```ignore
/// let store = AtomicBoolStore::from_pins(&[1, 2, 3]);
/// store.set(1, true);
/// assert_eq!(store.get(1), Some(true));
/// ```
#[derive(Debug)]
pub struct AtomicBoolStore {
    /// Boolean states stored as atomic values
    states: Vec<AtomicBool>,
    /// point_id -> slot index mapping (read-only after construction)
    index: HashMap<u32, usize>,
}

impl AtomicBoolStore {
    /// Create a new store from a list of pin IDs.
    ///
    /// All pins are initialized to `false`.
    pub fn from_pins(pin_ids: &[u32]) -> Self {
        let mut index = HashMap::with_capacity(pin_ids.len());
        let mut states = Vec::with_capacity(pin_ids.len());

        for (idx, &pin_id) in pin_ids.iter().enumerate() {
            index.insert(pin_id, idx);
            states.push(AtomicBool::new(false));
        }

        Self { states, index }
    }

    /// Get the current state of a pin (lock-free).
    #[inline]
    pub fn get(&self, pin_id: u32) -> Option<bool> {
        self.index
            .get(&pin_id)
            .map(|&idx| self.states[idx].load(Ordering::Acquire))
    }

    /// Set the state of a pin (lock-free).
    #[inline]
    pub fn set(&self, pin_id: u32, value: bool) {
        if let Some(&idx) = self.index.get(&pin_id) {
            self.states[idx].store(value, Ordering::Release);
        }
    }
}

// ============================================================================
// DataSlot - Single data point storage slot
// ============================================================================

/// A single data point storage slot with atomic version tracking.
///
/// Supports efficient single-writer, multi-reader access pattern.
/// The version counter enables change detection without reading the full value.
#[derive(Debug)]
pub struct DataSlot {
    /// Data value (requires lock due to non-Copy types like String)
    value: RwLock<Option<Value>>,
    /// Quality code stored as u8 for atomic access
    quality: AtomicU8,
    /// Timestamp as Unix milliseconds
    timestamp_ms: AtomicI64,
    /// Version counter for change detection (incremented on each update)
    version: AtomicU64,
}

impl DataSlot {
    /// Create a new empty data slot.
    pub fn new() -> Self {
        Self {
            value: RwLock::new(None),
            quality: AtomicU8::new(Quality::Good as u8),
            timestamp_ms: AtomicI64::new(0),
            version: AtomicU64::new(0),
        }
    }

    /// Update the slot value (typically called by single writer).
    ///
    /// Increments the version counter atomically.
    pub fn update(&self, value: Value, quality: Quality) {
        let now_ms = Utc::now().timestamp_millis();

        // Update atomic fields first
        self.quality.store(quality as u8, Ordering::Release);
        self.timestamp_ms.store(now_ms, Ordering::Release);

        // Update value under write lock
        {
            let mut guard = self.value.write().unwrap_or_else(|e| {
                warn!("DataSlot RwLock poisoned, recovering");
                e.into_inner()
            });
            *guard = Some(value);
        }

        // Increment version last to signal update complete
        self.version.fetch_add(1, Ordering::Release);
    }

    /// Read the current value, quality, timestamp, and version.
    ///
    /// Returns `None` if the slot has never been written to.
    pub fn read(&self) -> Option<(Value, Quality, i64, u64)> {
        let guard = self.value.read().unwrap_or_else(|e| {
            warn!("DataSlot RwLock poisoned on read, recovering");
            e.into_inner()
        });
        guard.as_ref().map(|v| {
            (
                v.clone(),
                quality_from_u8(self.quality.load(Ordering::Acquire)),
                self.timestamp_ms.load(Ordering::Acquire),
                self.version.load(Ordering::Acquire),
            )
        })
    }
}

impl Default for DataSlot {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// SlotStore - Single-writer, multi-reader store
// ============================================================================

/// Vec-based storage with O(1) index lookup.
///
/// Designed for single-writer scenarios like CAN data caching.
/// All point IDs must be known at construction time.
///
/// # Performance
///
/// - **Lookup**: O(1) via index HashMap + array access
/// - **Update**: O(1) direct array write
/// - **Memory**: Contiguous, cache-friendly layout
#[derive(Debug)]
pub struct SlotStore {
    /// Contiguous storage of data slots
    slots: Vec<DataSlot>,
    /// point_id -> slot index mapping (read-only after construction)
    index: HashMap<u32, usize>,
    /// Reverse mapping: slot index -> point_id
    point_ids: Vec<u32>,
    /// SCADA point type for all points in this store
    point_type: PointType,
}

impl SlotStore {
    /// Create an empty store with no points.
    ///
    /// Useful for initialization when points are not yet known.
    /// Use `from_points()` to create a fully configured store.
    pub fn empty() -> Self {
        Self {
            slots: Vec::new(),
            index: HashMap::new(),
            point_ids: Vec::new(),
            point_type: PointType::Telemetry, // Default type
        }
    }

    /// Create a new store from a list of point IDs with specified point type.
    pub fn from_points(point_ids: &[u32], point_type: PointType) -> Self {
        let mut index = HashMap::with_capacity(point_ids.len());
        let mut slots = Vec::with_capacity(point_ids.len());
        let mut ids = Vec::with_capacity(point_ids.len());

        for (idx, &point_id) in point_ids.iter().enumerate() {
            index.insert(point_id, idx);
            slots.push(DataSlot::new());
            ids.push(point_id);
        }

        Self {
            slots,
            index,
            point_ids: ids,
            point_type,
        }
    }

    /// Update a single point's value.
    pub fn update(&self, point_id: u32, value: Value, quality: Quality) {
        if let Some(&idx) = self.index.get(&point_id) {
            self.slots[idx].update(value, quality);
        }
    }

    /// Export all data as a DataBatch.
    ///
    /// Only includes points that have been written to.
    pub fn export_all(&self) -> DataBatch {
        let mut batch = DataBatch::with_capacity(self.slots.len());
        let now = Utc::now();
        for (idx, slot) in self.slots.iter().enumerate() {
            if let Some(point) = slot_to_point(slot, self.point_ids[idx], self.point_type, now) {
                batch.add(point);
            }
        }
        batch
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Export a slot's data as a DataPoint, if it has a value.
#[inline]
fn slot_to_point(
    slot: &DataSlot,
    point_id: u32,
    point_type: PointType,
    fallback_now: DateTime<Utc>,
) -> Option<DataPoint> {
    let (value, quality, ts_ms, _) = slot.read()?;
    Some(DataPoint {
        id: point_id,
        point_type,
        value,
        quality,
        timestamp: DateTime::from_timestamp_millis(ts_ms).unwrap_or(fallback_now),
        source_timestamp: None,
    })
}

/// Convert u8 back to Quality enum.
#[inline]
fn quality_from_u8(v: u8) -> Quality {
    match v {
        0 => Quality::Good,
        1 => Quality::Bad,
        2 => Quality::Uncertain,
        3 => Quality::Invalid,
        4 => Quality::NotConnected,
        5 => Quality::DeviceFailure,
        6 => Quality::SensorFailure,
        7 => Quality::CommFailure,
        8 => Quality::OutOfService,
        9 => Quality::Substituted,
        10 => Quality::Overflow,
        11 => Quality::Underflow,
        12 => Quality::ConfigError,
        13 => Quality::LastKnown,
        _ => Quality::Bad, // Default to Bad for unknown values
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn test_atomic_bool_store() {
        let store = AtomicBoolStore::from_pins(&[1, 2, 3]);

        // Initial values are false
        assert_eq!(store.get(1), Some(false));
        assert_eq!(store.get(2), Some(false));
        assert_eq!(store.get(99), None);

        // Set and get
        store.set(1, true);
        assert_eq!(store.get(1), Some(true));

        store.set(2, true);
        store.set(1, false);
        assert_eq!(store.get(1), Some(false));
        assert_eq!(store.get(2), Some(true));
    }

    #[test]
    fn test_data_slot() {
        let slot = DataSlot::new();

        assert!(slot.read().is_none());

        slot.update(Value::Float(42.5), Quality::Good);

        let (value, quality, _ts, version) = slot.read().unwrap();
        assert_eq!(value, Value::Float(42.5));
        assert_eq!(quality, Quality::Good);
        assert_eq!(version, 1);

        // Update again
        slot.update(Value::Integer(100), Quality::Uncertain);

        let (value, quality, _ts, version) = slot.read().unwrap();
        assert_eq!(value, Value::Integer(100));
        assert_eq!(quality, Quality::Uncertain);
        assert_eq!(version, 2);
    }

    #[test]
    fn test_slot_store() {
        let store = SlotStore::from_points(&[10, 20, 30], PointType::Telemetry);

        // Update single
        store.update(10, Value::Float(1.0), Quality::Good);
        store.update(20, Value::Float(2.0), Quality::Good);
        store.update(30, Value::Float(30.0), Quality::Good);

        // Export
        let batch = store.export_all();
        assert_eq!(batch.len(), 3);
        // Verify point_type is set correctly
        for point in batch.iter() {
            assert_eq!(point.point_type, PointType::Telemetry);
        }
    }
}
