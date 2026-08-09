//! Pure-infra SHM reader.
//!
//! `SlotReader` owns a read-only mmap of a SHM segment and exposes
//! slot-indexed reads, header introspection, and snapshot save. Like
//! `SlotWriter`, it has no knowledge of channels, point types,
//! instances, or routing.
//!
//! Consumers that only need slot reads take a [`SlotIo`]
//! bound rather than this concrete type; the trait carries no write
//! capability, so such code provably cannot mutate the segment.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::Ordering;

use memmap2::{Mmap, MmapOptions};

use crate::core::header::{
    AETHER_SHM_MAGIC, HeaderSnapshot, SHM_LAYOUT_VERSION, ShmHeader, validate_mapping_layout,
};
use crate::core::slot_io::{self, SlotIo, SlotRead};
use crate::{DataplaneError, DataplaneResult};

/// Pure-infra view of a SHM reader.
///
/// Owns the read-only mmap. Provides slot-indexed reads and header
/// access. **Does not understand any business concept** (channel,
/// instance, point type, routing).
pub struct SlotReader {
    pub(crate) mmap: Mmap,
    pub(crate) slot_count: usize,
}

impl SlotReader {
    /// Opens and validates a physical SHM file through a read-only mapping.
    ///
    /// This is the only file-to-mmap entry point required by read-side
    /// extensions. It validates the minimum file length before interpreting the
    /// header, then validates magic, version, live slot count, and exact length
    /// before any slot can be read.
    pub fn open(path: impl AsRef<Path>) -> DataplaneResult<Self> {
        let path = path.as_ref();
        let file = File::open(path)
            .map_err(|source| DataplaneError::io(format!("open SHM file {path:?}"), source))?;
        let file_len = file
            .metadata()
            .map_err(|source| DataplaneError::io(format!("stat SHM file {path:?}"), source))?
            .len() as usize;
        let header_len = std::mem::size_of::<ShmHeader>();
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
        // page-aligned, which satisfies `ShmHeader`'s 64-byte alignment;
        // integer atomics accept every bit pattern. The writer initializes this
        // fixed `repr(C)` header before publishing the canonical path.
        let header = unsafe { &*(mmap.as_ptr() as *const ShmHeader) };
        let snapshot = header.snapshot();
        if snapshot.magic != AETHER_SHM_MAGIC {
            return Err(DataplaneError::InvalidLayout(format!(
                "invalid SHM magic for {path:?}: expected 0x{AETHER_SHM_MAGIC:X}, got 0x{:X}",
                snapshot.magic
            )));
        }
        if snapshot.version != SHM_LAYOUT_VERSION {
            return Err(DataplaneError::InvalidLayout(format!(
                "unsupported SHM version for {path:?}: expected {SHM_LAYOUT_VERSION}, got {}",
                snapshot.version
            )));
        }
        if snapshot.publication_epoch == 0 {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM publication epoch is zero for {path:?}"
            )));
        }

        Self::from_mmap(mmap, snapshot.slot_count as usize)
    }

    fn from_mmap(mmap: Mmap, slot_count: usize) -> DataplaneResult<Self> {
        validate_mapping_layout(mmap.len(), slot_count)?;
        Ok(Self { mmap, slot_count })
    }

    /// Copies the current header into a read-only value snapshot.
    #[inline]
    pub fn header(&self) -> HeaderSnapshot {
        self.header_atomic().snapshot()
    }

    /// Returns the cross-plane publication identity stored in the header.
    #[inline]
    pub fn publication_epoch(&self) -> u64 {
        self.header_atomic().publication_epoch()
    }

    #[inline]
    fn header_atomic(&self) -> &ShmHeader {
        // SAFETY: mmap region starts with a valid ShmHeader.
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    #[inline]
    /// Returns the number of live slots declared by the mapped header.
    pub fn slot_count(&self) -> usize {
        self.slot_count
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
}

// ========== SlotIo (read-only) impl ==========

impl SlotIo for SlotReader {
    #[inline]
    fn slot_count(&self) -> usize {
        self.slot_count
    }

    fn read_slot(&self, index: usize) -> Option<SlotRead> {
        slot_io::read_slot(&self.mmap, self.slot_count, index)
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
