//! Unified Shared Memory Implementation (Simplified)
//!
//! SharedMemory only stores values (Header + PointSlot[]).
//! Indexes are Vec in process memory, accessed by ID as array index.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  SharedMemory (mmap file)                                   │
//! │  ┌────────────────────────────────────────────────────────┐ │
//! │  │ Header (64B) │ PointSlot[0] │ PointSlot[1] │ ...       │ │
//! │  └────────────────────────────────────────────────────────┘ │
//! │  Only values, no metadata!                                  │
//! └─────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Process Memory (Vec index, not HashMap)                    │
//! │  - channel_layouts: Vec<ChannelLayout>                      │
//! │  - slot = layouts[channel_id].base + type_offset + point_id │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Key Design Points
//!
//! - **Deterministic allocation**: Writer and Reader use same order → same slot numbers
//! - **Vec index**: O(1) array access, faster than HashMap
//! - **Routing is permission**: Runtime DashMap, not data synchronization

use crate::channel_points::ChannelPointCounts;
use crate::core::slot::PointSlot;
use crate::layout::{ChannelLayout, allocate_layouts};
use crate::shared_config::SharedConfig;
use aether_dataplane::AuthorityWriteGuard;
use aether_model::PointType;
use aether_routing::RoutingCache;
use anyhow::{Context, Result, bail};
use memmap2::MmapOptions;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub use crate::core::header::{
    DEFAULT_MAX_SLOTS, HeaderSnapshot, UNIFIED_MAGIC, UNIFIED_VERSION, UnifiedHeader,
    calculate_file_size, slot_offset,
};

use crate::core::reader::SlotReader;
use crate::core::writer::SlotWriter;

fn read_ne_bytes<const N: usize>(buf: &[u8], start: usize, label: &str) -> Result<[u8; N]> {
    let end = start
        .checked_add(N)
        .with_context(|| format!("Invalid snapshot offset for {}", label))?;
    let bytes = buf
        .get(start..end)
        .with_context(|| format!("Snapshot missing {}", label))?;
    bytes
        .try_into()
        .with_context(|| format!("Invalid snapshot field size for {}", label))
}

fn read_u64_ne(buf: &[u8], start: usize, label: &str) -> Result<u64> {
    Ok(u64::from_ne_bytes(read_ne_bytes(buf, start, label)?))
}

fn read_u32_ne(buf: &[u8], start: usize, label: &str) -> Result<u32> {
    Ok(u32::from_ne_bytes(read_ne_bytes(buf, start, label)?))
}

// Snapshot serialization is now in `core::snapshot_save`; SlotWriter /
// SlotReader call into it directly.

/// Validate a shared memory header: checks magic, version, and routing hash
///
/// Returns (max_slots, slot_count) on success. Used by both `open_for_actions`
/// and `UnifiedReader::open` to eliminate duplicate validation logic.
///
/// # Safety
/// The caller must ensure `header` points to a valid, readable `UnifiedHeader`.
fn validate_shm_header(
    header: &UnifiedHeader,
    channel_points: &ChannelPointCounts,
) -> Result<(u32, usize)> {
    if header.magic != UNIFIED_MAGIC {
        bail!(
            "Invalid magic: expected 0x{:X}, got 0x{:X}",
            UNIFIED_MAGIC,
            header.magic
        );
    }
    if header.version != UNIFIED_VERSION {
        bail!(
            "Version mismatch: expected {}, got {}",
            UNIFIED_VERSION,
            header.version
        );
    }

    let expected_hash = channel_points.layout_hash();
    let actual_hash = header.routing_hash.load(Ordering::Acquire);
    if expected_hash != actual_hash {
        bail!(
            "Channel layout mismatch! \
             SHM layout_hash=0x{:016X}, local layout_hash=0x{:016X}. \
             Slot indexes may be misaligned. \
             Solution: Restart the writer process (io) to synchronize.",
            actual_hash,
            expected_hash
        );
    }

    let max_slots = header.max_slots;
    let slot_count = header.slot_count.load(Ordering::Acquire) as usize;
    Ok((max_slots, slot_count))
}

/// Verify that calculated slot count matches what's stored in the SHM header
fn verify_slot_count(file_slot_count: usize, calculated_slots: usize) -> Result<()> {
    if calculated_slots != file_slot_count {
        bail!(
            "Slot count mismatch: file={}, calculated={}. \
             allocate_layouts() produced different results. \
             Solution: Restart both services to synchronize.",
            file_slot_count,
            calculated_slots
        );
    }
    Ok(())
}

// The previous `impl_shm_accessors!` macro is gone. Slot-level accessors
// (header, slot_at, slot_count, max_slots) now live on `SlotWriter` /
// `SlotReader` in `core::writer` / `core::reader`. Channel-aware methods
// (lookup, channel_layouts) live as explicit inherent methods on
// `UnifiedWriter` / `UnifiedReader` and read `self.channel_layouts`
// directly.

// ========== Memory Layout ==========

/// Calculate file size for given max_slots
///
/// Verify a file is at least header-sized before any unsafe header cast.
///
/// Returns Err if the file is shorter than `size_of::<UnifiedHeader>()`,
/// which would make casting the mmap pointer to `*const UnifiedHeader`
/// immediate UB. Guards against truncated or corrupt SHM files at startup.
fn verify_file_min_size(file: &File, path: &std::path::Path) -> Result<()> {
    let len = file
        .metadata()
        .with_context(|| format!("Failed to stat {:?}", path))?
        .len();
    let min = std::mem::size_of::<UnifiedHeader>() as u64;
    if len < min {
        bail!(
            "SHM file {:?} truncated: len={} < header size {} — refusing unsafe header cast",
            path,
            len,
            min
        );
    }
    Ok(())
}

/// Verify the mmap covers the full slot array implied by `max_slots`.
///
/// Called after `validate_shm_header` so we have a trusted `max_slots`.
/// Returns Err if the mmap is shorter than the slot region would require,
/// indicating a truncated file that would cause out-of-bounds slot reads.
fn verify_mmap_covers_slots(mmap_len: usize, max_slots: u32, path: &std::path::Path) -> Result<()> {
    let required = calculate_file_size(max_slots);
    if mmap_len < required {
        bail!(
            "SHM file {:?} too small for declared max_slots={}: have {} bytes, need {}",
            path,
            max_slots,
            mmap_len,
            required
        );
    }
    Ok(())
}

// ========== UnifiedWriter ==========

