//! Pure-infra SHM reader.
//!
//! `SlotReader` owns a read-only mmap of a SHM segment and exposes
//! slot-indexed reads, header introspection, and snapshot save. Like
//! `SlotWriter`, it has no knowledge of channels, point types,
//! instances, or routing.
//!
//! `UnifiedReader` (in `unified_shm.rs`) composes this struct and adds
//! the channel-aware iterators; consumers that only need slot reads
//! should program against `&SlotReader` or `&dyn SlotIo`.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::Ordering;

use memmap2::{Mmap, MmapOptions};

use crate::core::header::{
    HeaderSnapshot, UNIFIED_MAGIC, UNIFIED_VERSION, UnifiedHeader, slot_offset,
    validate_mapping_layout,
};
use crate::core::slot::PointSlot;
use crate::core::slot_io::{SlotIo, SlotRead};
use crate::{DataplaneError, DataplaneResult};

/// Pure-infra view of a SHM reader.
///
/// Owns the read-only mmap. Provides slot-indexed reads and header
/// access. **Does not understand any business concept** (channel,
/// instance, point type, routing).
pub struct SlotReader {
    pub(crate) mmap: Mmap,
    pub(crate) max_slots: u32,
    pub(crate) slot_count: usize,
}

impl SlotReader {
    /// Opens and validates a physical SHM file through a read-only mapping.
    ///
    /// This is the only file-to-mmap entry point required by read-side
    /// extensions. It validates the minimum file length before interpreting the
    /// header, then validates magic, version, capacity, and live slot count
    /// before any slot can be read.
    pub fn open(path: impl AsRef<Path>) -> DataplaneResult<Self> {
        let path = path.as_ref();
        let file = File::open(path)
            .map_err(|source| DataplaneError::io(format!("open SHM file {path:?}"), source))?;
        let file_len = file
            .metadata()
            .map_err(|source| DataplaneError::io(format!("stat SHM file {path:?}"), source))?
            .len() as usize;
        let header_len = std::mem::size_of::<UnifiedHeader>();
        if file_len < header_len {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM file {path:?} is shorter than its header: {file_len} < {header_len}"
            )));
        }

        // SAFETY: `file` is opened read-only and remains alive while the OS
        // creates the mapping. We do not expose mutable access to the returned
        // `Mmap`; all shared fields are subsequently read through atomics.
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap SHM file {path:?}"), source))?;
        if mmap.len() < header_len {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM mapping for {path:?} is shorter than its header: {} < {header_len}",
                mmap.len()
            )));
        }

        // SAFETY: the mapping length was checked above; mmap bases are
        // page-aligned, which satisfies `UnifiedHeader`'s 64-byte alignment;
        // integer atomics accept every bit pattern. The writer initializes this
        // fixed `repr(C)` header before publishing the canonical path.
        let header = unsafe { &*(mmap.as_ptr() as *const UnifiedHeader) };
        let snapshot = header.snapshot();
        if snapshot.magic != UNIFIED_MAGIC {
            return Err(DataplaneError::InvalidLayout(format!(
                "invalid SHM magic for {path:?}: expected 0x{UNIFIED_MAGIC:X}, got 0x{:X}",
                snapshot.magic
            )));
        }
        if snapshot.version != UNIFIED_VERSION {
            return Err(DataplaneError::InvalidLayout(format!(
                "unsupported SHM version for {path:?}: expected {UNIFIED_VERSION}, got {}",
                snapshot.version
            )));
        }

        Self::from_mmap(mmap, snapshot.max_slots, snapshot.slot_count as usize)
    }

    /// Wraps an already-opened mmap after validating its physical bounds.
    ///
    /// Logical metadata such as magic, version, and manifest identity remains
    /// the caller's policy decision; this constructor guarantees only that
    /// subsequent header and slot pointer dereferences stay inside the map.
    pub fn from_mmap(mmap: Mmap, max_slots: u32, slot_count: usize) -> DataplaneResult<Self> {
        validate_mapping_layout(mmap.len(), max_slots, slot_count)?;
        Ok(Self {
            mmap,
            max_slots,
            slot_count,
        })
    }

    /// Copies the current header into a read-only value snapshot.
    #[inline]
    pub fn header(&self) -> HeaderSnapshot {
        self.header_atomic().snapshot()
    }

    #[inline]
    fn header_atomic(&self) -> &UnifiedHeader {
        // SAFETY: mmap region starts with a valid UnifiedHeader.
        unsafe { &*(self.mmap.as_ptr() as *const UnifiedHeader) }
    }

    /// PointSlot at index. Panics if out of bounds.
    #[inline]
    pub(crate) fn slot_at(&self, index: usize) -> &PointSlot {
        assert!(
            index < self.slot_count,
            "slot_at: index {} out of bounds (slot_count={})",
            index,
            self.slot_count
        );
        // SAFETY: alignment chain — mmap base is page-aligned (≥4096),
        // slot_offset() == size_of::<UnifiedHeader>() == 64 (asserted at
        // const time in core::header), and 64 is divisible by 32. So the
        // base pointer for the slot array is 32-byte aligned, matching
        // PointSlot's `#[repr(C, align(32))]` requirement. `index` is
        // bounds-checked above against `slot_count`, and the constructor
        // verified the mmap covers at least `max_slots` slots.
        unsafe {
            let ptr = self.mmap.as_ptr().add(slot_offset()) as *const PointSlot;
            &*ptr.add(index)
        }
    }

    #[inline]
    /// Returns the number of live slots declared by the mapped header.
    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    #[inline]
    /// Returns the maximum slot capacity of the mapped file.
    pub fn max_slots(&self) -> u32 {
        self.max_slots
    }

    /// Most recent heartbeat timestamp written by the writer.
    pub fn writer_heartbeat(&self) -> u64 {
        self.header_atomic()
            .writer_heartbeat
            .load(Ordering::Relaxed)
    }

    /// Check if the writer is alive within the given timeout.
    pub fn is_writer_alive(&self, timeout_ms: u64) -> bool {
        let last_hb = self.writer_heartbeat();
        if last_hb == 0 {
            return false;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        now_ms.saturating_sub(last_hb) < timeout_ms
    }

    /// Save a tear-resistant snapshot of the current SHM state.
    ///
    /// Uses `self.slot_count` (captured at `open()`) rather than the live
    /// `header().slot_count` field. Reading the live header here would race
    /// with io's `reconfigure_existing`: during the reconfigure window
    /// `header.slot_count` has already been advanced to the new value, but
    /// the slot array is still being zeroed. Snapshotting through the live
    /// count in that window would capture all-zero slots that read as
    /// `value=0.0` (a valid finite reading) rather than NaN sentinels, and
    /// the snapshot would silently encode garbage as live data.
    pub fn save_snapshot(&self, path: &Path) -> DataplaneResult<()> {
        crate::core::snapshot_save::save_snapshot_impl(
            &self.mmap,
            self.slot_count,
            path,
            "SlotReader",
        )
    }
}

// ========== SlotIo (read-only) impl ==========

impl SlotIo for SlotReader {
    #[inline]
    fn slot_count(&self) -> usize {
        self.slot_count
    }

    fn read_slot(&self, index: usize) -> Option<SlotRead> {
        if index >= self.slot_count {
            return None;
        }
        let (value, raw, timestamp_ms) = self.slot_at(index).try_load_consistent()?;
        Some(SlotRead {
            value,
            raw,
            timestamp_ms,
        })
    }

    fn generation(&self) -> u64 {
        self.header_atomic()
            .writer_generation
            .load(Ordering::Acquire)
    }

    fn writer_heartbeat(&self) -> u64 {
        SlotReader::writer_heartbeat(self)
    }

    fn header(&self) -> HeaderSnapshot {
        SlotReader::header(self)
    }
}
