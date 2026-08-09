//! Exact deterministic channel-point to SHM-slot manifest.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use aether_domain::{ChannelCommandAddress, ChannelId, ChannelPointAddress, PointId, PointKind};
use aether_ports::{PortError, PortErrorKind, PortResult};

const POINT_MANIFEST_DOMAIN: &str = "aether.live-state.layout.v5.ownership-aligned";
const POINT_SLOTS_PER_CACHE_LINE: usize = 2;

const CHANNEL_POINT_KINDS: [PointKind; 4] = [
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

    /// Creates an address at a database or wire boundary that carries numeric
    /// identifiers.
    #[must_use]
    pub const fn from_raw_ids(channel_id: u32, kind: PointKind, point_id: u32) -> Self {
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

impl From<ChannelPointAddress> for PhysicalPointAddress {
    fn from(address: ChannelPointAddress) -> Self {
        Self::new(address.channel_id(), address.kind(), address.point_id())
    }
}

impl From<ChannelCommandAddress> for PhysicalPointAddress {
    fn from(address: ChannelCommandAddress) -> Self {
        Self::new(address.channel_id(), address.kind(), address.point_id())
    }
}

/// Immutable exact physical topology.
///
/// Addresses follow ascending channel id, T/S/C/A kind, then point id. Only
/// configured addresses receive slots; sparse point identifiers never create
/// implicit points. The sole padding rule aligns a channel's first C/A slot to
/// a 64-byte cache line when that same channel also has T/S points, so its two
/// writer authorities never contend on one cache line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPointManifest {
    points_by_address: Vec<PhysicalPointAddress>,
    slots_by_address: Vec<usize>,
    slot_count: usize,
    layout_hash: u64,
}

impl ChannelPointManifest {
    /// Compiles a canonical manifest under an explicit physical-slot capacity.
    ///
    /// The capacity is checked before each address is retained. Duplicate
    /// physical addresses are invalid instead of being silently collapsed.
    pub fn compile(
        addresses: impl IntoIterator<Item = PhysicalPointAddress>,
        max_slots: usize,
    ) -> PortResult<Self> {
        let mut canonical = BTreeMap::new();
        for address in addresses {
            if canonical.len() >= max_slots {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("point manifest exceeds configured capacity {max_slots}"),
                ));
            }
            let key = address_key(address);
            if canonical.insert(key, address).is_some() {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!(
                        "duplicate physical point {}:{}:{}",
                        address.channel_id().get(),
                        kind_code(address.kind()),
                        address.point_id().get()
                    ),
                ));
            }
        }

        let points_by_address = canonical.into_values().collect::<Vec<_>>();
        let (slots_by_address, slot_count) = allocate_slots(&points_by_address);
        if slot_count > max_slots {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("point manifest exceeds configured capacity {max_slots}"),
            ));
        }
        let layout_hash = calculate_layout_hash(&points_by_address, &slots_by_address, slot_count);
        Ok(Self {
            points_by_address,
            slots_by_address,
            slot_count,
            layout_hash,
        })
    }

    /// Builds a dense-id manifest for test fixtures.
    ///
    /// Runtime topology must call [`Self::compile`] with exact configured
    /// addresses. This helper exists only to keep synthetic fixtures concise.
    #[doc(hidden)]
    #[must_use]
    pub fn dense_test_fixture(entries: impl IntoIterator<Item = (u32, [u32; 4])>) -> Self {
        let mut addresses = Vec::new();
        for (channel_id, counts) in entries {
            for (kind, count) in CHANNEL_POINT_KINDS.into_iter().zip(counts) {
                addresses.extend((0..count).map(|point_id| {
                    PhysicalPointAddress::from_raw_ids(channel_id, kind, point_id)
                }));
            }
        }
        addresses.sort_unstable_by_key(|address| address_key(*address));
        addresses.dedup();
        let (slots_by_address, slot_count) = allocate_slots(&addresses);
        let layout_hash = calculate_layout_hash(&addresses, &slots_by_address, slot_count);
        Self {
            points_by_address: addresses,
            slots_by_address,
            slot_count,
            layout_hash,
        }
    }

    /// Resolves a strongly typed physical point address to its SHM slot.
    #[must_use]
    pub fn slot_for(&self, address: PhysicalPointAddress) -> Option<usize> {
        self.points_by_address
            .binary_search_by_key(&address_key(address), |candidate| address_key(*candidate))
            .ok()
            .map(|index| self.slots_by_address[index])
    }

    /// Returns the configured physical point occupying a slot.
    #[must_use]
    pub fn physical_point_at(&self, slot: usize) -> Option<PhysicalPointAddress> {
        self.slots_by_address
            .binary_search(&slot)
            .ok()
            .map(|index| self.points_by_address[index])
    }

    /// Iterates physical points in ascending slot order.
    pub fn iter_physical_points(&self) -> impl Iterator<Item = (usize, PhysicalPointAddress)> + '_ {
        self.slots_by_address
            .iter()
            .copied()
            .zip(self.points_by_address.iter().copied())
    }

    /// Iterates configured channel ids in ascending order.
    pub fn channel_ids(&self) -> impl Iterator<Item = u32> + '_ {
        let mut previous = None;
        self.points_by_address.iter().filter_map(move |address| {
            let channel_id = address.channel_id().get();
            if previous == Some(channel_id) {
                None
            } else {
                previous = Some(channel_id);
                Some(channel_id)
            }
        })
    }

    /// Iterates exact configured point ids for one channel and kind.
    pub fn point_ids(&self, channel_id: u32, kind: PointKind) -> impl Iterator<Item = u32> + '_ {
        let kind = kind_index(kind);
        let start = self
            .points_by_address
            .partition_point(|address| address_key(*address) < (channel_id, kind, 0));
        let end = self
            .points_by_address
            .partition_point(|address| address_key(*address) <= (channel_id, kind, u32::MAX));
        self.points_by_address[start..end]
            .iter()
            .map(|address| address.point_id().get())
    }

    /// Returns the physical slot count, including ownership-boundary padding.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Returns the number of configured physical points.
    #[must_use]
    pub const fn point_count(&self) -> usize {
        self.points_by_address.len()
    }

    /// Returns the exact layout fingerprint written into the SHM header.
    #[must_use]
    pub const fn layout_hash(&self) -> u64 {
        self.layout_hash
    }
}