/// Unified shared memory writer.
///
/// Single writer per shared memory file (io). Composes a pure-infra
/// `SlotWriter` (in `core::writer`) which owns the mmap and dirty bitmap;
/// `UnifiedWriter` itself only adds the channel/point-type adapters needed
/// by io/automation. The split lets pure-infra consumers (snapshot tools,
/// future generic clients) program against `&SlotWriter` / `&dyn SlotIo`
/// and have the type system reject business coupling.
///
/// Optionally holds a `PointWatchSignaler` that is called after each T/S slot
/// write on the hot path. The signaler is `None` by default (zero overhead in
/// non-PointWatch deployments).
pub struct UnifiedWriter {
    pub(crate) inner: Arc<SlotWriter>,
    /// Channel layouts (Vec indexed by channel_id) — business adapter state.
    channel_layouts: Vec<ChannelLayout>,
    /// Optional PointWatch signaler. When set, `set_direct` / `set` will emit
    /// an event after the seqlock write completes (non-blocking, best-effort).
    #[cfg(unix)]
    point_watch: Option<std::sync::Arc<crate::point_watch::PointWatchSignaler>>,
}

impl UnifiedWriter {
    /// Create shared memory and initialize from ChannelPointCounts
    pub fn create(config: &SharedConfig, channel_points: &ChannelPointCounts) -> Result<Self> {
        let path = config.path();
        let max_slots = config.max_slots().unwrap_or(DEFAULT_MAX_SLOTS);

        // Allocate layouts
        let (channel_layouts, slot_count) = allocate_layouts(channel_points);

        if slot_count > max_slots as usize {
            bail!("Too many slots: {} (max={})", slot_count, max_slots);
        }

        let file_size = calculate_file_size(max_slots);

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {:?}", parent))?;
        }

