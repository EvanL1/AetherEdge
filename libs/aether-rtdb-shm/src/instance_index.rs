//! Dynamic Instance Index for Unified Pool Architecture
//!
//! Supports hot add/remove of instances with dual indexing:
//! - **own_slots**: Instance's private slots (M/A points)
//! - **shared_slots**: References to Channel slots (via routing)
//!
//! # Design
//!
//! ```text
//! InstanceIndex (ArcSwap)
//! ┌──────────────────────────────────────────────────────────┐
//! │ 23 → DynamicInstanceLayout {                                    │
//! │        own_base: 200,           // Private slot base       │
//! │        own_counts: [M:5, A:3],  // Private M/A point counts│
//! │        shared_slots: [          // References to Channel slots│
//! │          SharedSlotRef { slot: 42, M, 7 },               │
//! │          SharedSlotRef { slot: 45, M, 8 },               │
//! │        ]                                                  │
//! │      }                                                    │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! # Shared Slots Concept
//!
//! When a route is created: `ch:1001:T:5 → inst:23:M:7`
//! - Channel 1001's slot for T:5 is **not copied**
//! - Instance 23 adds a SharedSlotRef pointing to that slot
//! - Reading inst:23:M:7 goes directly to Channel's slot
//!
//! This is the core of the "unified pool" - data stays in place, only indexes change.

use aether_model::PointType;
use arc_swap::ArcSwap;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use crate::core::bitmap::{SlotAllocation, SlotBitmap};

/// Reference to a shared slot (owned by a Channel)
#[derive(Clone, Debug, PartialEq)]
pub struct SharedSlotRef {
    /// Slot index in unified pool (points to Channel's slot)
    pub slot_id: usize,
    /// Point type as seen by Instance
    pub point_type: PointType,
    /// Point ID within Instance
    pub point_id: u32,
    /// Source channel ID (for debugging/API)
    pub source_channel_id: u32,
    /// Source point type in Channel
    pub source_point_type: PointType,
    /// Source point ID in Channel
    pub source_point_id: u32,
}

/// Instance layout - allocation info for one instance
///
/// Supports dual indexing: own slots + shared slots
#[derive(Clone, Debug)]
pub struct DynamicInstanceLayout {
    /// Instance ID
    pub instance_id: u32,
    /// Base slot index for own slots
    pub own_base: usize,
    /// Point count for own slots [M, A] (Measurement, Action)
    /// Note: Instances only have M and A types (not T/S/C)
    pub own_counts: [u32; 2],
    /// Total own points
    pub own_total: u32,
    /// Allocation info for own slots (for freeing)
    pub own_allocation: Option<SlotAllocation>,
    /// Shared slots - references to Channel slots
    pub shared_slots: Vec<SharedSlotRef>,
    /// Fast lookup index for shared slots: (point_type, point_id) -> slot_id
    shared_lookup: FxHashMap<(PointType, u32), usize>,
}

impl DynamicInstanceLayout {
    /// Create a new instance layout with own slots
    pub fn new(instance_id: u32, allocation: Option<SlotAllocation>, own_counts: [u32; 2]) -> Self {
        let own_total: u32 = own_counts.iter().sum();
        let own_base = allocation.as_ref().map(|a| a.base_slot).unwrap_or(0);
        Self {
            instance_id,
            own_base,
            own_counts,
            own_total,
            own_allocation: allocation,
            shared_slots: Vec::new(),
            shared_lookup: FxHashMap::default(),
        }
    }

    /// Get slot index for an own point (M or A)
    ///
    /// Layout: [M...][A...]
    #[inline]
    pub fn own_slot(&self, point_type: PointType, point_id: u32) -> Option<usize> {
        // Only M and A are valid for own slots
        let type_idx = match point_type {
            PointType::Telemetry => 0,
            PointType::Adjustment => 1,
            _ => return None, // T, S, C not valid for Instance own slots
        };

        if point_id == 0 || point_id > self.own_counts[type_idx] {
            return None;
        }

        // Calculate offset: sum of previous types
        let offset: u32 = self.own_counts[..type_idx].iter().sum();
        Some(self.own_base + offset as usize + (point_id - 1) as usize)
    }

