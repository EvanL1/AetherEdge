//! Pure-infra SHM writer.
//!
//! `SlotWriter` owns the mmap region, tracks dirty slots, and exposes
//! slot-indexed I/O. It has no knowledge of channels, point types,
//! instances, or routing — that lives in `UnifiedWriter` (in
//! `unified_shm.rs`) which composes a `SlotWriter` and adds the
//! channel-aware adapters.
//!
//! Consumers that only need slot-level I/O should program against
//! `&SlotWriter` or `&dyn SlotIo`; the channel adapters are not
//! reachable that way by design.

use std::fs::{File, Metadata, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use memmap2::{MmapMut, MmapOptions};

use crate::core::authority::AuthorityReadGuard;
use crate::core::header::{
    HeaderSnapshot, UNIFIED_MAGIC, UNIFIED_VERSION, UnifiedHeader, calculate_file_size,
    slot_offset, validate_mapping_layout,
};
use crate::core::slot::PointSlot;
use crate::core::slot_io::{SlotIo, SlotIoWrite, SlotRead};
use crate::{DataplaneError, DataplaneResult};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackingFileIdentity {
    primary: u64,
    secondary: u64,
}

impl BackingFileIdentity {
    #[cfg(unix)]
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            primary: metadata.dev(),
            secondary: metadata.ino(),
        }
    }

    #[cfg(not(unix))]
    fn from_metadata(metadata: &Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or_default();
        Self {
            primary: metadata.len(),
            secondary: modified,
        }
    }
}

// ========== Dirty bitmap helpers ==========

#[inline]
pub(crate) fn dirty_word_count(slot_count: usize) -> usize {
    slot_count.div_ceil(u64::BITS as usize)
}

pub(crate) fn new_dirty_words(slot_count: usize) -> Vec<AtomicU64> {
    (0..dirty_word_count(slot_count))
        .map(|_| AtomicU64::new(0))
        .collect()
}

// ========== SlotWriter ==========

/// Pure-infra view of a SHM writer.
///
/// Owns the mmap region and the process-local dirty bitmap. Provides
/// slot-indexed read/write and snapshot save. **Does not understand any
/// business concept** (channel, instance, point type, routing).
pub struct SlotWriter {
    pub(crate) mmap: MmapMut,
    pub(crate) path: PathBuf,
    pub(crate) max_slots: u32,
    pub(crate) slot_count: usize,
    backing_identity: BackingFileIdentity,
    /// Process-local dirty slot bitmap for fast SHM→Redis sync.
    ///
    /// PointSlot.dirty is shared across processes, but scanning it still
    /// costs O(slots). This bitmap is set by this writer's `set_direct`
    /// calls so io can drain changed slots in O(dirty_words +
    /// dirty_slots), with periodic full scans as fallback.
    pub(crate) dirty_words: Vec<AtomicU64>,
}

impl SlotWriter {
    /// Creates and publishes a fresh slot-based SHM file.
    ///
    /// `layout_hash` is an opaque composition-provided manifest fingerprint;
    /// the physical data plane stores and exposes it without interpreting it.
    pub fn create(
        path: impl AsRef<Path>,
        max_slots: u32,
        slot_count: usize,
        layout_hash: u64,
    ) -> DataplaneResult<Self> {
        let path = path.as_ref();
        if slot_count > max_slots as usize {
            return Err(DataplaneError::InvalidLayout(format!(
                "slot_count {slot_count} exceeds declared max_slots {max_slots}"
            )));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| {
                DataplaneError::io(format!("create SHM directory {parent:?}"), source)
            })?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|source| DataplaneError::io(format!("create SHM file {path:?}"), source))?;
        let file_size = calculate_file_size(max_slots);
        file.set_len(file_size as u64)
            .map_err(|source| DataplaneError::io(format!("size SHM file {path:?}"), source))?;