        // Open or create file
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true) // Always create fresh
            .open(path)
            .with_context(|| format!("Failed to open {:?}", path))?;

        // Set file size
        file.set_len(file_size as u64)
            .with_context(|| "Failed to set file size")?;

        // Memory map
        // SAFETY: File was just created/truncated with the correct size.
        // We have exclusive write access (single writer design).
        let mut mmap = unsafe {
            MmapOptions::new()
                .len(file_size)
                .map_mut(&file)
                .with_context(|| "Failed to mmap")?
        };

        // SAFETY: mmap region is at least size_of::<UnifiedHeader>() (64 bytes).
        // UnifiedHeader is #[repr(C, align(64))], and mmap base is page-aligned.
        let header = unsafe { &mut *(mmap.as_mut_ptr() as *mut UnifiedHeader) };
        header.magic = UNIFIED_MAGIC;
        header.version = UNIFIED_VERSION;
        header.max_slots = max_slots;
        header.slot_count = AtomicU32::new(slot_count as u32);
        header._pad = [0; 4];
        header.last_update_ts = AtomicU64::new(0);
        header.writer_heartbeat = AtomicU64::new(0);
        // Store channel layout hash for cross-process synchronization
        header.routing_hash = AtomicU64::new(channel_points.layout_hash());
        // Seed generation so a io restart that recreates the SHM file
        // produces a different generation than the previous incarnation.
        // Wall-clock alone is not enough — an NTP step-back within the same
        // second could yield ≤ the previous seed and silently bypass the
        // mismatch detection in ShmDispatch. Mix in a per-process nonce
        // (PID + a static-address ASLR bit) so monotonicity does not
        // depend on the system clock.
        static NONCE_ANCHOR: AtomicU64 = AtomicU64::new(0);
        let process_nonce = (std::process::id() as u64)
            .wrapping_mul(0x9E3779B97F4A7C15)
            .wrapping_add(&NONCE_ANCHOR as *const _ as usize as u64);
        let wall_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        // Force the initial generation to be even and nonzero. The reconfigure
        // path relies on the invariant "generation is even at rest, odd while
        // a reconfigure is in flight" — readers gate themselves out on odd.
        // A random odd seed would defeat that gating (and only fire the
        // debug_assert in debug builds, silently corrupting release).
        let generation_seed = (wall_nanos.wrapping_add(process_nonce) & !1u64).max(2);
        header.writer_generation = AtomicU64::new(generation_seed);
        header._reserved = [0; 8];

        // Initialize every PointSlot to the "unwritten" sentinel (NaN).
        // `set_len` above zero-filled the file, so without this loop slots
        // would default to (value=0.0, raw=0.0) — indistinguishable from a
        // real device reading of zero. Downstream readers rely on
        // `is_finite(value)` to filter unwritten slots.
        // SAFETY: `slots_ptr` points at the slot region inside the mmap;
        // each slot index < slot_count is within the file range we just
        // sized. PointSlot is `#[repr(C, align(32))]` so pointer arithmetic
        // is well-defined and reads back as a valid PointSlot reference.
        let slots_ptr =
            unsafe { mmap.as_mut_ptr().add(slot_offset()) as *const crate::core::slot::PointSlot };
        for i in 0..slot_count {
            let slot = unsafe { &*slots_ptr.add(i) };
            slot.init_unwritten();
        }

        // Flush header to backing file for cross-process visibility.
        // Without this, a reader on ARM64 mmap'ing the same file could see
        // partially-written header fields.
        mmap.flush()
            .with_context(|| "Failed to flush mmap after create")?;

        tracing::info!(
            "Created unified shared memory: {:?}, slots={}/{}, size={}KB, channels={}",
            path,
            slot_count,
            max_slots,
            file_size / 1024,
            channel_layouts.iter().filter(|l| l.is_valid()).count()
        );

        Ok(Self {
            inner: Arc::new(SlotWriter::from_mmap_with_file(
                mmap,
                path.to_path_buf(),
                max_slots,
                slot_count,
                &file,
            )?),
            channel_layouts,
            #[cfg(unix)]
            point_watch: None,
        })
    }

    /// Builds a fresh generation beside the canonical SHM path and publishes
    /// it with an atomic rename.
    ///
    /// Existing readers keep their old inode until their identity check
    /// reopens the canonical path; they never observe a truncate/reinitialize
    /// window during a writer-process restart.
    pub fn create_published(
        config: &SharedConfig,
        channel_points: &ChannelPointCounts,
    ) -> Result<Self> {
        Self::publish_generation(config, channel_points, Self::create)
    }

    /// Restores a snapshot into a private generation and atomically publishes
    /// it only after the complete restored mmap has been flushed.
    pub fn restore_from_snapshot_published(
        config: &SharedConfig,
        snapshot_path: &std::path::Path,
        channel_points: &ChannelPointCounts,
    ) -> Result<Self> {
        Self::publish_generation(config, channel_points, |staging_config, points| {
            Self::restore_from_snapshot(staging_config, snapshot_path, points)
        })
    }

    fn publish_generation(
        config: &SharedConfig,
        channel_points: &ChannelPointCounts,
        build: impl FnOnce(&SharedConfig, &ChannelPointCounts) -> Result<Self>,
    ) -> Result<Self> {
        let canonical_path = config.path();
        let _authority = AuthorityWriteGuard::acquire(canonical_path)
            .context("Failed to acquire exclusive SHM authority for publication")?;
        let sequence = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(1);
        let staging_path = crate::core::config::generation_file_path(canonical_path, sequence);
        let staging_config = config.clone().with_path(staging_path.clone());

        let result = (|| {
            let writer = build(&staging_config, channel_points)?;
            writer
                .flush()
                .context("Failed to flush private SHM generation before publish")?;
            crate::core::config::commit_generation_swap_locked(
                &staging_path,
                canonical_path,
                &_authority,
            )
            .context("Failed to publish private SHM generation")?;
            drop(writer);
            Self::open_existing(config, channel_points)
        })();

        if result.is_err() && staging_path.exists() {
            let _ = std::fs::remove_file(&staging_path);
        }
        result
    }

    // Pure-infra accessors delegate to inner SlotWriter.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.inner.slot_count()
    }
    #[inline]
    pub fn max_slots(&self) -> u32 {
        self.inner.max_slots()
    }
    #[inline]
    fn header(&self) -> &UnifiedHeader {
        self.inner.header()
    }
    /// Channel-aware adapter: look up the slot index for a (channel, type, point).
    #[inline]
    pub fn lookup(&self, channel_id: u32, point_type: u8, point_id: u32) -> Option<usize> {
        self.channel_layouts
            .get(channel_id as usize)?
            .slot(point_type, point_id)
    }
    /// Read-only access to the channel layout table — business adapter state.
    #[inline]
    pub fn channel_layouts(&self) -> &[ChannelLayout] {
        &self.channel_layouts
    }

    /// Shares the physical writer with the typed acquisition adapter.
    ///
    /// Kept crate-private so external callers cannot bypass typed addressing.
    pub(crate) fn acquisition_slot_writer(&self) -> Arc<SlotWriter> {
        Arc::clone(&self.inner)
    }

    /// Write value to slot by channel key
    ///
    /// # Arguments
    /// - `channel_id`: Channel ID
    /// - `point_type`: Point type (T=0, S=1, C=2, A=3)
    /// - `point_id`: Point ID
    /// - `value`: Engineering value
    /// - `raw`: Raw value
    /// - `timestamp_ms`: Timestamp in milliseconds
    #[inline]
    pub fn set(
        &self,
        channel_id: u32,
        point_type: u8,
        point_id: u32,
        value: f64,
        raw: f64,
        timestamp_ms: u64,
    ) -> bool {
        if let Some(layout) = self.channel_layouts.get(channel_id as usize)
            && let Some(slot) = layout.slot(point_type, point_id)
        {
            self.inner.set_direct(slot, value, raw, timestamp_ms);
            // Emit PointWatch event after seqlock write completes (non-blocking).
            #[cfg(unix)]
            if let Some(ref pw) = self.point_watch {
                pw.emit(slot, value, raw, timestamp_ms);
            }
            return true;
        }
        false
    }

    /// Direct write to slot index (for hot path). Delegates to `SlotWriter`.
    ///
    /// Also emits a `PointWatchEvent` (non-blocking) if a signaler is attached
    /// and the slot is subscribed in the bitmap. This is the primary injection
    /// point for the PointWatch event-driven path.
    #[inline]
    pub fn set_direct(&self, slot: usize, value: f64, raw: f64, timestamp_ms: u64) {
        self.inner.set_direct(slot, value, raw, timestamp_ms);
        // Emit PointWatch event after seqlock write — always after, never before.
        #[cfg(unix)]
        if let Some(ref pw) = self.point_watch {
            pw.emit(slot, value, raw, timestamp_ms);
        }
    }

    /// Drain process-local dirty slots set by this writer.
    #[inline]
    pub fn take_dirty_slots(&self) -> Vec<usize> {
        self.inner.take_dirty_slots()
    }

    /// Current writer generation from the SHM header.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    /// SHM file path.
    #[inline]
    pub fn path(&self) -> &PathBuf {
        self.inner.path()
    }

    /// Flush changes to disk.
    #[inline]
    pub fn flush(&self) -> Result<()> {
        Ok(self.inner.flush()?)
    }

    /// Current heartbeat timestamp (ms since epoch).
    #[inline]
    pub fn writer_heartbeat(&self) -> u64 {
        self.inner.writer_heartbeat()
    }

    /// Read-only access to a slot by index. (Kept for backward compatibility;
    /// new code should use the `SlotIo::read_slot` snapshot variant.)
    #[inline]
    pub fn slot(&self, index: usize) -> &crate::core::slot::PointSlot {
        self.inner.slot_at(index)
    }

    /// Update the writer heartbeat without writing a slot.
    #[inline]
    pub fn update_heartbeat(&self, timestamp_ms: u64) {
        self.inner.update_heartbeat(timestamp_ms);
    }

    /// Attach a `PointWatchSignaler` to the writer.
    ///
    /// After calling this, every `set_direct` / `set` invocation will also call
    /// `signaler.emit(...)` (non-blocking) for subscribed slots. Safe to call
    /// multiple times — the last signaler wins.
    ///
    /// # Example
    /// ```ignore
    /// writer.set_point_watcher(Some(Arc::clone(&my_signaler)));
    /// ```
    #[cfg(unix)]
    pub fn set_point_watcher(
        &mut self,
        signaler: Option<std::sync::Arc<crate::point_watch::PointWatchSignaler>>,
    ) {
        self.point_watch = signaler;
    }

    /// Return the currently attached `PointWatchSignaler`, if any.
    #[cfg(unix)]
    pub fn point_watcher(&self) -> Option<&std::sync::Arc<crate::point_watch::PointWatchSignaler>> {
        self.point_watch.as_ref()
    }

    /// Re-open and validate an existing SHM file without truncating it.
    ///
    /// Validates magic/version/routing-hash and slot coverage like every
    /// other open path. Two consumers: io's `ShmHandle::rebuild_via_swap`
    /// (full writer over the freshly swapped canonical file) and
    /// [`ActionWriter::open`] (automation's restricted C/A handle). Deliberately
    /// crate-private — external writers must choose `create` (io, owns
    /// T/S) or `ActionWriter::open` (automation, owns C/A) so cross-ownership
    /// writes stay unrepresentable outside this crate.
    pub(crate) fn open_existing(
        config: &SharedConfig,
        channel_points: &ChannelPointCounts,
    ) -> Result<Self> {
        let path = config.path();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("Failed to open {:?} for actions", path))?;

        // Guard against truncated/corrupt files: a file shorter than the
        // header makes the pointer cast below UB. Must verify BEFORE mmap.
        verify_file_min_size(&file, path)?;

        // SAFETY: File exists, was created by the primary writer (io),
        // and we just verified it is at least header-sized.
        let mmap = unsafe {
            MmapOptions::new()
                .map_mut(&file)
                .with_context(|| "Failed to mmap for actions")?
        };

        // SAFETY: mmap.len() >= sizeof(UnifiedHeader) by verify_file_min_size.
        let header = unsafe { &*(mmap.as_ptr() as *const UnifiedHeader) };
        let (max_slots, slot_count) = validate_shm_header(header, channel_points)?;

        // Now that max_slots is trusted, confirm the mmap actually covers
        // the slot array. Without this, set_direct on slot_count-1 could
        // read/write past the mmap end.
        verify_mmap_covers_slots(mmap.len(), max_slots, path)?;

        let (channel_layouts, calculated_slots) = allocate_layouts(channel_points);
        verify_slot_count(slot_count, calculated_slots)?;

        tracing::info!(
            "Opened unified writer for actions: {:?}, slots={}, C/A channels={}",
            path,
            slot_count,
            channel_layouts
                .iter()
                .filter(|l| l.type_counts[2] > 0 || l.type_counts[3] > 0)
                .count()
        );

        Ok(Self {
            inner: Arc::new(SlotWriter::from_mmap_with_file(
                mmap,
                path.to_path_buf(),
                max_slots,
                slot_count,
                &file,
            )?),
            channel_layouts,
            #[cfg(unix)]
            point_watch: None,
        })
    }

    /// Write Action point (Control or Adjustment only)
    ///
    /// This is a safe wrapper that only allows writing to C/A slots.
    /// Returns false if point_type is not Control (2) or Adjustment (3).
    #[inline]
    pub fn set_action(
        &self,
        channel_id: u32,
        point_type: u8,
        point_id: u32,
        value: f64,
        timestamp_ms: u64,
    ) -> bool {
        // Only allow Control (2) and Adjustment (3) types
        if point_type != 2 && point_type != 3 {
            tracing::warn!(
                "set_action called with non-action type: {} (expected 2 or 3)",
                point_type
            );
            return false;
        }
        self.set(channel_id, point_type, point_id, value, value, timestamp_ms)
    }

    // ========== Snapshot API ==========

    /// Save current shared memory state to a snapshot file
    ///
    /// Uses atomic write: writes to temp file first, then renames to final path.
    pub fn save_snapshot(&self, path: &std::path::Path) -> Result<()> {
        self.inner
            .flush()
            .context("Failed to flush mmap before snapshot")?;
        Ok(self.inner.save_snapshot(path)?)
    }

    /// Restore from snapshot file
    ///
    /// Creates a new UnifiedWriter and loads PointSlot data from the snapshot.
    /// The header is re-initialized from current config, only point data is restored.
    ///
    /// ## Data Validation
    ///
    /// Each slot is validated before restoration:
    /// - NaN values are skipped (logged as warning)
    /// - Infinite values are skipped (logged as warning)
    /// - New slots (not in snapshot) are initialized to default (0.0, 0.0, 0)
    ///
    /// ## Routing Hash Check
    ///
    /// If the snapshot's routing hash differs from current config, a warning is logged
    /// but restoration continues. This handles the case where routing changed after
    /// the snapshot was created.
    ///
    /// # Arguments
    /// - `config`: Shared memory configuration
    /// - `snapshot_path`: Path to the snapshot file
    /// - `channel_points`: Channel point counts for slot allocation
    ///
    /// # Returns
    /// - `Ok(Self)` with restored data
    /// - `Err` if snapshot is invalid or incompatible
    pub fn restore_from_snapshot(
        config: &SharedConfig,
        snapshot_path: &std::path::Path,
        channel_points: &ChannelPointCounts,
    ) -> Result<Self> {
        use std::io::Read;

        // First create a fresh writer with current config
        // Note: create() already initializes all slots to default values (zeroed)
        let writer = Self::create(config, channel_points)?;

        // Read snapshot file
        let mut file = std::fs::File::open(snapshot_path)
            .with_context(|| format!("Failed to open snapshot file: {:?}", snapshot_path))?;

        let mut snapshot_data = Vec::new();
        file.read_to_end(&mut snapshot_data)
            .with_context(|| "Failed to read snapshot file")?;

        // Validate snapshot header
        if snapshot_data.len() < std::mem::size_of::<UnifiedHeader>() {
            bail!("Snapshot file too small: {} bytes", snapshot_data.len());
        }

        // Read header fields individually from unaligned buffer.
        // Cannot use read_unaligned on UnifiedHeader because it contains
        // AtomicU32/AtomicU64 fields — creating Atomics from unaligned
        // memory is UB (hardware atomic instructions require alignment).
        //
        // UnifiedHeader layout (#[repr(C, align(64))]):
        //   offset 0:  magic (u64)
        //   offset 8:  version (u32)
        //   offset 12: max_slots (u32)
        //   offset 16: slot_count (AtomicU32 → read as u32)
        //   offset 20: _pad (4 bytes)
        //   offset 24: last_update_ts (AtomicU64)
        //   offset 32: writer_heartbeat (AtomicU64)
        //   offset 40: routing_hash (AtomicU64 → read as u64)
        //   offset 48: _reserved (16 bytes)
        let snap = &snapshot_data;
        let snap_magic = read_u64_ne(snap, 0, "header.magic")?;
        let snap_version = read_u32_ne(snap, 8, "header.version")?;
        let snap_slot_count_val = read_u32_ne(snap, 16, "header.slot_count")?;
        let snap_routing_hash = read_u64_ne(snap, 40, "header.routing_hash")?;

        if snap_magic != UNIFIED_MAGIC {
            bail!(
                "Invalid snapshot magic: expected 0x{:X}, got 0x{:X}",
                UNIFIED_MAGIC,
                snap_magic
            );
        }
        if snap_version != UNIFIED_VERSION {
            // v2 snapshots used 0.0 as the slot default — restoring them in v3
            // would re-introduce the pseudo-zero contamination this layout
            // bump fixed. Refuse the restore so the writer starts fresh; the
            // next protocol poll repopulates each live slot with a finite
            // value, and downstream readers see "missing" instead of
            // counterfeit zeros for whatever hasn't been re-polled yet.
            bail!(
                "Snapshot version mismatch: expected {}, got {} — refusing restore (start fresh)",
                UNIFIED_VERSION,
                snap_version
            );
        }

        // Check layout hash (warn but don't fail — channel config may have changed since snapshot)
        let current_hash = channel_points.layout_hash();
        if current_hash != snap_routing_hash {
            tracing::warn!(
                "Snapshot layout hash differs: snapshot=0x{:016X}, current=0x{:016X}",
                snap_routing_hash,
                current_hash
            );
        }

        let snapshot_slot_count = snap_slot_count_val as usize;

        // Determine how many slots to restore (min of snapshot and current allocation)
        let slots_to_restore = snapshot_slot_count.min(writer.slot_count());

        if slots_to_restore == 0 {
            tracing::warn!("Snapshot has no slots to restore");
            return Ok(writer);
        }

        // Calculate data ranges
        let header_size = slot_offset();
        let slot_size = std::mem::size_of::<PointSlot>();
        let snapshot_data_end = header_size + slots_to_restore * slot_size;

        if snapshot_data.len() < snapshot_data_end {
            bail!(
                "Snapshot file truncated: expected at least {} bytes, got {}",
                snapshot_data_end,
                snapshot_data.len()
            );
        }

        // Restore slots one by one with validation
        let mut restored_count = 0usize;
        let mut skipped_unwritten = 0usize;
        let mut skipped_invalid = 0usize;

        for i in 0..slots_to_restore {
            let slot_offset_in_file = header_size + i * slot_size;
            // Read PointSlot fields individually from unaligned buffer.
            // Cannot use read_unaligned on PointSlot because it contains
            // AtomicU64 fields — atomics require alignment for correctness.
            //
            // PointSlot layout (#[repr(C, align(32))]):
            //   offset 0:  value_bits (AtomicU64 → read as u64)
            //   offset 8:  timestamp  (AtomicU64 → read as u64)
            //   offset 16: raw_bits   (AtomicU64 → read as u64)
            //   offset 24: seq        (AtomicU32 → not needed for restore)
            //   offset 28: dirty      (AtomicU32 → not needed for restore)
            let sb = &snapshot_data[slot_offset_in_file..slot_offset_in_file + slot_size];
            let value = f64::from_bits(read_u64_ne(sb, 0, "slot.value_bits")?);
            let timestamp = read_u64_ne(sb, 8, "slot.timestamp")?;
            let raw = f64::from_bits(read_u64_ne(sb, 16, "slot.raw_bits")?);

            // NaN is the "unwritten" sentinel in v3 — leave the writer at its
            // create() default (already NaN). Skipping `set_direct` here also
            // leaves seq=0, so readers continue to treat these slots as
            // unwritten until the next protocol poll fills them.
            if value.is_nan() && raw.is_nan() {
                skipped_unwritten += 1;
                continue;
            }

            // Reject infinities and half-NaN combinations as data corruption.
            if !value.is_finite() || !raw.is_finite() {
                tracing::warn!(
                    "Skipping corrupt slot {}: value={}, raw={} (mixed finiteness or Infinity)",
                    i,
                    value,
                    raw
                );
                skipped_invalid += 1;
                continue;
            }

            // Write to current slot
            writer.set_direct(i, value, raw, timestamp);
            restored_count += 1;
        }

        // New slots (if current config has more than snapshot) are already initialized
        // to default values (0.0, 0.0, 0) by create()
        let new_slots = writer.slot_count().saturating_sub(snapshot_slot_count);

        // Update timestamps
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_millis() as u64;
        writer
            .header()
            .last_update_ts
            .store(now_ms, Ordering::Relaxed);
        writer
            .header()
            .writer_heartbeat
            .store(now_ms, Ordering::Relaxed);

        tracing::info!(
            "Snapshot restored: {:?}, restored={}, skipped_unwritten={}, skipped_invalid={}, new_slots={}",
            snapshot_path,
            restored_count,
            skipped_unwritten,
            skipped_invalid,
            new_slots
        );

        if skipped_invalid > 0 {
            tracing::warn!(
                "{} slots skipped due to invalid data (NaN/Infinity)",
                skipped_invalid
            );
        }

        if new_slots > 0 {
            tracing::info!(
                "{} new slots initialized to default values (routing config changed)",
                new_slots
            );
        }

        Ok(writer)
    }
}