    /// Get slot for any point (checks shared slots first, then own slots)
    ///
    /// Priority: shared_slots (O(1) HashMap) → own_slots
    #[inline]
    pub fn slot(&self, point_type: PointType, point_id: u32) -> Option<usize> {
        // O(1) HashMap lookup instead of O(N) linear scan
        if let Some(&slot_id) = self.shared_lookup.get(&(point_type, point_id)) {
            return Some(slot_id);
        }

        // Then check own slots
        self.own_slot(point_type, point_id)
    }

    /// Add a shared slot reference (from routing)
    pub fn add_shared_slot(&mut self, slot_ref: SharedSlotRef) {
        let key = (slot_ref.point_type, slot_ref.point_id);
        // Check for duplicates via O(1) lookup
        if let std::collections::hash_map::Entry::Vacant(e) = self.shared_lookup.entry(key) {
            e.insert(slot_ref.slot_id);
            self.shared_slots.push(slot_ref);
        }
    }

    /// Remove a shared slot reference (from routing deletion)
    pub fn remove_shared_slot(
        &mut self,
        point_type: PointType,
        point_id: u32,
    ) -> Option<SharedSlotRef> {
        self.shared_lookup.remove(&(point_type, point_id));
        if let Some(pos) = self
            .shared_slots
            .iter()
            .position(|s| s.point_type == point_type && s.point_id == point_id)
        {
            Some(self.shared_slots.remove(pos))
        } else {
            None
        }
    }

    /// Check if layout has any points (own or shared)
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.own_total > 0 || !self.shared_slots.is_empty()
    }

    /// Get count for a specific own point type
    #[inline]
    pub fn own_count(&self, point_type: PointType) -> u32 {
        match point_type {
            PointType::Telemetry => self.own_counts[0],
            PointType::Adjustment => self.own_counts[1],
            _ => 0,
        }
    }

    /// Get total shared slot count
    #[inline]
    pub fn shared_count(&self) -> usize {
        self.shared_slots.len()
    }
}

/// Thread-safe instance index with hot update support
///
/// Uses ArcSwap for lock-free reads and COW updates.
pub struct InstanceIndex {
    /// Instance layouts indexed by instance_id
    inner: ArcSwap<FxHashMap<u32, DynamicInstanceLayout>>,
}

