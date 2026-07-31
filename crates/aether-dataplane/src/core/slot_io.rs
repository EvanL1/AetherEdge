//! Pure-infra contract for SHM slot I/O.
//!
//! `SlotIo` is the declaration of what a "business-unaware SHM writer" can do:
//! address slots by index, write/read seqlocked cells, track dirty slots,
//! observe header state. **Nothing on this trait mentions channels, point
//! types, instances, or routing** — that is by design.
//!
//! `UnifiedWriter` (in `unified_shm.rs`) implements `SlotIo` to expose its
//! pure-infra capabilities. Its inherent methods continue to carry the
//! channel/point-type adapters; those adapters are deliberately NOT part of
//! this trait. Any caller that only needs slot-level I/O (snapshot restore,
//! pure-infra tests, future generic tools) should program against `dyn
//! SlotIo` so that the type system rejects business coupling at compile
//! time.
//!
//! This trait is intentionally not object-safe in pursuit of one thing or
//! another — it is plain `&self` so it composes with `Arc<dyn SlotIo>` if
//! callers want dynamic dispatch.

use crate::core::header::{HeaderSnapshot, slot_offset};
use crate::core::slot::PointSlot;

/// Addresses a slot inside a validated SHM mapping.
///
/// `SlotWriter` and `SlotReader` map the same layout through `MmapMut` and
/// `Mmap` respectively, so both delegate here. Keeping one implementation
/// keeps the alignment argument below in a single place: two copies of this
/// `unsafe` block could drift apart without the compiler noticing.
pub(crate) fn slot_at(mmap: &[u8], slot_count: usize, index: usize) -> &PointSlot {
    assert!(
        index < slot_count,
        "slot_at: index {} out of bounds (slot_count={})",
        index,
        slot_count
    );
    // SAFETY: alignment chain — mmap base is page-aligned (≥4096),
    // slot_offset() == size_of::<UnifiedHeader>() == 64 (asserted at
    // const time in core::header), and 64 is divisible by 32. So the
    // base pointer for the slot array is 32-byte aligned, matching
    // PointSlot's `#[repr(C, align(32))]` requirement. `index` is
    // bounds-checked above against `slot_count`, and the constructor
    // verified the mmap covers at least `max_slots` slots.
    unsafe {
        let ptr = mmap.as_ptr().add(slot_offset()) as *const PointSlot;
        &*ptr.add(index)
    }
}

/// Reads a slot consistently, returning `None` for an out-of-range index or
/// an exhausted seqlock retry budget.
pub(crate) fn read_slot(mmap: &[u8], slot_count: usize, index: usize) -> Option<SlotRead> {
    if index >= slot_count {
        return None;
    }
    let (value, raw, timestamp_ms) = slot_at(mmap, slot_count, index).try_load_consistent()?;
    Some(SlotRead {
        value,
        raw,
        timestamp_ms,
    })
}

/// A consistent read of a slot's measurement state.
#[derive(Debug, Clone, Copy)]
pub struct SlotRead {
    /// Engineering-unit value (may be `NaN` for unwritten slots).
    pub value: f64,
    /// Raw protocol-level value (may be `NaN`).
    pub raw: f64,
    /// Wall-clock timestamp in ms since UNIX epoch.
    pub timestamp_ms: u64,
}

/// Pure-infra **read view** of a SHM segment: slot-level reads and header
/// introspection.
///
/// Read access returns a value snapshot (`SlotRead`), never a reference to
/// the underlying atomic cell — exposing `&PointSlot` would let a caller
/// call `PointSlot::set` directly and bypass the writer's dirty-tracking
/// invariants. Mutating access lives on the sub-trait
/// [`SlotIoWrite`], so the type system rejects code that accepts
/// `&dyn SlotIo` from attempting to write.
pub trait SlotIo: Send + Sync {
    /// Number of slots currently live in this SHM.
    fn slot_count(&self) -> usize;

    /// Read a slot's current measurement using a seqlock-consistent load.
    ///
    /// Returns `None` if the index is out of bounds **or** if the seqlock
    /// retry budget was exhausted (a writer was concurrently mid-update).
    /// In the latter case, callers should retry on a subsequent tick — the
    /// torn-read window is microseconds.
    fn read_slot(&self, index: usize) -> Option<SlotRead>;

    /// Current writer generation. Bumped by the writer on each
    /// create/reconfigure; consumed by readers to detect writer restarts.
    fn generation(&self) -> u64;

    /// Most recent writer heartbeat timestamp (ms since UNIX epoch).
    fn writer_heartbeat(&self) -> u64;

    /// Copies the current header metadata into a read-only value snapshot.
    fn header(&self) -> HeaderSnapshot;
}

/// Pure-infra **write view** of a SHM segment. Sub-trait of `SlotIo` —
/// any writer is also a reader, but not vice versa.
///
/// Implementations must mark each written slot as dirty so a subsequent
/// [`take_dirty_slots`](Self::take_dirty_slots) call surfaces it; this is
/// the contract that makes downstream Redis-sync sweeps O(dirty) instead
/// of O(slot_count).
pub trait SlotIoWrite: SlotIo {
    /// Write a measurement to a slot. Returns `false` if the index is
    /// out of bounds.
    fn write_slot(&self, index: usize, value: f64, raw: f64, timestamp_ms: u64) -> bool;

    /// Drain and return the set of slot indices written since the last drain.
    fn take_dirty_slots(&self) -> Vec<usize>;
}