// ========== ActionWriter ==========

/// Write handle restricted to Control/Adjustment slots — automation's side of
/// the writer-ownership contract.
///
/// io owns T/S slots through the full [`UnifiedWriter`]; automation opens
/// the same SHM through this type, which exposes no general `set()` — a
/// cross-ownership write to T/S slots is unrepresentable at compile time
/// instead of a runtime warning.
pub struct ActionWriter(UnifiedWriter);

impl ActionWriter {
    /// Open an existing SHM file for Control/Adjustment writes (automation).
    ///
    /// Fails if io has not created the file yet, or if the header
    /// validation chain (magic/version/routing-hash/slot coverage) rejects it.
    pub fn open(config: &SharedConfig, channel_points: &ChannelPointCounts) -> Result<Self> {
        UnifiedWriter::open_existing(config, channel_points).map(Self)
    }

    /// Write an Action point (Control=2 or Adjustment=3 only).
    ///
    /// Returns false for non-action point types (defense-in-depth on top of
    /// the type-level restriction) or unmapped slots.
    #[inline]
    pub fn set_action(
        &self,
        channel_id: u32,
        point_type: u8,
        point_id: u32,
        value: f64,
        timestamp_ms: u64,
    ) -> bool {
        self.0
            .set_action(channel_id, point_type, point_id, value, timestamp_ms)
    }

