//! On-mmap SHM header — the **physical** layout shared by all readers/writers.
//!
//! This module is pure infra: every field is a generic atomic/integer/byte
//! buffer with no business meaning. The field names retain their historical
//! identifiers (`routing_hash`) for now — that hash is fundamentally a
//! manifest fingerprint computed by the business layer, not something the
//! header itself interprets.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::core::slot::PointSlot;
use crate::{DataplaneError, DataplaneResult};

// ========== Constants ==========

/// Magic number for unified shared memory: "AETHER_" in ASCII.
pub const UNIFIED_MAGIC: u64 = 0x564F4C544147455F;

/// SHM layout version.
///
/// - v3 changed the slot default from `(value=0.0, raw=0.0)` to
///   `(value=NaN, raw=NaN)`, so unwritten slots are self-describing instead
///   of relying on the `seq==0` side channel.
/// - v4 added physical padding slots to keep writer-ownership boundaries on
///   fresh cache lines. This changes physical slot indices even when channel
///   point counts are unchanged, so v3 snapshots must be rejected rather than
///   restored into the wrong slots.
pub const UNIFIED_VERSION: u32 = 4;

/// Default max slots (100,000 points).
pub const DEFAULT_MAX_SLOTS: u32 = 100_000;

// ========== Header ==========

/// Unified shared memory header.
///
/// Layout: 64 bytes, cache-line aligned. All multi-byte fields use native
/// endianness; readers and writers must run on the same architecture (we
/// only deploy on aarch64/x86_64 Linux).
#[repr(C, align(64))]
pub struct UnifiedHeader {
    /// Magic number for validation ("AETHER_")
    pub magic: u64,
    /// Version number
    pub version: u32,
    /// Maximum number of slots
    pub max_slots: u32,
    /// Current slot count (atomically updated)
    pub slot_count: AtomicU32,
    /// Padding for alignment
    pub _pad: [u8; 4],
    /// Last update timestamp (for monitoring)
    pub last_update_ts: AtomicU64,
    /// Writer heartbeat (for monitoring)
    pub writer_heartbeat: AtomicU64,
    /// Manifest-style layout fingerprint for cross-process validation.
    ///
    /// Historical name `routing_hash`: io writes the hash of its
    /// `ChannelPointCounts` layout on create; automation verifies its own hash
    /// matches on open. The header itself does not interpret the hash —
    /// what it fingerprints is a business-layer concern.
    pub routing_hash: AtomicU64,
    /// Writer generation counter — bumped on every create/reconfigure.
    /// Readers observe odd values to detect a reconfigure-in-progress window.
    pub writer_generation: AtomicU64,
    /// Reserved for future use
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<UnifiedHeader>() == 64);

/// Read-only value snapshot of the physical SHM header.
///
/// Unlike [`UnifiedHeader`], this type exposes no atomic cells and therefore
/// cannot be used to write through a read-only mmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderSnapshot {
    /// Physical layout magic.
    pub magic: u64,
    /// Physical layout version.
    pub version: u32,
    /// Allocated slot capacity.
    pub max_slots: u32,
    /// Current live slot count.
    pub slot_count: u32,
    /// Last data-plane update timestamp.
    pub last_update_ts: u64,
    /// Most recent writer heartbeat.
    pub writer_heartbeat: u64,
    /// Composition-provided manifest fingerprint.
    pub routing_hash: u64,
    /// Current writer generation.
    pub writer_generation: u64,
}

impl UnifiedHeader {
    /// Copies the current header values into a non-mutable view.
    #[must_use]
    pub fn snapshot(&self) -> HeaderSnapshot {
        HeaderSnapshot {
            magic: self.magic,
            version: self.version,
            max_slots: self.max_slots,
            slot_count: self.slot_count.load(Ordering::Acquire),
            last_update_ts: self.last_update_ts.load(Ordering::Relaxed),
            writer_heartbeat: self.writer_heartbeat.load(Ordering::Relaxed),
            routing_hash: self.routing_hash.load(Ordering::Acquire),
            writer_generation: self.writer_generation.load(Ordering::Acquire),
        }
    }
}

// ========== Layout Math ==========

/// Total file size required for a SHM with `max_slots` slots.
///
/// Layout: Header (64B) + PointSlot\[max_slots\] (32B each).
#[inline]
pub const fn calculate_file_size(max_slots: u32) -> usize {
    std::mem::size_of::<UnifiedHeader>() + (max_slots as usize) * std::mem::size_of::<PointSlot>()
}

/// Byte offset of the PointSlot array within the mmap region.
#[inline]
pub const fn slot_offset() -> usize {
    std::mem::size_of::<UnifiedHeader>()
}

/// Validates that a mapping can safely contain the declared slot layout.
///
/// This check must run before any header or slot pointer is dereferenced.
pub(crate) fn validate_mapping_layout(
    mapped_len: usize,
    max_slots: u32,
    slot_count: usize,
) -> DataplaneResult<()> {
    if slot_count > max_slots as usize {
        return Err(DataplaneError::InvalidLayout(format!(
            "slot_count {slot_count} exceeds declared max_slots {max_slots}"
        )));
    }

    let required = calculate_file_size(max_slots);
    if mapped_len < required {
        return Err(DataplaneError::InvalidLayout(format!(
            "SHM mapping too small: have {mapped_len} bytes, need {required} for max_slots={max_slots}"
        )));
    }

    Ok(())
}
