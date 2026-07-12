//! Legacy reverse-index projection: slot → (channel_id, point_type, point_id)
//!
//! Production entries come directly from `ChannelPointManifest`'s canonical
//! slot-order iterator. The stored legacy shape remains only for diagnostics,
//! snapshots, and rule-engine callers that have not migrated to strong IDs.

use crate::shared_config::{ChannelToSlotIndex, legacy_point_type};
use aether_model::PointType;

/// Origin info for one SHM slot
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotOrigin {
    pub channel_id: u32,
    pub point_type: PointType,
    pub point_id: u32,
}

/// Reverse mapping: slot index → [`SlotOrigin`]
///
/// Constructed once from [`ChannelToSlotIndex`]; thereafter read-only.
/// Removal criterion: delete this projection after all consumers use
/// `ChannelPointManifest::physical_point_at` and typed physical addresses.
pub struct ReverseSlotIndex {
    origins: Vec<Option<SlotOrigin>>,
    mapped_count: usize,
}

impl ReverseSlotIndex {
    /// Build the reverse index from a forward [`ChannelToSlotIndex`].
    ///
    /// Any slot beyond `slot_count` is silently ignored.
    pub fn from_forward(forward: &ChannelToSlotIndex, slot_count: usize) -> Self {
        let mut origins: Vec<Option<SlotOrigin>> = vec![None; slot_count];
        let mut mapped_count = 0usize;

        if let Some(manifest) = forward.formal_manifest() {
            for (slot, address) in manifest.iter_physical_points() {
                if slot < slot_count {
                    origins[slot] = Some(SlotOrigin {
                        channel_id: address.channel_id().get(),
                        point_type: legacy_point_type(address.kind()),
                        point_id: address.point_id().get(),
                    });
                    mapped_count += 1;
                }
            }
        } else {
            // Compatibility path for arbitrary mappings inserted by legacy
            // test helpers. Production indexes always project the manifest.
            for ((channel_id, point_type, point_id), slot) in forward.iter() {
                if *slot < slot_count {
                    origins[*slot] = Some(SlotOrigin {
                        channel_id: *channel_id,
                        point_type: *point_type,
                        point_id: *point_id,
                    });
                    mapped_count += 1;
                }
            }
        }

        Self {
            origins,
            mapped_count,
        }
    }

    /// Look up the origin for a slot index.
    #[inline]
    pub fn get(&self, slot: usize) -> Option<&SlotOrigin> {
        self.origins.get(slot)?.as_ref()
    }

    /// Total number of slots tracked (including unmapped ones).
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.origins.len()
    }

    /// Number of slots that have a known origin.
    #[inline]
    pub fn mapped_count(&self) -> usize {
        self.mapped_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_config::ChannelToSlotIndex;
    use aether_shm_bridge::ChannelPointManifest;

    fn make_forward(entries: &[(u32, PointType, u32, usize)]) -> ChannelToSlotIndex {
        let mut idx = ChannelToSlotIndex::new_empty();
        for &(ch, pt, pid, slot) in entries {
            idx.insert(ch, pt, pid, slot);
        }
        idx
    }

    #[test]
    fn round_trips_origin() {
        let forward = make_forward(&[
            (1, PointType::Telemetry, 0, 0),
            (1, PointType::Signal, 0, 1),
            (2, PointType::Control, 3, 5),
        ]);
        let rev = ReverseSlotIndex::from_forward(&forward, 8);

        assert_eq!(rev.slot_count(), 8);
        assert_eq!(rev.mapped_count(), 3);

        let o0 = rev.get(0).unwrap();
        assert_eq!(o0.channel_id, 1);
        assert_eq!(o0.point_type, PointType::Telemetry);
        assert_eq!(o0.point_id, 0);

        let o5 = rev.get(5).unwrap();
        assert_eq!(o5.channel_id, 2);
        assert_eq!(o5.point_type, PointType::Control);
        assert_eq!(o5.point_id, 3);

        assert!(rev.get(2).is_none());
        assert!(rev.get(99).is_none());
    }

    #[test]
    fn production_projection_uses_manifest_slot_order_and_padding() {
        let manifest = ChannelPointManifest::from_entries([(7, [1, 1, 0, 1]), (1, [1, 0, 1, 0])]);
        let slot_count = manifest.slot_count();
        let forward = ChannelToSlotIndex::from_manifest(manifest);
        let reverse = ReverseSlotIndex::from_forward(&forward, slot_count);

        assert_eq!(forward.lookup(1, PointType::Telemetry, 0), Some(0));
        assert_eq!(forward.lookup(1, PointType::Control, 0), Some(2));
        assert_eq!(forward.lookup(7, PointType::Telemetry, 0), Some(4));
        assert_eq!(forward.lookup(7, PointType::Signal, 0), Some(5));
        assert_eq!(forward.lookup(7, PointType::Adjustment, 0), Some(6));
        assert_eq!(forward.len(), 5);

        assert!(reverse.get(1).is_none());
        assert!(reverse.get(3).is_none());
        assert_eq!(reverse.mapped_count(), 5);
        assert_eq!(
            reverse.get(6),
            Some(&SlotOrigin {
                channel_id: 7,
                point_type: PointType::Adjustment,
                point_id: 0,
            })
        );
    }
}