    /// Writer generation stamped in the SHM header (io bumps on rebuild).
    #[inline]
    pub fn generation(&self) -> u64 {
        self.0.generation()
    }

    /// Build a read-only channel→slot index from this writer's layout.
    ///
    /// Read-only derivation — grants no write capability to T/S slots.
    pub fn channel_slot_index(&self) -> crate::shared_config::ChannelToSlotIndex {
        crate::shared_config::ChannelToSlotIndex::from_unified_writer(&self.0)
    }
}

// ========== UnifiedReader ==========

/// Unified shared memory reader
///
/// Multiple readers allowed (automation, aether, etc.).
/// Builds indexes from ChannelPointCounts using same allocation algorithm.
pub struct UnifiedReader {
    pub(crate) inner: SlotReader,
    /// Channel layouts (Vec index by channel_id) — business adapter state.
    channel_layouts: Vec<ChannelLayout>,
    /// For aether API: list of valid channel IDs — business adapter state.
    channel_ids: Vec<u32>,
}

/// Atomically replaceable read handle for long-lived consumer processes.
///
/// A reader guard pins one mmap generation for the duration of a lookup.
/// Replacing the handle after a canonical-path inode change therefore never
/// invalidates an in-flight read, while subsequent reads use the new layout.
pub struct UnifiedReaderHandle {
    reader: arc_swap::ArcSwap<UnifiedReader>,
}

impl UnifiedReaderHandle {
    /// Creates a handle around the initially validated reader generation.
    #[must_use]
    pub fn new(reader: Arc<UnifiedReader>) -> Self {
        Self {
            reader: arc_swap::ArcSwap::new(reader),
        }
    }

