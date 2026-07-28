//! Shared memory layout definitions.
//!
//! These structures define the memory layout shared between:
//! - MCU firmware (bare-metal or RTOS)
//! - Linux gateway services (via mmap)
//!
//! ## Memory Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    ShmHeader (64 bytes)                 │
//! │  magic: u64        - Magic number for validation        │
//! │  version: u32      - Layout version                     │
//! │  slot_count: u32   - Number of point slots              │
//! │  last_update: u64  - Last update timestamp (ms)         │
//! │  writer_pid: u32   - Writer process ID (Linux only)     │
//! │  flags: u32        - Status flags                       │
//! │  _reserved: [u8; 32]                                    │
//! ├─────────────────────────────────────────────────────────┤
//! │                 PointSlot[0] (32 bytes)                 │
//! │  point_id: u32     - Point identifier                   │
//! │  instance_id: u32  - Instance/channel identifier        │
//! │  value: f64        - Point value                        │
//! │  timestamp: u64    - Update timestamp (ms)              │
//! │  quality: u8       - Data quality                       │
//! │  point_type: u8    - T/S/C/A                            │
//! │  flags: u8         - Slot flags                         │
//! │  _padding: [u8; 5]                                      │
//! ├─────────────────────────────────────────────────────────┤
//! │                 PointSlot[1] (32 bytes)                 │
//! │                        ...                              │
//! ├─────────────────────────────────────────────────────────┤
//! │                 PointSlot[N-1] (32 bytes)               │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Atomic Operations
//!
//! The `sequence` field in each slot uses atomic operations for lock-free
//! read/write synchronization (seqlock pattern):
//!
//! 1. Writer increments sequence to odd (write in progress)
//! 2. Writer updates value, timestamp, quality
//! 3. Writer increments sequence to even (write complete)
//!
//! Reader spins until sequence is even and unchanged after read.

use core::sync::atomic::{AtomicU32, Ordering};

// AtomicU64 is only available on platforms with native 64-bit atomics
#[cfg(target_has_atomic = "64")]
use core::sync::atomic::AtomicU64;

// For 32-bit platforms without AtomicU64, use a cell-based approach.
// This is safe because embedded firmware typically has a single writer.
#[cfg(not(target_has_atomic = "64"))]
use core::cell::UnsafeCell;

/// A portable 64-bit value that uses atomics when available.
///
/// On 64-bit platforms: uses `AtomicU64` for lock-free access.
/// On 32-bit platforms: uses `UnsafeCell<u64>` (assumes single writer).
#[repr(C)]
pub struct PortableU64 {
    #[cfg(target_has_atomic = "64")]
    inner: AtomicU64,
    #[cfg(not(target_has_atomic = "64"))]
    inner: UnsafeCell<u64>,
}

// Safety: On 32-bit platforms, we assume single-writer semantics.
// The seqlock pattern provides synchronization for PointSlot.
// For ShmHeader, firmware is the sole writer.
#[cfg(not(target_has_atomic = "64"))]
unsafe impl Sync for PortableU64 {}
#[cfg(not(target_has_atomic = "64"))]
unsafe impl Send for PortableU64 {}

impl PortableU64 {
    /// Create a new value.
    #[cfg(target_has_atomic = "64")]
    pub const fn new(val: u64) -> Self {
        Self {
            inner: AtomicU64::new(val),
        }
    }

    #[cfg(not(target_has_atomic = "64"))]
    pub const fn new(val: u64) -> Self {
        Self {
            inner: UnsafeCell::new(val),
        }
    }

    /// Load the value.
    #[cfg(target_has_atomic = "64")]
    #[inline]
    pub fn load(&self, order: Ordering) -> u64 {
        self.inner.load(order)
    }

    #[cfg(not(target_has_atomic = "64"))]
    #[inline]
    pub fn load(&self, _order: Ordering) -> u64 {
        // Safety: single-writer assumption, volatile read for visibility
        unsafe { core::ptr::read_volatile(self.inner.get()) }
    }

    /// Store a value.
    #[cfg(target_has_atomic = "64")]
    #[inline]
    pub fn store(&self, val: u64, order: Ordering) {
        self.inner.store(val, order);
    }

    #[cfg(not(target_has_atomic = "64"))]
    #[inline]
    pub fn store(&self, val: u64, _order: Ordering) {
        // Safety: single-writer assumption, volatile write for visibility
        unsafe { core::ptr::write_volatile(self.inner.get(), val) }
    }
}

/// Magic number for shared memory validation.
/// "VOLT" in little-endian + version nibble.
pub const SHM_MAGIC: u64 = 0x544C_4F56_0001_0000; // "VOLT" + v1.0

/// Current layout version.
pub const SHM_VERSION: u32 = 1;

