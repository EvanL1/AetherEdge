//! Raw pointer implementation for embedded platforms.
//!
//! This module provides a zero-allocation shared memory interface
//! suitable for bare-metal or RTOS environments.

use crate::traits::{ShmOps, ShmOpsExt};
use aether_core::shm::{
    HEADER_SIZE, PointSlot, SHM_MAGIC, SHM_VERSION, SLOT_SIZE, ShmHeader, slot_flags,
};
use core::ptr;

/// Raw pointer based shared memory.
///
/// # Safety
///
/// The caller must ensure:
/// - The base pointer points to valid, properly aligned memory
/// - The memory region is at least `shm_size(max_slots)` bytes
/// - The memory is shared between firmware and Linux via hardware mechanism
///
/// # Example
///
/// ```rust,ignore
/// // In embedded firmware:
/// let base: *mut u8 = 0x2000_0000 as *mut u8;  // Shared SRAM region
/// let mut shm = unsafe { RawPtrShm::from_raw(base, 256) };
///
/// // Initialize (only once, by writer)
/// shm.init();
///
/// // Write data
/// let timestamp = get_systick_ms();
/// shm.write_slot(0, 220.5, timestamp, 0);
/// ```
pub struct RawPtrShm {
    /// Base pointer to shared memory region.
    base: *mut u8,
    /// Maximum number of slots.
    max_slots: u32,
}

impl RawPtrShm {
    /// Create from a raw pointer.
    ///
    /// # Safety
    ///
    /// The caller must ensure the pointer is valid and properly sized.
    #[inline]
    pub const unsafe fn from_raw(base: *mut u8, max_slots: u32) -> Self {
        Self { base, max_slots }
    }

    /// Initialize the shared memory region.
    ///
    /// This should be called once by the writer before any read/write operations.
    ///
    /// # Safety
    ///
    /// Must have exclusive access during initialization.
    pub fn init(&mut self) {
        // SAFETY: Caller guarantees exclusive access during initialization (documented in
        // fn-level # Safety). base pointer is valid and sized for shm_size(max_slots).
        // header_mut() returns a properly aligned pointer, and slot_mut() bounds-checks
        // each index against max_slots before computing the offset.
        unsafe {
            // Initialize header
            let header = self.header_mut();
            (*header).init(self.max_slots);

            // Zero all slots
            for i in 0..self.max_slots {
                let slot = self.slot_mut(i);
                ptr::write(slot, PointSlot::zeroed());
            }
        }
    }

    /// Get a pointer to the header.
    #[inline]
    fn header(&self) -> *const ShmHeader {
        self.base as *const ShmHeader
    }

    /// Get a mutable pointer to the header.
    #[inline]
    fn header_mut(&mut self) -> *mut ShmHeader {
        self.base as *mut ShmHeader
    }

    /// Get a pointer to a slot.
    #[inline]
    fn slot(&self, index: u32) -> *const PointSlot {
        if index >= self.max_slots {
            return core::ptr::null();
        }
        // SAFETY: index < max_slots (checked above), so HEADER_SIZE + index * SLOT_SIZE
        // is within the memory region sized to shm_size(max_slots). base is valid per from_raw().
        unsafe { self.base.add(HEADER_SIZE + (index as usize) * SLOT_SIZE) as *const PointSlot }
    }

    /// Get a mutable pointer to a slot.
    #[inline]
    fn slot_mut(&mut self, index: u32) -> *mut PointSlot {
        if index >= self.max_slots {
            return core::ptr::null_mut();
        }
        // SAFETY: index < max_slots (checked above), so offset is within the memory region.
        // base is valid and writable per from_raw() contract.
        unsafe { self.base.add(HEADER_SIZE + (index as usize) * SLOT_SIZE) as *mut PointSlot }
    }

    /// Check if the shared memory is valid (initialized).
    pub fn is_valid(&self) -> bool {
        // SAFETY: base pointer is valid per from_raw() contract. header() returns
        // base cast to *const ShmHeader, which is properly aligned for the memory region.
        unsafe {
            let header = &*self.header();
            header.magic == SHM_MAGIC && header.version == SHM_VERSION
        }
    }
}

// SAFETY: RawPtrShm can be safely sent across threads because:
// - The raw pointer points to a fixed memory region (e.g., shared SRAM)
//   whose lifetime is independent of the RawPtrShm instance
// - The memory region is established at system startup and remains valid
// - No thread-local state is used
unsafe impl Send for RawPtrShm {}

// SAFETY: RawPtrShm can be safely shared between threads because:
// - The shared memory protocol uses atomic-compatible operations for slot access
// - Each slot has independent validity flags checked before read/write
// - The hardware memory region supports concurrent access from firmware and Linux
unsafe impl Sync for RawPtrShm {}

impl ShmOps for RawPtrShm {
    fn slot_count(&self) -> u32 {
        // SAFETY: header() returns base cast to *const ShmHeader; base is valid per from_raw().
        unsafe { (*self.header()).slot_count() }
    }

    fn is_slot_valid(&self, index: u32) -> bool {
        let slot = self.slot(index);
        if slot.is_null() {
            return false;
        }
        // SAFETY: slot is non-null and was bounds-checked by self.slot(index).
        unsafe { (*slot).is_valid() }
    }