impl InstanceIndex {
    /// Create empty instance index
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(FxHashMap::default()),
        }
    }

    /// Get instance layout (lock-free read)
    #[inline]
    pub fn get(&self, instance_id: u32) -> Option<DynamicInstanceLayout> {
        let guard = self.inner.load();
        guard.get(&instance_id).cloned()
    }

    /// Get slot index for an instance point (lock-free)
    #[inline]
    pub fn slot(&self, instance_id: u32, point_type: PointType, point_id: u32) -> Option<usize> {
        let guard = self.inner.load();
        guard
            .get(&instance_id)
            .and_then(|layout| layout.slot(point_type, point_id))
    }

    /// Check if instance exists
    #[inline]
    pub fn contains(&self, instance_id: u32) -> bool {
        let guard = self.inner.load();
        guard.contains_key(&instance_id)
    }

    /// Get all instance IDs
    pub fn instance_ids(&self) -> Vec<u32> {
        let guard = self.inner.load();
        guard.keys().copied().collect()
    }

    /// Get instance count
    pub fn len(&self) -> usize {
        let guard = self.inner.load();
        guard.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ========== Hot Update Operations ==========

    /// Add a new instance (hot operation)
    ///
    /// # Arguments
    /// - `instance_id`: Instance ID to add
    /// - `own_counts`: Point counts for [M, A]
    /// - `bitmap`: SlotBitmap for allocation (can be None if no own points)
    ///
    /// # Returns
    /// - `Ok(DynamicInstanceLayout)`: Successfully added instance
    /// - `Err(String)`: Allocation failed or instance already exists
    pub fn add_instance(
        &self,
        instance_id: u32,
        own_counts: [u32; 2],
        bitmap: Option<&mut SlotBitmap>,
    ) -> Result<DynamicInstanceLayout, String> {
        // Check if instance already exists
        if self.contains(instance_id) {
            return Err(format!("Instance {} already exists", instance_id));
        }

        let total: u32 = own_counts.iter().sum();
        let allocation = if total > 0 {
            let bm = bitmap.ok_or("SlotBitmap required for instance with own points")?;
            Some(bm.alloc_contiguous(total as usize).ok_or_else(|| {
                format!(
                    "Failed to allocate {} slots for instance {}",
                    total, instance_id
                )
            })?)
        } else {
            None
        };

        let layout = DynamicInstanceLayout::new(instance_id, allocation, own_counts);

        // COW update
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicInstanceLayout> = (**guard).clone();
        new_map.insert(instance_id, layout.clone());
        self.inner.store(Arc::new(new_map));

        Ok(layout)
    }

    /// Remove an instance (hot operation)
    ///
    /// # Arguments
    /// - `instance_id`: Instance ID to remove
    /// - `bitmap`: SlotBitmap for freeing own slots
    ///
    /// # Returns
    /// - `Ok(DynamicInstanceLayout)`: Removed instance layout
    /// - `Err(String)`: Instance not found
    pub fn remove_instance(
        &self,
        instance_id: u32,
        bitmap: Option<&mut SlotBitmap>,
    ) -> Result<DynamicInstanceLayout, String> {
        let layout = self
            .get(instance_id)
            .ok_or_else(|| format!("Instance {} not found", instance_id))?;

        // COW update
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicInstanceLayout> = (**guard).clone();
        new_map.remove(&instance_id);
        self.inner.store(Arc::new(new_map));

        // Free own slots after index update
        if let (Some(allocation), Some(bm)) = (&layout.own_allocation, bitmap) {
            bm.free(allocation.base_slot, allocation.count);
        }

        Ok(layout)
    }

    /// Add a shared slot reference to an instance (for routing)
    ///
    /// This is called when a route is created: ch:X:T:Y → inst:Z:M:W
    pub fn add_shared_slot(&self, instance_id: u32, slot_ref: SharedSlotRef) -> Result<(), String> {
        let mut layout = self
            .get(instance_id)
            .ok_or_else(|| format!("Instance {} not found", instance_id))?;

        layout.add_shared_slot(slot_ref);

        // COW update
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicInstanceLayout> = (**guard).clone();
        new_map.insert(instance_id, layout);
        self.inner.store(Arc::new(new_map));

        Ok(())
    }

    /// Remove a shared slot reference from an instance (for routing deletion)
    pub fn remove_shared_slot(
        &self,
        instance_id: u32,
        point_type: PointType,
        point_id: u32,
    ) -> Result<Option<SharedSlotRef>, String> {
        let mut layout = self
            .get(instance_id)
            .ok_or_else(|| format!("Instance {} not found", instance_id))?;

        let removed = layout.remove_shared_slot(point_type, point_id);

        // COW update
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicInstanceLayout> = (**guard).clone();
        new_map.insert(instance_id, layout);
        self.inner.store(Arc::new(new_map));

        Ok(removed)
    }

    /// Clear all shared_slots for an instance (for bulk routing deletion)
    ///
    /// Returns the number of shared_slots cleared.
    pub fn clear_shared_slots(&self, instance_id: u32) -> Result<usize, String> {
        let mut layout = self
            .get(instance_id)
            .ok_or_else(|| format!("Instance {} not found", instance_id))?;

        let cleared_count = layout.shared_slots.len();
        layout.shared_slots.clear();
        layout.shared_lookup.clear();

        // COW update
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicInstanceLayout> = (**guard).clone();
        new_map.insert(instance_id, layout);
        self.inner.store(Arc::new(new_map));

        Ok(cleared_count)
    }

    /// Get a snapshot of all layouts (for debugging/API)
    pub fn snapshot(&self) -> FxHashMap<u32, DynamicInstanceLayout> {
        let guard = self.inner.load();
        (**guard).clone()
    }
}

impl Default for InstanceIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_instance_index_new() {
        let index = InstanceIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_instance_layout_own_slots() {
        let allocation = SlotAllocation {
            base_slot: 100,
            count: 8,
        };
        // Layout: M:5, A:3
        let layout = DynamicInstanceLayout::new(23, Some(allocation), [5, 3]);

        // M:1 → 100, M:5 → 104
        assert_eq!(layout.own_slot(PointType::Telemetry, 1), Some(100));
        assert_eq!(layout.own_slot(PointType::Telemetry, 5), Some(104));
        assert_eq!(layout.own_slot(PointType::Telemetry, 6), None);

        // A:1 → 105, A:3 → 107
        assert_eq!(layout.own_slot(PointType::Adjustment, 1), Some(105));
        assert_eq!(layout.own_slot(PointType::Adjustment, 3), Some(107));
        assert_eq!(layout.own_slot(PointType::Adjustment, 4), None);

        // T, S, C not valid for Instance own slots
        assert_eq!(layout.own_slot(PointType::Signal, 1), None);
        assert_eq!(layout.own_slot(PointType::Control, 1), None);
    }

