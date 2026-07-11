//! Channel slot layout — **business-side**, not part of `core::`.
//!
//! `ChannelLayout` knows the 4 PointType semantics (T/S/C/A) and translates
//! `(channel_id, point_type, point_id)` triples into flat slot indices. This
//! is the adapter between SHM's slot-only world (`core::slot::PointSlot`,
//! addressed by usize) and the channel/point-type vocabulary that io
//! and automation speak.
//!
//! Long-term direction: when SHM core is promoted to a standalone crate,
//! this module stays in the business adapter. The header on-mmap already
//! captures the layout fingerprint as `routing_hash` (a manifest hash);
//! `ChannelLayout` itself never crosses the process boundary — it is
//! re-derived in each process from the same `ChannelPointCounts` source.

use crate::channel_points::ChannelPointCounts;

/// Channel layout — allocation info for one channel.
///
/// Stored in process memory as `Vec<ChannelLayout>`, indexed by `channel_id`.
/// Re-derived independently in io (writer) and automation (reader) from the
/// same `ChannelPointCounts` source; cross-process agreement is verified via
/// the header's `routing_hash` field, not by sharing this struct.
#[derive(Clone, Default, Debug)]
pub struct ChannelLayout {
    /// Base slot index for this channel
    pub base_slot: usize,
    /// Offset for each point type [T, S, C, A]
    pub type_offsets: [usize; 4],
    /// Point count for each type [T, S, C, A]
    pub type_counts: [u32; 4],
    /// Total points for this channel
    pub total_points: u32,
}

impl ChannelLayout {
    /// Calculate slot index for given type and point_id.
    #[inline]
    pub fn slot(&self, point_type: u8, point_id: u32) -> Option<usize> {
        let type_idx = point_type as usize;
        if type_idx >= 4 || point_id >= self.type_counts[type_idx] {
            return None;
        }
        Some(self.base_slot + self.type_offsets[type_idx] + point_id as usize)
    }

    /// Check if this layout is valid (has any points).
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.total_points > 0
    }
}

/// Round a slot index up to the next 64-byte cache-line boundary.
///
/// `PointSlot` is 32 B and the slot array starts 64-byte-aligned (the header
/// is exactly 64 B), so an even slot index begins a fresh cache line.
#[inline]
const fn align_to_cache_line(slot: usize) -> usize {
    (slot + 1) & !1
}

/// Allocate layouts from channel point counts.
///
/// Both Writer and Reader call this with the same `ChannelPointCounts` →
/// same slot allocation. Routing-independent: layout only depends on which
/// channels have which point counts.
///
/// # Writer-ownership cache-line padding
///
/// io writes T/S slots, automation writes C/A slots — from different cores.
/// Two 32-byte slots share one 64-byte cache line, so an unpadded boundary
/// between the owners makes the line ping-pong between cores (false sharing:
/// measured ×5.7 per-write penalty on the Cortex-A55 deployment target,
/// `benches/false_sharing.rs`). Padding rules:
///
/// - every channel's `base_slot` starts on a fresh cache line (covers the
///   previous channel's C/A tail → this channel's T/S head boundary)
/// - the C/A region starts on a fresh cache line when the channel has any
///   C/A points (covers the in-channel T/S → C/A boundary)
///
/// Cost: ≤1 padding slot (32 B) per boundary. Padding slots are never
/// referenced by any `ChannelLayout`, never written, never dirty.
/// Dev-machine caveat: Apple Silicon has 128-byte lines, so residual
/// sharing is possible there — the production targets (A55, x86) are 64 B.
pub fn allocate_layouts(channel_points: &ChannelPointCounts) -> (Vec<ChannelLayout>, usize) {
    // Find max channel_id to size the Vec
    let max_channel_id = channel_points.0.keys().copied().max().unwrap_or(0);
    let vec_size = (max_channel_id + 1) as usize;

    let mut layouts = vec![ChannelLayout::default(); vec_size];
    let mut next_slot = 0usize;

    // Allocate in channel_id order (BTreeMap is sorted)
    for (&channel_id, counts) in &channel_points.0 {
        let layout = &mut layouts[channel_id as usize];

        // Ownership boundary: previous channel's automation-owned tail must not
        // share a cache line with this channel's io-owned head.
        next_slot = align_to_cache_line(next_slot);
        layout.base_slot = next_slot;

        let has_action_slots = counts[2] + counts[3] > 0;

        // Allocate each type in T/S/C/A order
        for (type_idx, &count) in counts.iter().enumerate() {
            // Ownership boundary inside the channel: C/A (automation) must not
            // share a cache line with T/S (io). Skipped when the channel
            // has no C/A points so pure-telemetry channels stay dense.
            if type_idx == 2 && has_action_slots {
                next_slot = align_to_cache_line(next_slot);
            }
            layout.type_offsets[type_idx] = next_slot - layout.base_slot;
            layout.type_counts[type_idx] = count;
            next_slot += count as usize;
        }

        layout.total_points = counts.iter().sum();
    }

    (layouts, next_slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn counts(map: &[(u32, [u32; 4])]) -> ChannelPointCounts {
        ChannelPointCounts::from_map(BTreeMap::from_iter(map.iter().copied()))
    }

    #[test]
    fn ca_region_starts_on_fresh_cache_line() {
        // T=3 (io) then C/A (automation): without padding C would land on
        // slot 3, sharing a 64-byte line with T slot 2 → false sharing.
        let (layouts, _) = allocate_layouts(&counts(&[(1, [3, 0, 1, 1])]));
        let c_slot = layouts[1].slot(2, 0).unwrap();
        assert_eq!(c_slot % 2, 0, "C region must start on an even slot index");
        // A follows C contiguously (same owner, no boundary between them).
        assert_eq!(layouts[1].slot(3, 0).unwrap(), c_slot + 1);
    }

    #[test]
    fn channel_base_starts_on_fresh_cache_line() {
        // ch1 ends with automation-owned C; ch2 starts with io-owned T.
        // ch2's base must not share a cache line with ch1's tail.
        let (layouts, _) = allocate_layouts(&counts(&[(1, [1, 0, 1, 0]), (2, [1, 0, 0, 0])]));
        assert_eq!(
            layouts[2].base_slot % 2,
            0,
            "channel base must start on an even slot index"
        );
    }

    #[test]
    fn no_padding_when_no_automation_slots() {
        // A channel with only T/S has no internal ownership boundary —
        // no padding slots should be wasted before the empty C/A region.
        let (layouts, total) = allocate_layouts(&counts(&[(1, [3, 1, 0, 0])]));
        assert_eq!(total, 4, "pure-T/S channel must stay densely packed");
        assert_eq!(layouts[1].slot(1, 0).unwrap(), 3);
    }
}
