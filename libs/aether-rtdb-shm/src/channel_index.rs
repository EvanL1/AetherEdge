//! Dynamic Channel Index for Unified Pool Architecture
//!
//! Supports hot add/remove of channels with ArcSwap for lock-free reads.
//!
//! # Design
//!
//! ```text
//! ChannelIndex (ArcSwap)
//! ┌────────────────────────────────────────────┐
//! │ 1001 → DynamicChannelLayout { base: 42, ... }     │
//! │ 1002 → DynamicChannelLayout { base: 57, ... }     │
//! │ ...                                         │
//! └────────────────────────────────────────────┘
//!           ↑ Atomic swap on add/remove
//! ```
//!
//! # Hot Operations
//!
//! - **Add Channel**: SlotBitmap.alloc → ChannelIndex.insert → ArcSwap.store
//! - **Remove Channel**: Check dependencies → ChannelIndex.remove → SlotBitmap.free

use aether_model::PointType;
use arc_swap::ArcSwap;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use crate::core::bitmap::{SlotAllocation, SlotBitmap};
use aether_routing::routing_cache::RoutingCache;

/// Channel layout - allocation info for one channel
///
/// Stored in ChannelIndex as HashMap values.
/// Compatible with v3 DynamicChannelLayout but with additional metadata.
#[derive(Clone, Debug)]
pub struct DynamicChannelLayout {
    /// Base slot index for this channel
    pub base_slot: usize,
    /// Point count for each type [T, S, C, A]
    pub type_counts: [u32; 4],
    /// Total points for this channel
    pub total_points: u32,
    /// Allocation info (for freeing)
    pub allocation: SlotAllocation,
}

impl DynamicChannelLayout {
    /// Create a new channel layout from allocation
    pub fn new(allocation: SlotAllocation, type_counts: [u32; 4]) -> Self {
        let total_points: u32 = type_counts.iter().sum();
        Self {
            base_slot: allocation.base_slot,
            type_counts,
            total_points,
            allocation,
        }
    }

    /// Get slot index for a specific point
    ///
    /// Layout: [T...][S...][C...][A...]
    #[inline]
    pub fn slot(&self, point_type: PointType, point_id: u32) -> Option<usize> {
        let type_idx = point_type.to_u8() as usize;
        if point_id == 0 || point_id > self.type_counts[type_idx] {
            return None;
        }

        // Calculate offset: sum of all previous types
        let offset: u32 = self.type_counts[..type_idx].iter().sum();
        Some(self.base_slot + offset as usize + (point_id - 1) as usize)
    }

    /// Check if layout is valid (has any points)
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.total_points > 0
    }

    /// Get count for a specific point type
    #[inline]
    pub fn count(&self, point_type: PointType) -> u32 {
        self.type_counts[point_type.to_u8() as usize]
    }
}

/// Thread-safe channel index with hot update support
///
/// Uses ArcSwap for lock-free reads and COW updates.
pub struct ChannelIndex {
    /// Channel layouts indexed by channel_id
    inner: ArcSwap<FxHashMap<u32, DynamicChannelLayout>>,
}