impl Default for ChannelPointManifest {
    fn default() -> Self {
        Self {
            points_by_address: Vec::new(),
            slots_by_address: Vec::new(),
            slot_count: 0,
            layout_hash: calculate_layout_hash(&[], &[], 0),
        }
    }
}

fn allocate_slots(points_by_address: &[PhysicalPointAddress]) -> (Vec<usize>, usize) {
    let mut slots = Vec::with_capacity(points_by_address.len());
    let mut next_slot = 0_usize;
    let mut previous_channel = None;
    let mut channel_has_acquisition = false;
    let mut command_boundary_aligned = false;

    for address in points_by_address {
        let channel_id = address.channel_id().get();
        if previous_channel != Some(channel_id) {
            previous_channel = Some(channel_id);
            channel_has_acquisition = false;
            command_boundary_aligned = false;
        }
        if address.kind().is_acquisition_owned() {
            channel_has_acquisition = true;
        } else if channel_has_acquisition && !command_boundary_aligned {
            if !next_slot.is_multiple_of(POINT_SLOTS_PER_CACHE_LINE) {
                next_slot = next_slot.saturating_add(1);
            }
            command_boundary_aligned = true;
        }
        slots.push(next_slot);
        next_slot = next_slot.saturating_add(1);
    }
    (slots, next_slot)
}

fn calculate_layout_hash(
    points_by_address: &[PhysicalPointAddress],
    slots_by_address: &[usize],
    slot_count: usize,
) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    POINT_MANIFEST_DOMAIN.hash(&mut hasher);
    (slot_count as u64).hash(&mut hasher);
    (points_by_address.len() as u64).hash(&mut hasher);
    for (&slot, address) in slots_by_address
        .iter()
        .zip(points_by_address.iter().copied())
    {
        (slot as u64).hash(&mut hasher);
        address.channel_id().get().hash(&mut hasher);
        kind_index(address.kind()).hash(&mut hasher);
        address.point_id().get().hash(&mut hasher);
    }
    hasher.finish()
}

const fn address_key(address: PhysicalPointAddress) -> (u32, u8, u32) {
    (
        address.channel_id().get(),
        kind_index(address.kind()),
        address.point_id().get(),
    )
}

const fn kind_index(kind: PointKind) -> u8 {
    match kind {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    }
}

const fn kind_code(kind: PointKind) -> char {
    match kind {
        PointKind::Telemetry => 'T',
        PointKind::Status => 'S',
        PointKind::Command => 'C',
        PointKind::Action => 'A',
    }
}