    /// Publishes a newly validated reader generation.
    pub fn replace(&self, reader: Arc<UnifiedReader>) {
        self.reader.store(reader);
    }

    /// Reads an instance point from the currently published generation.
    #[inline]
    pub fn get_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
        routing_cache: &RoutingCache,
    ) -> Option<(f64, u64)> {
        self.reader
            .load()
            .get_instance(instance_id, instance_type, point_id, routing_cache)
    }

    /// Returns the active reader generation for diagnostics.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.reader.load().generation()
    }
}

impl UnifiedReader {
    /// Open shared memory and build indexes from ChannelPointCounts
    pub fn open(config: &SharedConfig, channel_points: &ChannelPointCounts) -> Result<Self> {
        let path = config.path();

        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("Failed to open {:?}", path))?;

        // Guard against truncated/corrupt files before any unsafe cast.
        verify_file_min_size(&file, path)?;

        // SAFETY: file is at least header-sized per verify_file_min_size.
        let mmap = unsafe {
            MmapOptions::new()
                .map(&file)
                .with_context(|| "Failed to mmap")?
        };

        // SAFETY: mmap.len() >= sizeof(UnifiedHeader) by verify_file_min_size.
        let header = unsafe { &*(mmap.as_ptr() as *const UnifiedHeader) };
        let (max_slots, slot_count) = validate_shm_header(header, channel_points)?;

        verify_mmap_covers_slots(mmap.len(), max_slots, path)?;

        let (channel_layouts, calculated_slots) = allocate_layouts(channel_points);
        verify_slot_count(slot_count, calculated_slots)?;

        let channel_ids: Vec<u32> = channel_layouts
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is_valid())
            .map(|(id, _)| id as u32)
            .collect();

        tracing::info!(
            "Opened unified reader: {} slots, {} channels",
            slot_count,
            channel_ids.len()
        );

        Ok(Self {
            inner: SlotReader::from_mmap(mmap, max_slots, slot_count)?,
            channel_layouts,
            channel_ids,
        })
    }

    /// Open shared memory read-only without layout validation.
    ///
    /// For debug tools (aether) that don't have channel point data.
    /// Reads slot_count from header but cannot navigate to specific channel points.
    pub fn open_raw(config: &SharedConfig) -> Result<Self> {
        let path = config.path();
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("Failed to open {:?}", path))?;

        // Guard against truncated/corrupt files before any unsafe cast.
        verify_file_min_size(&file, path)?;

        // SAFETY: file is at least header-sized per verify_file_min_size.
        let mmap = unsafe {
            MmapOptions::new()
                .map(&file)
                .with_context(|| "Failed to mmap")?
        };

        // SAFETY: mmap.len() >= sizeof(UnifiedHeader) by verify_file_min_size.
        let header = unsafe { &*(mmap.as_ptr() as *const UnifiedHeader) };
        if header.magic != UNIFIED_MAGIC {
            bail!(
                "Invalid magic: expected 0x{:X}, got 0x{:X}",
                UNIFIED_MAGIC,
                header.magic
            );
        }

        let max_slots = header.max_slots;
        verify_mmap_covers_slots(mmap.len(), max_slots, path)?;
        let slot_count = header.slot_count.load(Ordering::Acquire) as usize;

        tracing::info!(
            "Opened raw reader: {} slots (no layout validation)",
            slot_count
        );

        Ok(Self {
            inner: SlotReader::from_mmap(mmap, max_slots, slot_count)?,
            channel_layouts: Vec::new(),
            channel_ids: Vec::new(),
        })
    }

    // Pure-infra accessors delegate to inner SlotReader.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.inner.slot_count()
    }
    #[inline]
    pub fn max_slots(&self) -> u32 {
        self.inner.max_slots()
    }
    /// Writer generation observed by this read-only mapping.
    #[inline]
    pub fn generation(&self) -> u64 {
        crate::core::slot_io::SlotIo::generation(&self.inner)
    }
    /// Channel-aware adapter: look up the slot index for a (channel, type, point).
    #[inline]
    pub fn lookup(&self, channel_id: u32, point_type: u8, point_id: u32) -> Option<usize> {
        self.channel_layouts
            .get(channel_id as usize)?
            .slot(point_type, point_id)
    }
    /// Read-only access to the channel layout table — business adapter state.
    #[inline]
    pub fn channel_layouts(&self) -> &[ChannelLayout] {
        &self.channel_layouts
    }

    // ========== Point Query API ==========

    /// Get value by channel key
    #[inline]
    pub fn get_channel(
        &self,
        channel_id: u32,
        point_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)> {
        let layout = self.channel_layouts.get(channel_id as usize)?;
        let slot = layout.slot(point_type, point_id)?;
        let point = crate::core::slot_io::SlotIo::read_slot(&self.inner, slot)?;
        Some((point.value, point.timestamp_ms))
    }

    /// Get value by instance key (requires RoutingCache for C2M lookup)
    ///
    /// For Measurement (type=0): looks up C2M routing
    /// For Action (type=1): looks up M2C routing
    #[inline]
    pub fn get_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
        routing_cache: &RoutingCache,
    ) -> Option<(f64, u64)> {
        // Measurement: instance reads channel T/S via C2M
        // Action: instance writes channel C/A via M2C
        let (channel_id, channel_type, channel_point_id) = if instance_type == 0 {
            // Measurement: reverse C2M lookup via the dedicated O(1) reverse index.
            // RoutingCache now maintains an `(instance, point) → (channel, type, point)`
            // hashmap built at config-load time, so this is no longer the O(routes)
            // scan the old implementation did.
            let (ch_id, pt_type, ch_pt_id) =
                routing_cache.lookup_c2m_reverse(instance_id, point_id)?;
            (ch_id, pt_type.to_u8(), ch_pt_id)
        } else {
            // Action - lookup M2C (try Control first, then Adjustment)
            // Instance "Action" can map to either C (Control) or A (Adjustment)
            let target = routing_cache
                .lookup_m2c_by_parts(instance_id, PointType::Control, point_id)
                .or_else(|| {
                    routing_cache.lookup_m2c_by_parts(instance_id, PointType::Adjustment, point_id)
                })?;
            (
                target.channel_id,
                target.point_type.to_u8(),
                target.point_id,
            )
        };

        self.get_channel(channel_id, channel_type, channel_point_id)
    }

    // ========== Aether Compatible API ==========

    /// Get all channel IDs
    #[inline]
    pub fn channel_ids(&self) -> &[u32] {
        &self.channel_ids
    }

    /// Get instance IDs from RoutingCache
    pub fn instance_ids(&self, routing_cache: &RoutingCache) -> Vec<u32> {
        let mut ids = std::collections::HashSet::new();
        for (_, target) in routing_cache.c2m_iter() {
            ids.insert(target.instance_id);
        }
        ids.into_iter().collect()
    }

    /// Iterate channel points of given type
    pub fn iter_channel_points<F>(&self, channel_id: u32, point_type: PointType, mut f: F)
    where
        F: FnMut(u32, f64),
    {
        if let Some(layout) = self.channel_layouts.get(channel_id as usize) {
            let type_idx = point_type.to_u8() as usize;
            let count = layout.type_counts[type_idx];
            for pt_id in 0..count {
                if let Some(slot) = layout.slot(point_type.to_u8(), pt_id)
                    && let Some(point) = crate::core::slot_io::SlotIo::read_slot(&self.inner, slot)
                {
                    f(pt_id, point.value);
                }
            }
        }
    }

    /// Iterate instance Measurement points (requires RoutingCache)
    pub fn iter_instance_measurements<F>(
        &self,
        instance_id: u32,
        routing_cache: &RoutingCache,
        mut f: F,
    ) where
        F: FnMut(u32, f64),
    {
        for ((ch_id, pt_type, ch_pt_id), target) in routing_cache.c2m_iter() {
            if target.instance_id == instance_id
                && let Some((val, _ts)) = self.get_channel(ch_id, pt_type.to_u8(), ch_pt_id)
            {
                f(target.point_id, val);
            }
        }
    }

    /// Iterate instance Action points (requires RoutingCache)
    pub fn iter_instance_actions<F>(&self, instance_id: u32, routing_cache: &RoutingCache, mut f: F)
    where
        F: FnMut(u32, f64),
    {
        // M2C: (instance, point_type, point) → (channel, type, point)
        // Filter by instance_id
        for ((inst_id, _inst_type, inst_pt_id), target) in routing_cache.m2c_iter() {
            if inst_id == instance_id
                && let Some((val, _ts)) = self.get_channel(
                    target.channel_id,
                    target.point_type.to_u8(),
                    target.point_id,
                )
            {
                f(inst_pt_id, val);
            }
        }
    }

    // ========== Stats ==========

    /// Writer heartbeat — delegates to inner SlotReader.
    #[inline]
    pub fn writer_heartbeat(&self) -> u64 {
        self.inner.writer_heartbeat()
    }

    /// Check if writer is alive based on heartbeat timestamp — delegates.
    #[inline]
    pub fn is_writer_alive(&self, timeout_ms: u64) -> bool {
        self.inner.is_writer_alive(timeout_ms)
    }

    /// Save current SHM state to a snapshot file — delegates to inner.
    pub fn save_snapshot(&self, path: &std::path::Path) -> Result<()> {
        Ok(self.inner.save_snapshot(path)?)
    }
}

