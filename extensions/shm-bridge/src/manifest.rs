//! Deterministic channel-to-slot manifest used by legacy service adapters.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use aether_domain::{ChannelId, PointId, PointKind};

/// Point kinds in their stable physical SHM allocation order.
pub const CHANNEL_POINT_KINDS: [PointKind; 4] = [
    PointKind::Telemetry,
    PointKind::Status,
    PointKind::Command,
    PointKind::Action,
];

/// Strongly typed address of one physical channel point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalPointAddress {
    channel_id: ChannelId,
    kind: PointKind,
    point_id: PointId,
}

impl PhysicalPointAddress {
    /// Creates a physical channel-point address.
    #[must_use]
    pub const fn new(channel_id: ChannelId, kind: PointKind, point_id: PointId) -> Self {
        Self {
            channel_id,
            kind,
            point_id,
        }
    }

    /// Creates an address from the legacy database/wire representation.
    ///
    /// New domain code should use [`Self::new`]. This named conversion remains
    /// only while existing service interfaces still expose raw numeric IDs.
    #[must_use]
    pub const fn from_legacy_raw(channel_id: u32, kind: PointKind, point_id: u32) -> Self {
        Self::new(ChannelId::new(channel_id), kind, PointId::new(point_id))
    }

    /// Returns the owning physical channel identifier.
    #[must_use]
    pub const fn channel_id(self) -> ChannelId {
        self.channel_id
    }

    /// Returns the point kind without exposing numeric wire codes.
    #[must_use]
    pub const fn kind(self) -> PointKind {
        self.kind
    }

    /// Returns the point identifier within the channel and kind.
    #[must_use]
    pub const fn point_id(self) -> PointId {
        self.point_id
    }
}

/// Deterministic slot layout for one physical channel.
#[derive(Debug, Clone, Default)]
pub struct ChannelPointLayout {
    base_slot: usize,
    type_offsets: [usize; 4],
    type_counts: [u32; 4],
}

impl ChannelPointLayout {
    /// Returns the first slot reserved for this channel.
    #[must_use]
    pub const fn base_slot(&self) -> usize {
        self.base_slot
    }

    /// Returns the four point counts in stable T/S/C/A order.
    #[must_use]
    pub const fn counts(&self) -> [u32; 4] {
        self.type_counts
    }

    /// Returns one kind's offset from this channel's base slot.
    #[must_use]
    pub const fn type_offset(&self, kind: PointKind) -> usize {
        self.type_offsets[kind_index(kind)]
    }

    /// Returns the number of points of one typed kind.
    #[must_use]
    pub const fn point_count(&self, kind: PointKind) -> u32 {
        self.type_counts[kind_index(kind)]
    }

    /// Returns the total number of physical points, excluding padding slots.
    #[must_use]
    pub fn total_points(&self) -> u32 {
        self.type_counts.iter().copied().sum()
    }

    /// Resolves a typed point within this channel to a physical slot.
    #[must_use]
    pub fn slot(&self, kind: PointKind, point_id: u32) -> Option<usize> {
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
    layouts: BTreeMap<u32, ChannelPointLayout>,
    physical_points: Vec<Option<PhysicalPointAddress>>,
    point_count: usize,
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
                ChannelPointLayout {
                    base_slot,
                    type_offsets,
                    type_counts: *channel_counts,
                },
            );
        }

        let mut physical_points = vec![None; next_slot];
        let mut point_count = 0_usize;
        for (&channel_id, layout) in &layouts {
            for kind in CHANNEL_POINT_KINDS {
                for point_id in 0..layout.point_count(kind) {
                    if let Some(slot) = layout.slot(kind, point_id) {
                        physical_points[slot] = Some(PhysicalPointAddress::new(
                            ChannelId::new(channel_id),
                            kind,
                            PointId::new(point_id),
                        ));
                        point_count += 1;
                    }
                }
            }
        }

        Self {
            counts,
            layouts,
            physical_points,
            point_count,
            slot_count: next_slot,
        }
    }

    /// Resolves a strongly typed physical point address to its SHM slot.
    #[must_use]
    pub fn slot_for(&self, address: PhysicalPointAddress) -> Option<usize> {
        self.layouts
            .get(&address.channel_id().get())?
            .slot(address.kind(), address.point_id().get())
    }

    /// Resolves a legacy raw channel point to its physical SHM slot.
    ///
    /// This compatibility shim can be removed once HTTP/CLI adapters convert
    /// their numeric IDs into domain IDs before invoking the manifest.
    #[must_use]
    pub fn slot(&self, channel_id: u32, kind: PointKind, point_id: u32) -> Option<usize> {
        self.slot_for(PhysicalPointAddress::from_legacy_raw(
            channel_id, kind, point_id,
        ))
    }

    /// Returns the typed physical point occupying a slot, or `None` for
    /// padding and out-of-range slots.
    #[must_use]
    pub fn physical_point_at(&self, slot: usize) -> Option<PhysicalPointAddress> {
        self.physical_points.get(slot).copied().flatten()
    }

    /// Iterates physical points in ascending slot order, skipping padding.
    pub fn iter_physical_points(&self) -> impl Iterator<Item = (usize, PhysicalPointAddress)> + '_ {
        self.physical_points
            .iter()
            .enumerate()
            .filter_map(|(slot, address)| address.map(|address| (slot, address)))
    }

    /// Returns one channel's deterministic typed layout.
    #[must_use]
    pub fn channel_layout(&self, channel_id: u32) -> Option<&ChannelPointLayout> {
        self.layouts.get(&channel_id)
    }

    /// Iterates channel layouts in ascending channel-id order.
    pub fn iter_channel_layouts(&self) -> impl Iterator<Item = (u32, &ChannelPointLayout)> + '_ {
        self.layouts
            .iter()
            .map(|(&channel_id, layout)| (channel_id, layout))
    }

    /// Returns the number of live slots, including cache-line padding.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Returns the number of physical points, excluding padding slots.
    #[must_use]
    pub const fn point_count(&self) -> usize {
        self.point_count
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