    #[test]
    fn test_instance_layout_shared_slots() {
        let mut layout = DynamicInstanceLayout::new(23, None, [0, 0]);

        // Add shared slot: ch:1001:T:5 → inst:23:M:7
        layout.add_shared_slot(SharedSlotRef {
            slot_id: 42,
            point_type: PointType::Telemetry,
            point_id: 7,
            source_channel_id: 1001,
            source_point_type: PointType::Telemetry,
            source_point_id: 5,
        });

        // Lookup via slot() should find shared slot
        assert_eq!(layout.slot(PointType::Telemetry, 7), Some(42));
        assert_eq!(layout.shared_count(), 1);

        // Remove shared slot
        let removed = layout.remove_shared_slot(PointType::Telemetry, 7);
        assert!(removed.is_some());
        assert_eq!(layout.shared_count(), 0);
        assert_eq!(layout.slot(PointType::Telemetry, 7), None);
    }

    #[test]
    fn test_instance_index_add_remove() {
        let index = InstanceIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        // Add instance 23 with M:5, A:3
        let layout = index.add_instance(23, [5, 3], Some(&mut bitmap)).unwrap();
        assert_eq!(layout.own_base, 0);
        assert_eq!(layout.own_total, 8);

        // Add instance 24 with M:10, A:0
        let layout2 = index.add_instance(24, [10, 0], Some(&mut bitmap)).unwrap();
        assert_eq!(layout2.own_base, 8);
        assert_eq!(layout2.own_total, 10);

        assert_eq!(index.len(), 2);

        // Remove instance 23
        let removed = index.remove_instance(23, Some(&mut bitmap)).unwrap();
        assert_eq!(removed.own_base, 0);
        assert!(!index.contains(23));

        // Add instance 25 - should reuse freed slots
        let layout3 = index.add_instance(25, [4, 0], Some(&mut bitmap)).unwrap();
        assert_eq!(layout3.own_base, 0); // Reused
    }

    #[test]
    fn test_instance_index_no_own_points() {
        let index = InstanceIndex::new();

        // Instance with no own points (all shared)
        let layout = index.add_instance(100, [0, 0], None).unwrap();
        assert_eq!(layout.own_total, 0);
        assert!(layout.own_allocation.is_none());
    }

    #[test]
    fn test_instance_index_shared_slot_operations() {
        let index = InstanceIndex::new();

        // Add instance without own slots
        index.add_instance(50, [0, 0], None).unwrap();

        // Add shared slot via index
        index
            .add_shared_slot(
                50,
                SharedSlotRef {
                    slot_id: 200,
                    point_type: PointType::Telemetry,
                    point_id: 1,
                    source_channel_id: 1001,
                    source_point_type: PointType::Telemetry,
                    source_point_id: 1,
                },
            )
            .unwrap();

        // Verify
        let layout = index.get(50).unwrap();
        assert_eq!(layout.shared_count(), 1);
        assert_eq!(layout.slot(PointType::Telemetry, 1), Some(200));

        // Remove shared slot
        let removed = index
            .remove_shared_slot(50, PointType::Telemetry, 1)
            .unwrap();
        assert!(removed.is_some());

        // Verify removal
        let layout = index.get(50).unwrap();
        assert_eq!(layout.shared_count(), 0);
    }

    #[test]
    fn test_instance_index_slot_lookup() {
        let index = InstanceIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        // Add instance with own slots
        index.add_instance(30, [3, 2], Some(&mut bitmap)).unwrap();

        // Add a shared slot
        index
            .add_shared_slot(
                30,
                SharedSlotRef {
                    slot_id: 500,
                    point_type: PointType::Telemetry,
                    point_id: 10, // Higher than own count
                    source_channel_id: 2001,
                    source_point_type: PointType::Telemetry,
                    source_point_id: 1,
                },
            )
            .unwrap();

        // Own slot lookup
        assert_eq!(index.slot(30, PointType::Telemetry, 1), Some(0)); // own
        assert_eq!(index.slot(30, PointType::Adjustment, 1), Some(3)); // own

        // Shared slot lookup
        assert_eq!(index.slot(30, PointType::Telemetry, 10), Some(500)); // shared

        // Non-existent
        assert_eq!(index.slot(30, PointType::Telemetry, 100), None);
        assert_eq!(index.slot(9999, PointType::Telemetry, 1), None);
    }
}