impl ChannelIndex {
    /// Create empty channel index
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(FxHashMap::default()),
        }
    }

    /// Create channel index from routing cache (for migration from v3)
    ///
    /// Allocates slots using the provided bitmap.
    pub fn from_routing_cache(
        routing_cache: &RoutingCache,
        bitmap: &mut SlotBitmap,
    ) -> (Self, usize) {
        let channel_points = collect_channel_points(routing_cache);
        let mut layouts = FxHashMap::default();
        let mut total_slots = 0usize;

        for (channel_id, counts) in channel_points {
            let total: u32 = counts.iter().sum();
            if total == 0 {
                continue;
            }

            // Allocate contiguous slots
            if let Some(allocation) = bitmap.alloc_contiguous(total as usize) {
                total_slots += allocation.count;
                layouts.insert(channel_id, DynamicChannelLayout::new(allocation, counts));
            } else {
                // Allocation failed - log warning but continue
                tracing::warn!(
                    channel_id,
                    required = total,
                    "Failed to allocate slots for channel"
                );
            }
        }

        (
            Self {
                inner: ArcSwap::from_pointee(layouts),
            },
            total_slots,
        )
    }

    /// Get channel layout (lock-free read)
    #[inline]
    pub fn get(&self, channel_id: u32) -> Option<DynamicChannelLayout> {
        let guard = self.inner.load();
        guard.get(&channel_id).cloned()
    }

    /// Get slot index for a channel point (lock-free)
    #[inline]
    pub fn slot(&self, channel_id: u32, point_type: PointType, point_id: u32) -> Option<usize> {
        let guard = self.inner.load();
        guard
            .get(&channel_id)
            .and_then(|layout| layout.slot(point_type, point_id))
    }

    /// Check if channel exists
    #[inline]
    pub fn contains(&self, channel_id: u32) -> bool {
        let guard = self.inner.load();
        guard.contains_key(&channel_id)
    }

    /// Get all channel IDs
    pub fn channel_ids(&self) -> Vec<u32> {
        let guard = self.inner.load();
        guard.keys().copied().collect()
    }

    /// Get channel count
    pub fn len(&self) -> usize {
        let guard = self.inner.load();
        guard.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ========== Hot Update Operations ==========

    /// Add a new channel (hot operation)
    ///
    /// # Arguments
    /// - `channel_id`: Channel ID to add
    /// - `type_counts`: Point counts for [T, S, C, A]
    /// - `bitmap`: SlotBitmap for allocation
    ///
    /// # Returns
    /// - `Ok(DynamicChannelLayout)`: Successfully added channel
    /// - `Err(String)`: Allocation failed or channel already exists
    pub fn add_channel(
        &self,
        channel_id: u32,
        type_counts: [u32; 4],
        bitmap: &mut SlotBitmap,
    ) -> Result<DynamicChannelLayout, String> {
        let total: u32 = type_counts.iter().sum();
        if total == 0 {
            return Err("Cannot add channel with zero points".into());
        }

        // Check if channel already exists
        if self.contains(channel_id) {
            return Err(format!("Channel {} already exists", channel_id));
        }

        // Allocate slots
        let allocation = bitmap.alloc_contiguous(total as usize).ok_or_else(|| {
            format!(
                "Failed to allocate {} slots for channel {}",
                total, channel_id
            )
        })?;

        let layout = DynamicChannelLayout::new(allocation, type_counts);

        // COW update: clone inner HashMap → insert → wrap in Arc → swap
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicChannelLayout> = (**guard).clone();
        new_map.insert(channel_id, layout.clone());
        self.inner.store(Arc::new(new_map));

        Ok(layout)
    }

    /// Remove a channel (hot operation)
    ///
    /// # Arguments
    /// - `channel_id`: Channel ID to remove
    /// - `bitmap`: SlotBitmap for freeing slots
    ///
    /// # Returns
    /// - `Ok(DynamicChannelLayout)`: Removed channel layout
    /// - `Err(String)`: Channel not found
    pub fn remove_channel(
        &self,
        channel_id: u32,
        bitmap: &mut SlotBitmap,
    ) -> Result<DynamicChannelLayout, String> {
        // Get layout before removal
        let layout = self
            .get(channel_id)
            .ok_or_else(|| format!("Channel {} not found", channel_id))?;

        // COW update: clone inner HashMap → remove → wrap in Arc → swap
        let guard = self.inner.load();
        let mut new_map: FxHashMap<u32, DynamicChannelLayout> = (**guard).clone();
        new_map.remove(&channel_id);
        self.inner.store(Arc::new(new_map));

        // Free slots after index update (so readers see consistent state)
        bitmap.free(layout.allocation.base_slot, layout.allocation.count);

        Ok(layout)
    }

    /// Update channel point counts (requires reallocation)
    ///
    /// This is a remove + add operation internally.
    pub fn update_channel(
        &self,
        channel_id: u32,
        new_type_counts: [u32; 4],
        bitmap: &mut SlotBitmap,
    ) -> Result<DynamicChannelLayout, String> {
        // Remove old (if exists)
        if self.contains(channel_id) {
            let _ = self.remove_channel(channel_id, bitmap)?;
        }

        // Add new
        self.add_channel(channel_id, new_type_counts, bitmap)
    }

    /// Get a snapshot of all layouts (for debugging/API)
    pub fn snapshot(&self) -> FxHashMap<u32, DynamicChannelLayout> {
        let guard = self.inner.load();
        (**guard).clone()
    }
}

impl Default for ChannelIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ========== Helper Functions ==========