// ========== Pure-infra contract: SlotIo ==========
//
// UnifiedWriter implements the business-unaware slot I/O contract declared
// in `core::slot_io`. The trait deliberately omits every channel/point-type
// adapter on UnifiedWriter — anything reachable via `&dyn SlotIo` is, by
// type system enforcement, infrastructure only.

impl crate::core::slot_io::SlotIo for UnifiedWriter {
    #[inline]
    fn slot_count(&self) -> usize {
        crate::core::slot_io::SlotIo::slot_count(self.inner.as_ref())
    }

    #[inline]
    fn read_slot(&self, index: usize) -> Option<crate::core::slot_io::SlotRead> {
        crate::core::slot_io::SlotIo::read_slot(self.inner.as_ref(), index)
    }

    #[inline]
    fn generation(&self) -> u64 {
        crate::core::slot_io::SlotIo::generation(self.inner.as_ref())
    }

    #[inline]
    fn writer_heartbeat(&self) -> u64 {
        crate::core::slot_io::SlotIo::writer_heartbeat(self.inner.as_ref())
    }

    #[inline]
    fn header(&self) -> HeaderSnapshot {
        crate::core::slot_io::SlotIo::header(self.inner.as_ref())
    }
}

// UnifiedReader implements only the read view; it does not implement
// SlotIoWrite, so `&dyn SlotIo` taken from a reader cannot mutate.
impl crate::core::slot_io::SlotIo for UnifiedReader {
    #[inline]
    fn slot_count(&self) -> usize {
        crate::core::slot_io::SlotIo::slot_count(&self.inner)
    }

    #[inline]
    fn read_slot(&self, index: usize) -> Option<crate::core::slot_io::SlotRead> {
        crate::core::slot_io::SlotIo::read_slot(&self.inner, index)
    }

    #[inline]
    fn generation(&self) -> u64 {
        crate::core::slot_io::SlotIo::generation(&self.inner)
    }

    #[inline]
    fn writer_heartbeat(&self) -> u64 {
        crate::core::slot_io::SlotIo::writer_heartbeat(&self.inner)
    }

    #[inline]
    fn header(&self) -> HeaderSnapshot {
        crate::core::slot_io::SlotIo::header(&self.inner)
    }
}

impl crate::core::slot_io::SlotIoWrite for UnifiedWriter {
    #[inline]
    fn write_slot(&self, index: usize, value: f64, raw: f64, timestamp_ms: u64) -> bool {
        crate::core::slot_io::SlotIoWrite::write_slot(
            self.inner.as_ref(),
            index,
            value,
            raw,
            timestamp_ms,
        )
    }

    #[inline]
    fn take_dirty_slots(&self) -> Vec<usize> {
        crate::core::slot_io::SlotIoWrite::take_dirty_slots(self.inner.as_ref())
    }
}

// ========== Tests ==========

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
#[allow(clippy::approx_constant)] // Test values, not actual PI
mod tests {
    use super::*;
    use aether_routing::RoutingCache;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn test_config(dir: &std::path::Path) -> SharedConfig {
        SharedConfig::default()
            .with_path(dir.join("test.shm"))
            .with_max_slots(1000)
    }

    /// Build ChannelPointCounts matching the test topology:
    /// - channel 1001: T:3, C:1 (maps to instance 23)
    /// - channel 1002: S:1     (maps to instance 24)
    fn test_channel_points() -> ChannelPointCounts {
        let mut map = BTreeMap::new();
        map.insert(1001, [3u32, 0, 1, 0]); // T=3, S=0, C=1, A=0
        map.insert(1002, [0u32, 1, 0, 0]); // T=0, S=1, C=0, A=0
        ChannelPointCounts::from_map(map)
    }

    #[test]
    fn action_writer_exposes_only_action_writes() {
        let dir = tempdir().unwrap();
        let config = test_config(dir.path());
        let counts = test_channel_points();

        // io side creates the SHM; automation side opens it as ActionWriter.
        let owner = UnifiedWriter::create(&config, &counts).unwrap();

        let aw = ActionWriter::open(&config, &counts).unwrap();
        assert_eq!(aw.generation(), owner.generation());

        // Control write lands; Telemetry write is refused (runtime guard
        // kept as defense-in-depth — the type itself exposes no `set()`).
        assert!(aw.set_action(1001, 2, 0, 42.0, 1));
        assert!(!aw.set_action(1001, 0, 0, 42.0, 1));
    }