/// Header size in bytes (64 bytes, cache-line aligned).
pub const HEADER_SIZE: usize = 64;

/// Point slot size in bytes (32 bytes).
pub const SLOT_SIZE: usize = 32;

/// Default maximum number of slots.
pub const DEFAULT_MAX_SLOTS: u32 = 8192;

/// Shared memory header.
///
/// Located at offset 0, contains metadata about the shared memory region.
#[repr(C)]
pub struct ShmHeader {
    /// Magic number for validation (SHM_MAGIC).
    pub magic: u64,
    /// Layout version (SHM_VERSION).
    pub version: u32,
    /// Number of allocated point slots.
    pub slot_count: AtomicU32,
    /// Last update timestamp (Unix milliseconds).
    /// Uses PortableU64 for cross-platform compatibility.
    pub last_update: PortableU64,
    /// Writer process ID (0 if no active writer, Linux only).
    pub writer_pid: AtomicU32,
    /// Status flags.
    pub flags: AtomicU32,
    /// Reserved for future use.
    pub _reserved: [u8; 32],
}

impl ShmHeader {
    /// Check if the header is valid.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.magic == SHM_MAGIC && self.version == SHM_VERSION
    }

    /// Get the number of slots.
    #[inline]
    pub fn slot_count(&self) -> u32 {
        self.slot_count.load(Ordering::Acquire)
    }

    /// Get the last update timestamp.
    #[inline]
    pub fn last_update(&self) -> u64 {
        self.last_update.load(Ordering::Acquire)
    }

    /// Update the last update timestamp.
    #[inline]
    pub fn set_last_update(&self, ts: u64) {
        self.last_update.store(ts, Ordering::Release);
    }

    /// Initialize a new header.
    ///
    /// # Safety
    /// Caller must ensure exclusive access during initialization.
    pub fn init(&mut self, max_slots: u32) {
        self.magic = SHM_MAGIC;
        self.version = SHM_VERSION;
        self.slot_count = AtomicU32::new(0);
        self.last_update = PortableU64::new(0);
        self.writer_pid = AtomicU32::new(0);
        self.flags = AtomicU32::new(0);
        self._reserved = [0u8; 32];
        // Pre-allocate slot count for reader to know bounds
        self.slot_count.store(max_slots, Ordering::Release);
    }
}

// Ensure correct size at compile time
const _: () = assert!(core::mem::size_of::<ShmHeader>() == HEADER_SIZE);

/// Point slot flags.
pub mod slot_flags {
    /// Slot contains valid data.
    pub const VALID: u8 = 0x01;
    /// Value has been updated since last read.
    pub const DIRTY: u8 = 0x02;
    /// Slot is being written (lock flag for simple spinlock).
    pub const WRITING: u8 = 0x04;
}

/// Point data slot.
///
/// Each slot stores one measurement or control point.
/// Uses seqlock pattern for lock-free synchronization.
///
/// Memory layout (32 bytes, optimized for cache and alignment):
/// ```text
/// Offset  Size  Field
///   0      4    sequence (AtomicU32)
///   4      4    point_id
///   8      8    value (f64, 8-byte aligned)
///  16      8    timestamp (u64)
///  24      4    instance_id
///  28      1    quality
///  29      1    point_type
///  30      1    flags
///  31      1    _padding
/// ```
#[repr(C)]
pub struct PointSlot {
    /// Sequence counter for seqlock (odd = write in progress).
    pub sequence: AtomicU32,
    /// Point identifier within the instance.
    pub point_id: u32,
    /// Point value (IEEE 754 double).
    pub value: f64,
    /// Update timestamp (Unix milliseconds).
    pub timestamp: u64,
    /// Instance/channel identifier.
    pub instance_id: u32,
    /// Data quality (see Quality enum).
    pub quality: u8,
    /// Point type (T/S/C/A).
    pub point_type: u8,
    /// Slot flags.
    pub flags: u8,
    /// Padding to 32 bytes.
    pub _padding: u8,
}

