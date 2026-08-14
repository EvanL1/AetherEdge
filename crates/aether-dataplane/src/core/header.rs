//! On-mmap SHM header — the physical layout shared by all readers and writers.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::core::slot::PointSlot;
use crate::{DataplaneError, DataplaneResult};

/// AetherEdge shared-memory magic (`AETHER__` as a big-endian integer).
pub const AETHER_SHM_MAGIC: u64 = u64::from_be_bytes(*b"AETHER__");

/// Physical shared-memory header.
///
/// Layout: 64 bytes, cache-line aligned. All multi-byte fields use native
/// endianness; readers and writers must run on the same architecture.
#[repr(C, align(64))]
pub struct ShmHeader {
    /// Physical layout magic.
    pub magic: u64,
    /// Reserved contract space. The only accepted layout requires zero.
    pub(crate) reserved: u32,
    /// Current live slot count.
    pub slot_count: AtomicU32,
    /// Owner-controlled liveness heartbeat in milliseconds since UNIX epoch.
    pub writer_heartbeat: AtomicU64,
    /// Composition-provided physical slot-layout fingerprint.
    pub layout_hash: AtomicU64,
    /// Writer generation counter; odd values invalidate retained mappings.
    pub writer_generation: AtomicU64,
    /// Cross-plane publication identity. Zero is never a valid publication.
    pub publication_epoch: u64,
    /// Reserved contract space. The only accepted layout requires zero.
    pub(crate) reserved_tail: [u8; 16],
}

const _: () = assert!(std::mem::size_of::<ShmHeader>() == 64);
const _: () = assert!(std::mem::offset_of!(ShmHeader, magic) == 0);
const _: () = assert!(std::mem::offset_of!(ShmHeader, reserved) == 8);
const _: () = assert!(std::mem::offset_of!(ShmHeader, slot_count) == 12);

/// Read-only value snapshot of the physical SHM header.
///
/// Unlike [`ShmHeader`], this type exposes no atomic cells and therefore
/// cannot be used to write through a read-only mmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderSnapshot {
    /// Physical layout magic.
    pub magic: u64,
    /// Current live slot count.
    pub slot_count: u32,
    /// Most recent owner heartbeat.
    pub writer_heartbeat: u64,
    /// Composition-provided physical slot-layout fingerprint.
    pub layout_hash: u64,
    /// Current writer generation.
    pub writer_generation: u64,
    /// Cross-plane publication identity.
    pub publication_epoch: u64,
}

impl ShmHeader {
    /// Returns the cross-plane publication identity.
    #[must_use]
    pub const fn publication_epoch(&self) -> u64 {
        self.publication_epoch
    }

    /// Copies the current header values into a non-mutable view.
    #[must_use]
    pub fn snapshot(&self) -> HeaderSnapshot {
        HeaderSnapshot {
            magic: self.magic,
            slot_count: self.slot_count.load(Ordering::Acquire),
            writer_heartbeat: self.writer_heartbeat.load(Ordering::Relaxed),
            layout_hash: self.layout_hash.load(Ordering::Acquire),
            writer_generation: self.writer_generation.load(Ordering::Acquire),
            publication_epoch: self.publication_epoch,
        }
    }

    /// Returns whether every byte reserved by the one accepted contract is zero.
    #[must_use]
    pub(crate) fn reserved_bytes_are_zero(&self) -> bool {
        self.reserved == 0 && self.reserved_tail.iter().all(|byte| *byte == 0)
    }
}

/// Total file size required for a SHM with `slot_count` live slots.
///
/// Layout: Header (64B) + PointSlot\[slot_count\] (32B each).
#[inline]
pub const fn calculate_file_size(slot_count: u32) -> usize {
    std::mem::size_of::<ShmHeader>() + (slot_count as usize) * std::mem::size_of::<PointSlot>()
}

/// Byte offset of the PointSlot array within the mmap region.
#[inline]
pub const fn slot_offset() -> usize {
    std::mem::size_of::<ShmHeader>()
}

/// Validates that a mapping can safely contain the declared slot layout.
///
/// This check must run before any header or slot pointer is dereferenced.
pub(crate) fn validate_mapping_layout(mapped_len: usize, slot_count: usize) -> DataplaneResult<()> {
    let slot_count = u32::try_from(slot_count).map_err(|_| {
        DataplaneError::InvalidLayout(format!("slot_count {slot_count} exceeds u32::MAX"))
    })?;
    let required = calculate_file_size(slot_count);
    if mapped_len != required {
        return Err(DataplaneError::InvalidLayout(format!(
            "SHM mapping length mismatch: have {mapped_len} bytes, need exactly {required} for slot_count={slot_count}"
        )));
    }

    Ok(())
}