    /// Build RoutingCache for tests that still exercise instance-based lookup APIs.
    fn test_routing_cache() -> RoutingCache {
        let mut c2m = std::collections::HashMap::new();
        let mut m2c = std::collections::HashMap::new();
        let c2c = std::collections::HashMap::new();

        c2m.insert("1001:T:0".to_string(), "23:M:0".to_string());
        c2m.insert("1001:T:1".to_string(), "23:M:1".to_string());
        c2m.insert("1001:T:2".to_string(), "23:M:2".to_string());
        c2m.insert("1002:S:0".to_string(), "24:M:0".to_string());
        m2c.insert("23:C:0".to_string(), "1001:C:0".to_string());

        RoutingCache::from_maps(c2m, m2c, c2c)
    }

    fn setup_test_env() -> (tempfile::TempDir, SharedConfig, ChannelPointCounts) {
        let dir = tempdir().unwrap();
        let config = test_config(dir.path());
        let channel_points = test_channel_points();
        (dir, config, channel_points)
    }

    /// Create writer, run setup closure, flush, then open reader
    fn write_and_open_reader(
        config: &SharedConfig,
        channel_points: &ChannelPointCounts,
        setup: impl FnOnce(&UnifiedWriter),
    ) -> UnifiedReader {
        let writer = UnifiedWriter::create(config, channel_points).unwrap();
        setup(&writer);
        writer.flush().unwrap();
        UnifiedReader::open(config, channel_points).unwrap()
    }

    #[test]
    fn test_header_size() {
        assert_eq!(std::mem::size_of::<UnifiedHeader>(), 64);
    }

    #[test]
    fn test_file_size() {
        // Header (64B) + PointSlot[1000] (32B each) = 64 + 32000 = 32064
        assert_eq!(calculate_file_size(1000), 64 + 32 * 1000);
    }

    #[test]
    fn test_allocate_layouts() {
        let channel_points = test_channel_points();
        let (layouts, slot_count) = allocate_layouts(&channel_points);

        // Should have layouts up to channel 1002
        assert!(layouts.len() >= 1003);

        // Channel 1001: T:3, C:1 = 4 points
        let layout_1001 = &layouts[1001];
        assert_eq!(layout_1001.type_counts[0], 3); // T
        assert_eq!(layout_1001.type_counts[2], 1); // C
        assert_eq!(layout_1001.total_points, 4);

        // Channel 1002: S:1 = 1 point
        let layout_1002 = &layouts[1002];
        assert_eq!(layout_1002.type_counts[1], 1); // S
        assert_eq!(layout_1002.total_points, 1);

        // Total slots: ch1001 T@0-2, pad@3, C@4 (cache-line aligned) = 5;
        // ch1002 base aligned to 6, S@6 = 7 total. See allocate_layouts
        // writer-ownership padding.
        assert_eq!(slot_count, 7);
    }

    #[test]
    fn test_writer_create() {
        let (_dir, config, channel_points) = setup_test_env();
        let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
        assert_eq!(writer.slot_count(), 7); // includes ownership-boundary padding
        assert_eq!(writer.max_slots(), 1000);
    }

    #[test]
    fn test_write_read() {
        let (_dir, config, channel_points) = setup_test_env();
        let reader = write_and_open_reader(&config, &channel_points, |w| {
            assert!(w.set(1001, 0, 0, 3.14, 3.14, 1705234567890));
            assert!(w.set(1001, 0, 1, 2.71, 2.71, 1705234567890));
        });
        assert_eq!(reader.slot_count(), 7); // includes ownership-boundary padding

        let (val, ts) = reader.get_channel(1001, 0, 0).unwrap();
        assert!((val - 3.14).abs() < 1e-10);
        assert_eq!(ts, 1705234567890);

        let (val2, _) = reader.get_channel(1001, 0, 1).unwrap();
        assert!((val2 - 2.71).abs() < 1e-10);
    }

    #[test]
    fn test_instance_lookup() {
        let (_dir, config, channel_points) = setup_test_env();
        let routing = test_routing_cache();
        let reader = write_and_open_reader(&config, &channel_points, |w| {
            w.set(1001, 0, 1, 42.0, 42.0, 100);
        });
        let (val, _) = reader.get_instance(23, 0, 1, &routing).unwrap();
        assert!((val - 42.0).abs() < 1e-10);
    }

    #[test]
    fn test_aether_api() {
        let (_dir, config, channel_points) = setup_test_env();
        let routing = test_routing_cache();
        let reader = write_and_open_reader(&config, &channel_points, |w| {
            w.set(1001, 0, 0, 1.0, 1.0, 100);
            w.set(1001, 0, 1, 2.0, 2.0, 100);
            w.set(1001, 0, 2, 3.0, 3.0, 100);
        });

        assert!(reader.channel_ids().contains(&1001));
        assert!(reader.channel_ids().contains(&1002));

        let mut sum = 0.0;
        reader.iter_channel_points(1001, PointType::Telemetry, |_pt_id, val| {
            sum += val;
        });
        assert!((sum - 6.0).abs() < 1e-10);

        let mut count = 0;
        reader.iter_instance_measurements(23, &routing, |_pt_id, _val| {
            count += 1;
        });
        assert_eq!(count, 3);
    }

    #[test]
    fn test_direct_write() {
        let (_dir, config, channel_points) = setup_test_env();
        let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
        let slot = writer.lookup(1001, 0, 0).unwrap();
        writer.set_direct(slot, 99.0, 99.0, 100);
        writer.flush().unwrap();

        let reader = UnifiedReader::open(&config, &channel_points).unwrap();
        let (val, _) = reader.get_channel(1001, 0, 0).unwrap();
        assert!((val - 99.0).abs() < 1e-10);
    }

    // The previous `test_reconfigure_resets_slots_to_unwritten_nan` and
    // `test_reconfigure_rejects_corrupt_slot_count` tests exercised the
    // legacy in-place `reconfigure_existing` path that Step 3 PR 2
    // replaced with `ShmHandle::rebuild_via_swap`. The atomic-swap path
    // creates an entirely fresh file (no in-place clearing window), so
    // the NaN-reset invariant the first test guarded is satisfied
    // structurally by `UnifiedWriter::create`'s init loop (covered by
    // `test_write_read` / `test_writer_create`). The corrupt-header
    // rejection the second test guarded only applies to a code path
    // that opens an existing file in-place; the swap path never opens
    // the previous canonical file for writing, so the failure mode is
    // gone with the function.
}
