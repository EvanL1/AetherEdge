//! Deterministic channel-to-slot manifest used by legacy service adapters.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use aether_domain::PointKind;

#[derive(Debug, Clone, Default)]
struct ChannelLayout {
    base_slot: usize,
    type_offsets: [usize; 4],
    type_counts: [u32; 4],
}

impl ChannelLayout {
    fn slot(&self, kind: PointKind, point_id: u32) -> Option<usize> {
        let type_index = kind_index(kind);
        if point_id >= self.type_counts[type_index] {
            return None;
        }
        Some(self.base_slot + self.type_offsets[type_index] + point_id as usize)
    }
}

/// Immutable manifest that reproduces the writer's deterministic T/S/C/A
/// allocation without importing routing, SQL, or the legacy RTDB crate.
///
/// Counts are ordered as telemetry, status, command, action. Each count is the
/// highest point id plus one, matching the physical writer contract.
#[derive(Debug, Clone, Default)]
pub struct ChannelPointManifest {
    counts: BTreeMap<u32, [u32; 4]>,
    layouts: BTreeMap<u32, ChannelLayout>,
    slot_count: usize,
}

impl ChannelPointManifest {
    /// Compiles a deterministic manifest from channel/count entries.
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = (u32, [u32; 4])>) -> Self {
        Self::from_map(entries.into_iter().collect())
    }

    /// Compiles a deterministic manifest from an ordered count map.
    #[must_use]
    pub fn from_map(counts: BTreeMap<u32, [u32; 4]>) -> Self {
        let mut layouts = BTreeMap::new();
        let mut next_slot = 0_usize;

        for (&channel_id, channel_counts) in &counts {
            next_slot = align_to_cache_line(next_slot);
            let base_slot = next_slot;
            let mut type_offsets = [0_usize; 4];
            let has_action_slots = channel_counts[2].saturating_add(channel_counts[3]) > 0;

            for (type_index, &count) in channel_counts.iter().enumerate() {
                if type_index == 2 && has_action_slots {
                    next_slot = align_to_cache_line(next_slot);
                }
                type_offsets[type_index] = next_slot - base_slot;
                next_slot = next_slot.saturating_add(count as usize);
            }

            layouts.insert(
                channel_id,
                ChannelLayout {
                    base_slot,
                    type_offsets,
                    type_counts: *channel_counts,
                },
            );
        }

        Self {
            counts,
            layouts,
            slot_count: next_slot,
        }
    }

    /// Resolves a channel point to its physical SHM slot.
    #[must_use]
    pub fn slot(&self, channel_id: u32, kind: PointKind, point_id: u32) -> Option<usize> {
        self.layouts.get(&channel_id)?.slot(kind, point_id)
    }

    /// Returns the number of live slots, including cache-line padding.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Computes the exact layout fingerprint written into the SHM header.
    #[must_use]
    pub fn layout_hash(&self) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        for (channel_id, counts) in &self.counts {
            channel_id.hash(&mut hasher);
            counts.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Returns the ordered point counts used to compile this manifest.
    #[must_use]
    pub const fn counts(&self) -> &BTreeMap<u32, [u32; 4]> {
        &self.counts
    }
}

const fn kind_index(kind: PointKind) -> usize {
    match kind {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    }
}

const fn align_to_cache_line(slot: usize) -> usize {
    (slot + 1) & !1
}