        // SAFETY: the writable file has just been sized to the exact mapping
        // length and stays open while the OS creates the mapping. This is the
        // sole owner during initialization, before the canonical path is handed
        // to readers.
        let mut mmap = unsafe { MmapOptions::new().len(file_size).map_mut(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap SHM file {path:?}"), source))?;
        let generation = new_generation();
        let header = UnifiedHeader {
            magic: UNIFIED_MAGIC,
            version: UNIFIED_VERSION,
            max_slots,
            slot_count: AtomicU32::new(slot_count as u32),
            _pad: [0; 4],
            last_update_ts: AtomicU64::new(0),
            writer_heartbeat: AtomicU64::new(0),
            routing_hash: AtomicU64::new(layout_hash),
            writer_generation: AtomicU64::new(generation),
            _reserved: [0; 8],
        };
        // SAFETY: mmap bases are page-aligned, satisfying the header's 64-byte
        // alignment; the map is large enough by construction. `ptr::write`
        // initializes the complete `repr(C)` value before any shared reference
        // or reader exists.
        unsafe { (mmap.as_mut_ptr() as *mut UnifiedHeader).write(header) };

        let writer =
            Self::from_mmap_with_file(mmap, path.to_path_buf(), max_slots, slot_count, &file)?;
        for index in 0..slot_count {
            writer.slot_at(index).init_unwritten();
        }
        writer.flush()?;
        Ok(writer)
    }

    /// Opens an existing writer-owned segment after validating its physical
    /// header against a composition-provided manifest snapshot.
    ///
    /// This operation is business-neutral: callers provide only the expected
    /// live slot count and opaque layout fingerprint. It never creates,
    /// truncates, or reconfigures the canonical segment.
    pub fn open_existing(
        path: impl AsRef<Path>,
        expected_slot_count: usize,
        expected_layout_hash: u64,
    ) -> DataplaneResult<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| {
                DataplaneError::io(format!("open existing SHM file {path:?}"), source)
            })?;
        let file_len = file
            .metadata()
            .map_err(|source| DataplaneError::io(format!("stat SHM file {path:?}"), source))?
            .len() as usize;
        if file_len < std::mem::size_of::<UnifiedHeader>() {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM file {path:?} is shorter than its header"
            )));
        }

        // SAFETY: the file is open read/write, its non-zero header length was
        // checked above, and it remains alive while the OS creates the map.
        let mmap = unsafe { MmapOptions::new().map_mut(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap SHM file {path:?}"), source))?;
        // SAFETY: mmap bases are page-aligned and the checked mapping contains
        // a complete `UnifiedHeader`. No slot pointer is formed until all
        // header-derived bounds have been validated below.
        let header = unsafe { &*(mmap.as_ptr() as *const UnifiedHeader) };
        if header.magic != UNIFIED_MAGIC {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM magic mismatch: expected {UNIFIED_MAGIC:#x}, got {:#x}",
                header.magic
            )));
        }
        if header.version != UNIFIED_VERSION {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM version mismatch: expected {UNIFIED_VERSION}, got {}",
                header.version
            )));
        }

        let snapshot = header.snapshot();
        let slot_count = snapshot.slot_count as usize;
        validate_mapping_layout(mmap.len(), snapshot.max_slots, slot_count)?;
        if slot_count != expected_slot_count {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM slot_count mismatch: expected {expected_slot_count}, got {slot_count}"
            )));
        }
        if snapshot.routing_hash != expected_layout_hash {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM layout hash mismatch: expected {expected_layout_hash:#018x}, got {:#018x}",
                snapshot.routing_hash
            )));
        }
        if snapshot.writer_generation == 0 || snapshot.writer_generation & 1 != 0 {
            return Err(DataplaneError::InvalidLayout(format!(
                "SHM writer generation {} is not a stable published generation",
                snapshot.writer_generation
            )));
        }

        Self::from_mmap_with_file(
            mmap,
            path.to_path_buf(),
            snapshot.max_slots,
            slot_count,
            &file,
        )
    }

    /// Wraps an already-initialized mmap region after validating its bounds.
    ///
    /// The mmap is expected to:
    /// - have a valid `UnifiedHeader` at offset 0 (magic, version, etc. set)
    /// - have `slot_count` slots initialized to a known state (NaN sentinel
    ///   for fresh, or restored live data)
    /// - be at least `calculate_file_size(max_slots)` bytes long
    pub fn from_mmap(
        mmap: MmapMut,
        path: PathBuf,
        max_slots: u32,
        slot_count: usize,
    ) -> DataplaneResult<Self> {
        let metadata = std::fs::metadata(&path).map_err(|source| {
            DataplaneError::io(format!("stat SHM backing path {path:?}"), source)
        })?;
        Self::from_mmap_with_identity(
            mmap,
            path,
            max_slots,
            slot_count,
            BackingFileIdentity::from_metadata(&metadata),
        )
    }

    /// Wraps an mmap and records the identity of the exact file descriptor
    /// used to create it.
    ///
    /// This is the race-free constructor for composition crates that build
    /// their own mappings. Capturing identity from the open descriptor avoids
    /// confusing a concurrently renamed canonical path with the mapped inode.
    #[doc(hidden)]
    pub fn from_mmap_with_file(
        mmap: MmapMut,
        path: PathBuf,
        max_slots: u32,
        slot_count: usize,
        file: &File,
    ) -> DataplaneResult<Self> {
        let metadata = file.metadata().map_err(|source| {
            DataplaneError::io(format!("stat mapped SHM file for {path:?}"), source)
        })?;
        Self::from_mmap_with_identity(
            mmap,
            path,
            max_slots,
            slot_count,
            BackingFileIdentity::from_metadata(&metadata),
        )
    }

    fn from_mmap_with_identity(
        mmap: MmapMut,
        path: PathBuf,
        max_slots: u32,
        slot_count: usize,
        backing_identity: BackingFileIdentity,
    ) -> DataplaneResult<Self> {
        validate_mapping_layout(mmap.len(), max_slots, slot_count)?;
        Ok(Self {
            dirty_words: new_dirty_words(slot_count),
            mmap,
            path,
            max_slots,
            slot_count,
            backing_identity,
        })
    }

    /// Header reference. SAFETY: mmap starts with a valid UnifiedHeader.
    #[inline]
    pub fn header(&self) -> &UnifiedHeader {
        // SAFETY: mmap region starts with a valid UnifiedHeader.
        // UnifiedHeader is #[repr(C, align(64))], mmap base is page-aligned.
        unsafe { &*(self.mmap.as_ptr() as *const UnifiedHeader) }
    }

    /// PointSlot at index. Panics if out of bounds (use `read_slot`/`write_slot`
    /// for fallible variants).
    #[inline]
    #[doc(hidden)]
    pub fn slot_at(&self, index: usize) -> &PointSlot {
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

    #[inline]
    /// Returns the canonical path backing this writer.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Confirms that this mapping still backs the file named by its published
    /// path.
    ///
    /// An atomic rename leaves old mmaps usable and their in-mmap generation
    /// unchanged. Comparing the mapped descriptor identity captured at open
    /// time with a fresh `stat(2)` of the path closes that blind spot. Callers
    /// must check both before and after a write transaction.
    pub fn validate_authoritative_path(&self) -> DataplaneResult<()> {
        let metadata = std::fs::metadata(&self.path).map_err(|source| {
            DataplaneError::io(
                format!("stat authoritative SHM path {:?}", self.path),
                source,
            )
        })?;
        let current = BackingFileIdentity::from_metadata(&metadata);
        if current == self.backing_identity {
            return Ok(());
        }
        Err(DataplaneError::InvalidLayout(format!(
            "mapped SHM file at {:?} is no longer the authoritative path target",
            self.path
        )))
    }

    /// Acquires a shared transaction lease for this writer's canonical path.
    /// Canonical replacement cannot begin until the returned guard is dropped.
    pub fn acquire_authority_read(&self) -> DataplaneResult<AuthorityReadGuard> {
        AuthorityReadGuard::acquire(&self.path)
    }

    /// Attempts to acquire a shared transaction lease without blocking.
    pub fn try_acquire_authority_read(&self) -> DataplaneResult<Option<AuthorityReadGuard>> {
        AuthorityReadGuard::try_acquire(&self.path)
    }

    /// Flush mmap-backed file changes to disk.
    pub fn flush(&self) -> DataplaneResult<()> {
        self.mmap
            .flush()
            .map_err(|source| DataplaneError::io("flush SHM mmap", source))
    }

    /// Direct slot write — the hot path. Panics if `slot` is out of bounds.
    #[inline]
    pub fn set_direct(&self, slot: usize, value: f64, raw: f64, timestamp_ms: u64) {
        assert!(
            slot < self.slot_count,
            "set_direct: slot {} out of bounds (slot_count={})",
            slot,
            self.slot_count
        );
        self.slot_at(slot).set(value, raw, timestamp_ms);
        self.mark_dirty_slot(slot);
        self.header()
            .writer_heartbeat
            .store(timestamp_ms, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn mark_dirty_slot(&self, slot: usize) {
        let word_idx = slot / u64::BITS as usize;
        let bit_idx = slot % u64::BITS as usize;
        if let Some(word) = self.dirty_words.get(word_idx) {
            word.fetch_or(1u64 << bit_idx, Ordering::Release);
        }
    }

    /// Drain process-local dirty slots set by this writer.
    pub fn take_dirty_slots(&self) -> Vec<usize> {
        let mut slots = Vec::new();
        for (word_idx, word) in self.dirty_words.iter().enumerate() {
            let mut bits = word.swap(0, Ordering::AcqRel);
            while bits != 0 {
                let bit_idx = bits.trailing_zeros() as usize;
                let slot = word_idx * u64::BITS as usize + bit_idx;
                if slot < self.slot_count {
                    slots.push(slot);
                }
                bits &= bits - 1;
            }
        }
        slots
    }

    /// Current writer generation (from header).
    pub fn generation(&self) -> u64 {
        self.header().writer_generation.load(Ordering::Acquire)
    }

    /// Most recent heartbeat timestamp written by this writer.
    pub fn writer_heartbeat(&self) -> u64 {
        self.header().writer_heartbeat.load(Ordering::Relaxed)
    }

    /// Update the writer heartbeat without writing a slot.
    pub fn update_heartbeat(&self, timestamp_ms: u64) {
        self.header()
            .writer_heartbeat
            .store(timestamp_ms, Ordering::Relaxed);
    }

    /// Save a snapshot using tear-resistant per-slot serialization.
    ///
    /// Flushes the mmap first so OS-buffered dirty pages are stable in the
    /// backing file before the snapshot's tear-resistant per-slot read,
    /// then delegates to `core::snapshot_save`. This makes the public
    /// `SlotWriter::save_snapshot` self-contained — callers outside
    /// `UnifiedWriter` do not need to remember to flush.
    pub fn save_snapshot(&self, path: &Path) -> DataplaneResult<()> {
        self.flush()?;
        let current_slot_count = self.header().slot_count.load(Ordering::Acquire) as usize;
        crate::core::snapshot_save::save_snapshot_impl(
            &self.mmap,
            current_slot_count,
            path,
            "SlotWriter",
        )
    }
}

fn new_generation() -> u64 {
    static GENERATION_SEQUENCE: AtomicU64 = AtomicU64::new(2);
    let sequence = GENERATION_SEQUENCE.fetch_add(2, Ordering::Relaxed);
    let wall_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    (wall_nanos.wrapping_add(sequence) & !1_u64).max(2)
}

// ========== SlotIo (read view) impl ==========

impl SlotIo for SlotWriter {
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
        SlotWriter::generation(self)
    }

    fn writer_heartbeat(&self) -> u64 {
        SlotWriter::writer_heartbeat(self)
    }

    fn header(&self) -> HeaderSnapshot {
        SlotWriter::header(self).snapshot()
    }
}

// ========== SlotIoWrite (mutating view) impl ==========

impl SlotIoWrite for SlotWriter {
    fn write_slot(&self, index: usize, value: f64, raw: f64, timestamp_ms: u64) -> bool {
        if index >= self.slot_count {
            return false;
        }
        self.slot_at(index).set(value, raw, timestamp_ms);
        self.mark_dirty_slot(index);
        self.header()
            .writer_heartbeat
            .store(timestamp_ms, Ordering::Relaxed);
        true
    }

    fn take_dirty_slots(&self) -> Vec<usize> {
        SlotWriter::take_dirty_slots(self)
    }
}
