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
//! - [`SlotStore`]: Single-writer store for CAN/J1939 cached data
//! - [`ShardedSlotStore`]: Multi-writer store for VirtualChannel

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};

use aether_model::PointType;
use chrono::{DateTime, Utc};
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

    /// Get all pin states as a vector of (pin_id, state) pairs.
    pub fn get_all(&self) -> Vec<(u32, bool)> {
        self.index
            .iter()
            .map(|(&pin_id, &idx)| (pin_id, self.states[idx].load(Ordering::Acquire)))
            .collect()
    }

    /// Get the number of pins in the store.
    #[inline]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Check if the store is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Check if a pin ID exists in the store.
    #[inline]
    pub fn contains(&self, pin_id: u32) -> bool {
        self.index.contains_key(&pin_id)
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

    /// Get the current version counter.
    ///
    /// Useful for change detection without reading the full value.
    #[inline]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
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
/// Designed for single-writer scenarios like CAN/J1939 data caching.
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

    /// Create a new store for Telemetry points (convenience).
    pub fn telemetry(point_ids: &[u32]) -> Self {
        Self::from_points(point_ids, PointType::Telemetry)
    }

    /// Get a reference to a data slot by point ID.
    #[inline]
    pub fn get(&self, point_id: u32) -> Option<&DataSlot> {
        self.index.get(&point_id).map(|&idx| &self.slots[idx])
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

    /// Get the number of slots in the store.
    #[inline]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Check if the store is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Check if a point ID exists in the store.
    #[inline]
    pub fn contains(&self, point_id: u32) -> bool {
        self.index.contains_key(&point_id)
    }

    /// Get all point IDs in the store.
    pub fn point_ids(&self) -> &[u32] {
        &self.point_ids
    }
}

// ============================================================================
// ShardedSlotStore - Multi-writer store with sharding
// ============================================================================

/// Sharded slot store for multi-writer scenarios.
///
/// Divides slots into shards to reduce lock contention.
/// Suitable for VirtualChannel where multiple sources may write concurrently.
///
/// # Sharding Strategy
///
/// Points are distributed across shards based on `point_id % shard_count`.
/// Each shard has its own lock, so concurrent writes to different shards
/// don't block each other.
#[derive(Debug)]
pub struct ShardedSlotStore {
    /// Sharded storage: each shard is independently locked
    shards: Vec<RwLock<Vec<DataSlot>>>,
    /// point_id -> (shard_idx, slot_idx_within_shard)
    index: HashMap<u32, (usize, usize)>,
    /// Reverse mapping per shard: slot_idx -> point_id
    shard_point_ids: Vec<Vec<u32>>,
    /// Number of shards
    shard_count: usize,
    /// SCADA point type for all points in this store
    point_type: PointType,
}

impl ShardedSlotStore {
    /// Create a new sharded store with specified point type.
    ///
    /// # Arguments
    ///
    /// * `point_ids` - List of all point IDs
    /// * `shard_count` - Number of shards (typically CPU core count or fixed like 16)
    /// * `point_type` - SCADA point type for all points
    pub fn new(point_ids: &[u32], shard_count: usize, point_type: PointType) -> Self {
        let shard_count = shard_count.max(1);

        // Pre-calculate shard assignments
        let mut shard_points: Vec<Vec<u32>> = vec![Vec::new(); shard_count];
        for &point_id in point_ids {
            let shard_idx = (point_id as usize) % shard_count;
            shard_points[shard_idx].push(point_id);
        }

        // Build shards and index
        let mut index = HashMap::with_capacity(point_ids.len());
        let mut shards = Vec::with_capacity(shard_count);
        let mut shard_point_ids = Vec::with_capacity(shard_count);

        for (shard_idx, points) in shard_points.into_iter().enumerate() {
            let mut slots = Vec::with_capacity(points.len());
            let mut ids = Vec::with_capacity(points.len());

            for (slot_idx, point_id) in points.into_iter().enumerate() {
                index.insert(point_id, (shard_idx, slot_idx));
                slots.push(DataSlot::new());
                ids.push(point_id);
            }

            shards.push(RwLock::new(slots));
            shard_point_ids.push(ids);
        }

        Self {
            shards,
            index,
            shard_point_ids,
            shard_count,
            point_type,
        }
    }

    /// Update a single point's value.
    ///
    /// Only locks the shard containing this point.
    pub fn update(&self, point_id: u32, value: Value, quality: Quality) {
        if let Some(&(shard_idx, slot_idx)) = self.index.get(&point_id) {
            let shard = self.shards[shard_idx]
                .read()
                .unwrap_or_else(|e| e.into_inner());
            shard[slot_idx].update(value, quality);
        }
    }

    /// Read a single point's value.
    pub fn read(&self, point_id: u32) -> Option<(Value, Quality, i64, u64)> {
        if let Some(&(shard_idx, slot_idx)) = self.index.get(&point_id) {
            let shard = self.shards[shard_idx]
                .read()
                .unwrap_or_else(|e| e.into_inner());
            shard[slot_idx].read()
        } else {
            None
        }
    }

    /// Export all data as a DataBatch.
    pub fn export_all(&self) -> DataBatch {
        let total_capacity: usize = self.shard_point_ids.iter().map(|v| v.len()).sum();
        let mut batch = DataBatch::with_capacity(total_capacity);
        let now = Utc::now();
        for (shard_idx, shard) in self.shards.iter().enumerate() {
            let slots = shard.read().unwrap_or_else(|e| e.into_inner());
            for (slot_idx, slot) in slots.iter().enumerate() {
                let pid = self.shard_point_ids[shard_idx][slot_idx];
                if let Some(point) = slot_to_point(slot, pid, self.point_type, now) {
                    batch.add(point);
                }
            }
        }
        batch
    }

    /// Get the number of slots across all shards.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Check if the store is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Check if a point ID exists in the store.
    #[inline]
    pub fn contains(&self, point_id: u32) -> bool {
        self.index.contains_key(&point_id)
    }

    /// Get the number of shards.
    #[inline]
    pub fn shard_count(&self) -> usize {
        self.shard_count
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

        assert_eq!(store.len(), 3);
        assert!(store.contains(1));
        assert!(!store.contains(99));

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

        // Get all
        let all = store.get_all();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_data_slot() {
        let slot = DataSlot::new();

        assert_eq!(slot.version(), 0);
        assert!(slot.read().is_none());

        slot.update(Value::Float(42.5), Quality::Good);
        assert_eq!(slot.version(), 1);

        let (value, quality, _ts, version) = slot.read().unwrap();
        assert_eq!(value, Value::Float(42.5));
        assert_eq!(quality, Quality::Good);
        assert_eq!(version, 1);

        // Update again
        slot.update(Value::Integer(100), Quality::Uncertain);
        assert_eq!(slot.version(), 2);

        let (value, quality, _ts, version) = slot.read().unwrap();
        assert_eq!(value, Value::Integer(100));
        assert_eq!(quality, Quality::Uncertain);
        assert_eq!(version, 2);
    }

    #[test]
    fn test_slot_store() {
        let store = SlotStore::from_points(&[10, 20, 30], PointType::Telemetry);

        assert_eq!(store.len(), 3);
        assert!(store.contains(10));
        assert!(!store.contains(99));

        // Update single
        store.update(10, Value::Float(1.0), Quality::Good);
        store.update(20, Value::Float(2.0), Quality::Good);
        store.update(30, Value::Float(30.0), Quality::Good);

        let slot = store.get(10).unwrap();
        let (value, _, _, _) = slot.read().unwrap();
        assert_eq!(value, Value::Float(1.0));

        // Export
        let batch = store.export_all();
        assert_eq!(batch.len(), 3);
        // Verify point_type is set correctly
        for point in batch.iter() {
            assert_eq!(point.point_type, PointType::Telemetry);
        }
    }

    #[test]
    fn test_sharded_slot_store() {
        let store = ShardedSlotStore::new(&[1, 2, 3, 4, 5, 6, 7, 8], 4, PointType::Signal);

        assert_eq!(store.len(), 8);
        assert_eq!(store.shard_count(), 4);
        assert!(store.contains(1));
        assert!(!store.contains(99));

        // Update and read
        store.update(1, Value::Float(1.0), Quality::Good);
        store.update(5, Value::Float(5.0), Quality::Good);

        let (value, quality, _, _) = store.read(1).unwrap();
        assert_eq!(value, Value::Float(1.0));
        assert_eq!(quality, Quality::Good);

        let (value, _, _, _) = store.read(5).unwrap();
        assert_eq!(value, Value::Float(5.0));

        // Non-existent
        assert!(store.read(99).is_none());

        // Export
        let batch = store.export_all();
        assert_eq!(batch.len(), 2); // Only 2 points have values
    }
}