    fn read_slot(&self, index: u32) -> Option<(f64, u64, u8)> {
        let slot = self.slot(index);
        if slot.is_null() {
            return None;
        }
        // SAFETY: slot is non-null and within the valid memory region (bounds-checked).
        unsafe { (*slot).try_read() }
    }

    fn read_slot_spin(&self, index: u32) -> (f64, u64, u8) {
        let slot = self.slot(index);
        if slot.is_null() {
            return (0.0, 0, 0);
        }
        // SAFETY: slot is non-null and within the valid memory region (bounds-checked).
        unsafe { (*slot).read_spin() }
    }

    fn write_slot(&mut self, index: u32, value: f64, timestamp: u64, quality: u8) {
        let slot = self.slot_mut(index);
        if slot.is_null() {
            return;
        }
        // SAFETY: slot is non-null and within the valid writable memory region.
        unsafe {
            (*slot).write(value, timestamp, quality);
        }

        // Update header last_update
        // SAFETY: header_mut() points to the base of the valid writable memory region.
        unsafe {
            (*self.header_mut()).set_last_update(timestamp);
        }
    }

    fn last_update(&self) -> u64 {
        // SAFETY: header() returns base cast to *const ShmHeader; base is valid per from_raw().
        unsafe { (*self.header()).last_update() }
    }
}

impl ShmOpsExt for RawPtrShm {
    fn slot_point_id(&self, index: u32) -> Option<u32> {
        let slot = self.slot(index);
        if slot.is_null() {
            return None;
        }
        // SAFETY: slot is non-null and within the valid memory region (bounds-checked).
        unsafe { Some((*slot).point_id) }
    }

    fn slot_instance_id(&self, index: u32) -> Option<u32> {
        let slot = self.slot(index);
        if slot.is_null() {
            return None;
        }
        // SAFETY: slot is non-null and within the valid memory region (bounds-checked).
        unsafe { Some((*slot).instance_id) }
    }

    fn slot_point_type(&self, index: u32) -> Option<u8> {
        let slot = self.slot(index);
        if slot.is_null() {
            return None;
        }
        // SAFETY: slot is non-null and within the valid memory region (bounds-checked).
        unsafe { Some((*slot).point_type) }
    }

    fn set_slot_metadata(&mut self, index: u32, point_id: u32, instance_id: u32, point_type: u8) {
        let slot = self.slot_mut(index);
        if slot.is_null() {
            return;
        }
        // SAFETY: slot is non-null and within the valid writable memory region.
        // Writing individual fields is safe as no concurrent access occurs (single writer).
        unsafe {
            (*slot).point_id = point_id;
            (*slot).instance_id = instance_id;
            (*slot).point_type = point_type;
            (*slot).flags |= slot_flags::VALID;
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use aether_core::shm::shm_size;

    #[test]
    fn test_raw_ptr_shm_init() {
        let max_slots = 10u32;
        let size = shm_size(max_slots);
        let mut buffer = vec![0u8; size];

        let mut shm = unsafe { RawPtrShm::from_raw(buffer.as_mut_ptr(), max_slots) };

        // Before init, should not be valid
        assert!(!shm.is_valid());

        // Initialize
        shm.init();

        // After init, should be valid
        assert!(shm.is_valid());
        assert_eq!(shm.slot_count(), max_slots);
    }

    #[test]
    fn test_raw_ptr_shm_read_write() {
        let max_slots = 10u32;
        let size = shm_size(max_slots);
        let mut buffer = vec![0u8; size];

        let mut shm = unsafe { RawPtrShm::from_raw(buffer.as_mut_ptr(), max_slots) };
        shm.init();

        // Write a value
        shm.write_slot(0, 42.5, 1234567890, 0);

        // Read it back
        let result = shm.read_slot(0);
        assert!(result.is_some());

        let (value, ts, quality) = result.unwrap();
        assert_eq!(value, 42.5);
        assert_eq!(ts, 1234567890);
        assert_eq!(quality, 0);

        // Check last update
        assert_eq!(shm.last_update(), 1234567890);
    }

    #[test]
    fn test_raw_ptr_shm_metadata() {
        let max_slots = 10u32;
        let size = shm_size(max_slots);
        let mut buffer = vec![0u8; size];

        let mut shm = unsafe { RawPtrShm::from_raw(buffer.as_mut_ptr(), max_slots) };
        shm.init();

        // Set metadata
        shm.set_slot_metadata(0, 100, 200, 1);

        // Read metadata
        assert_eq!(shm.slot_point_id(0), Some(100));
        assert_eq!(shm.slot_instance_id(0), Some(200));
        assert_eq!(shm.slot_point_type(0), Some(1));
        assert!(shm.is_slot_valid(0));
    }

    #[test]
    fn test_raw_ptr_shm_bounds() {
        let max_slots = 10u32;
        let size = shm_size(max_slots);
        let mut buffer = vec![0u8; size];

        let mut shm = unsafe { RawPtrShm::from_raw(buffer.as_mut_ptr(), max_slots) };
        shm.init();

        // Out of bounds read should return None
        assert!(shm.read_slot(100).is_none());
        assert!(shm.slot_point_id(100).is_none());

        // Out of bounds write should be a no-op
        shm.write_slot(100, 42.5, 123, 0);
    }
}