impl PointSlot {
    /// Create a zeroed slot.
    pub const fn zeroed() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            point_id: 0,
            value: 0.0,
            timestamp: 0,
            instance_id: 0,
            quality: 0,
            point_type: 0,
            flags: 0,
            _padding: 0,
        }
    }

    /// Check if the slot contains valid data.
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.flags & slot_flags::VALID != 0
    }

    /// Begin a write operation (seqlock pattern).
    ///
    /// Returns the current sequence number (will be odd after this call).
    #[inline]
    pub fn begin_write(&self) -> u32 {
        let seq = self.sequence.fetch_add(1, Ordering::AcqRel);
        core::sync::atomic::fence(Ordering::Release);
        seq + 1
    }

    /// End a write operation (seqlock pattern).
    ///
    /// Increments sequence to even, signaling write complete.
    #[inline]
    pub fn end_write(&self) {
        core::sync::atomic::fence(Ordering::Release);
        self.sequence.fetch_add(1, Ordering::Release);
    }

    /// Read the slot value with seqlock synchronization.
    ///
    /// Returns None if read was interrupted by a write.
    /// Caller should retry in a loop until Some is returned.
    #[inline]
    pub fn try_read(&self) -> Option<(f64, u64, u8)> {
        let seq1 = self.sequence.load(Ordering::Acquire);

        // If sequence is odd, write is in progress
        if seq1 & 1 != 0 {
            return None;
        }

        // Read the data
        core::sync::atomic::fence(Ordering::Acquire);
        let value = self.value;
        let timestamp = self.timestamp;
        let quality = self.quality;
        core::sync::atomic::fence(Ordering::Acquire);

        // Check if sequence changed during read
        let seq2 = self.sequence.load(Ordering::Acquire);
        if seq1 != seq2 {
            return None;
        }

        Some((value, timestamp, quality))
    }

    /// Read the slot value, spinning until successful.
    ///
    /// Use with caution in interrupt contexts; prefer try_read with timeout.
    #[inline]
    pub fn read_spin(&self) -> (f64, u64, u8) {
        loop {
            if let Some(data) = self.try_read() {
                return data;
            }
            core::hint::spin_loop();
        }
    }

    /// Write a value to the slot.
    ///
    /// Uses seqlock pattern for lock-free synchronization.
    pub fn write(&mut self, value: f64, timestamp: u64, quality: u8) {
        self.begin_write();

        // Write data (not atomic, protected by seqlock)
        self.value = value;
        self.timestamp = timestamp;
        self.quality = quality;
        self.flags |= slot_flags::VALID | slot_flags::DIRTY;

        self.end_write();
    }
}

// Ensure correct size at compile time
const _: () = assert!(core::mem::size_of::<PointSlot>() == SLOT_SIZE);

/// Calculate the total size needed for shared memory region.
#[inline]
pub const fn shm_size(slot_count: u32) -> usize {
    HEADER_SIZE + (slot_count as usize) * SLOT_SIZE
}

/// Calculate the offset of a slot from the start of shared memory.
#[inline]
pub const fn slot_offset(slot_index: u32) -> usize {
    HEADER_SIZE + (slot_index as usize) * SLOT_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_size() {
        assert_eq!(core::mem::size_of::<ShmHeader>(), HEADER_SIZE);
        assert_eq!(HEADER_SIZE, 64);
    }

    #[test]
    fn test_slot_size() {
        assert_eq!(core::mem::size_of::<PointSlot>(), SLOT_SIZE);
        assert_eq!(SLOT_SIZE, 32);
    }

    #[test]
    fn test_shm_size_calculation() {
        assert_eq!(shm_size(0), 64);
        assert_eq!(shm_size(1), 64 + 32);
        assert_eq!(shm_size(100), 64 + 100 * 32);
    }

    #[test]
    fn test_slot_offset_calculation() {
        assert_eq!(slot_offset(0), 64);
        assert_eq!(slot_offset(1), 64 + 32);
        assert_eq!(slot_offset(10), 64 + 10 * 32);
    }

    #[test]
    fn test_header_validation() {
        let mut header = ShmHeader {
            magic: 0,
            version: 0,
            slot_count: AtomicU32::new(0),
            last_update: PortableU64::new(0),
            writer_pid: AtomicU32::new(0),
            flags: AtomicU32::new(0),
            _reserved: [0; 32],
        };

        assert!(!header.is_valid());

        header.init(100);
        assert!(header.is_valid());
        assert_eq!(header.slot_count(), 100);
    }

    #[test]
    fn test_slot_read_write() {
        let mut slot = PointSlot::zeroed();

        // Initial state
        assert!(!slot.is_valid());

        // Write a value
        slot.write(42.5, 1234567890, 0);
        assert!(slot.is_valid());

        // Read it back
        let (value, ts, quality) = slot.read_spin();
        assert_eq!(value, 42.5);
        assert_eq!(ts, 1234567890);
        assert_eq!(quality, 0);
    }

    #[test]
    fn test_seqlock_try_read() {
        let slot = PointSlot::zeroed();

        // Sequence is 0 (even), read should succeed
        assert!(slot.try_read().is_some());

        // Simulate write in progress
        slot.sequence.store(1, Ordering::Release);
        assert!(slot.try_read().is_none());

        // Write complete
        slot.sequence.store(2, Ordering::Release);
        assert!(slot.try_read().is_some());
    }
}