/// Collect channel points from routing cache
///
/// Returns: BTreeMap<channel_id, [T_count, S_count, C_count, A_count]>
fn collect_channel_points(
    routing_cache: &RoutingCache,
) -> std::collections::BTreeMap<u32, [u32; 4]> {
    let mut channel_points: std::collections::BTreeMap<u32, [u32; 4]> =
        std::collections::BTreeMap::new();

    // Collect from C2M routes (these are the main channel→instance mappings)
    for (key, _target) in routing_cache.c2m_iter() {
        let (channel_id, point_type, point_id) = key;
        let counts = channel_points.entry(channel_id).or_insert([0, 0, 0, 0]);
        let type_idx = point_type.to_u8() as usize;
        // Track max point_id for each type
        counts[type_idx] = counts[type_idx].max(point_id);
    }

    // Note: c2c_iter not available in current RoutingCache API
    // C2C routes are not commonly used, skipping for now

    // Collect from M2C routes (target channels - used for downlink commands)
    for (_key, target) in routing_cache.m2c_iter() {
        let counts = channel_points
            .entry(target.channel_id)
            .or_insert([0, 0, 0, 0]);
        let type_idx = target.point_type.to_u8() as usize;
        counts[type_idx] = counts[type_idx].max(target.point_id);
    }

    channel_points
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_channel_index_new() {
        let index = ChannelIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_channel_layout_slot() {
        let allocation = SlotAllocation {
            base_slot: 100,
            count: 15,
        };
        // Layout: T:5, S:4, C:3, A:3
        let layout = DynamicChannelLayout::new(allocation, [5, 4, 3, 3]);

        // T:1 → 100, T:5 → 104
        assert_eq!(layout.slot(PointType::Telemetry, 1), Some(100));
        assert_eq!(layout.slot(PointType::Telemetry, 5), Some(104));
        assert_eq!(layout.slot(PointType::Telemetry, 6), None); // Out of range

        // S:1 → 105, S:4 → 108
        assert_eq!(layout.slot(PointType::Signal, 1), Some(105));
        assert_eq!(layout.slot(PointType::Signal, 4), Some(108));

        // C:1 → 109, C:3 → 111
        assert_eq!(layout.slot(PointType::Control, 1), Some(109));
        assert_eq!(layout.slot(PointType::Control, 3), Some(111));

        // A:1 → 112, A:3 → 114
        assert_eq!(layout.slot(PointType::Adjustment, 1), Some(112));
        assert_eq!(layout.slot(PointType::Adjustment, 3), Some(114));
    }

    #[test]
    fn test_channel_index_add_remove() {
        let index = ChannelIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        // Add channel 1001 with 10 points
        let layout = index.add_channel(1001, [5, 2, 2, 1], &mut bitmap).unwrap();
        assert_eq!(layout.base_slot, 0);
        assert_eq!(layout.total_points, 10);

        // Add channel 1002 with 5 points
        let layout2 = index.add_channel(1002, [3, 1, 1, 0], &mut bitmap).unwrap();
        assert_eq!(layout2.base_slot, 10); // After channel 1001
        assert_eq!(layout2.total_points, 5);

        // Verify index state
        assert_eq!(index.len(), 2);
        assert!(index.contains(1001));
        assert!(index.contains(1002));

        // Remove channel 1001
        let removed = index.remove_channel(1001, &mut bitmap).unwrap();
        assert_eq!(removed.base_slot, 0);
        assert!(!index.contains(1001));
        assert_eq!(index.len(), 1);

        // Add channel 1003 - should reuse freed slots
        let layout3 = index.add_channel(1003, [5, 0, 0, 0], &mut bitmap).unwrap();
        assert_eq!(layout3.base_slot, 0); // Reused slots 0-4
    }

    #[test]
    fn test_channel_index_add_duplicate() {
        let index = ChannelIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        index.add_channel(1001, [5, 0, 0, 0], &mut bitmap).unwrap();

        // Try to add duplicate - should fail
        let result = index.add_channel(1001, [3, 0, 0, 0], &mut bitmap);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn test_channel_index_remove_nonexistent() {
        let index = ChannelIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        // Try to remove non-existent channel
        let result = index.remove_channel(9999, &mut bitmap);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_channel_index_update() {
        let index = ChannelIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        // Add channel
        index.add_channel(1001, [5, 0, 0, 0], &mut bitmap).unwrap();

        // Update to larger
        let updated = index
            .update_channel(1001, [10, 5, 0, 0], &mut bitmap)
            .unwrap();
        assert_eq!(updated.total_points, 15);

        // Verify
        let layout = index.get(1001).unwrap();
        assert_eq!(layout.total_points, 15);
    }

    #[test]
    fn test_channel_index_slot_lookup() {
        let index = ChannelIndex::new();
        let mut bitmap = SlotBitmap::new(1000);

        index.add_channel(1001, [5, 4, 3, 2], &mut bitmap).unwrap();

        // Direct slot lookup
        assert_eq!(index.slot(1001, PointType::Telemetry, 1), Some(0));
        assert_eq!(index.slot(1001, PointType::Signal, 1), Some(5));
        assert_eq!(index.slot(1001, PointType::Control, 1), Some(9));
        assert_eq!(index.slot(1001, PointType::Adjustment, 1), Some(12));

        // Non-existent channel
        assert_eq!(index.slot(9999, PointType::Telemetry, 1), None);
    }
}
