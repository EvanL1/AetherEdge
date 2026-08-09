//! Raw-pointer access to firmware-owned shared memory.

use core::ptr;

use crate::layout::{
    FIRMWARE_SLOT_SIZE, FirmwarePointSlot, FirmwareShmHeader, firmware_shm_size,
    firmware_slot_offset,
};
use crate::traits::{ShmOps, ShmOpsExt};

/// Raw-pointer access to a fixed firmware shared-memory region.
///
/// # Safety contract
///
/// The constructor's caller owns the lifetime and hardware coherency policy for
/// the region. The region must be aligned to 8 bytes, writable for
/// the checked size returned by [`RawPtrShm::required_size`], and support
/// coherent 8-bit and 32-bit atomic operations between every participating
/// core. Initialization requires exclusive access; after publication, each
/// slot may have at most one writer.
pub struct RawPtrShm {
    base: *mut u8,
    slot_count: u32,
    region_size: usize,
}

impl RawPtrShm {
    /// Create a view over a firmware-provided memory region.
    ///
    /// # Safety
    ///
    /// When `slot_count` is representable and this returns `Some`, `base` must
    /// satisfy the type-level safety contract for the full lifetime of the
    /// value. Creating another view over the same region is safe only when its
    /// accesses obey the same initialization and single-writer rules. No
    /// pointer validity is required when this returns `None`.
    #[inline]
    pub const unsafe fn from_raw(base: *mut u8, slot_count: u32) -> Option<Self> {
        match firmware_shm_size(slot_count) {
            Some(region_size) => Some(Self {
                base,
                slot_count,
                region_size,
            }),
            None => None,
        }
    }

    /// Return the exact region size, or `None` when it cannot fit in `usize`.
    #[inline]
    pub const fn required_size(slot_count: u32) -> Option<usize> {
        firmware_shm_size(slot_count)
    }

    /// Initialize the header and every slot.
    ///
    /// This must run under exclusive access to the memory region.
    pub fn init(&mut self) -> bool {
        // SAFETY: from_raw requires an aligned, writable region of exactly the
        // required size. &mut self and the API contract provide exclusive
        // initialization access.
        unsafe {
            ptr::write(self.header_mut(), FirmwareShmHeader::new(self.slot_count));
            for index in 0..self.slot_count {
                let Some(slot) = self.slot(index) else {
                    return false;
                };
                ptr::write(slot, FirmwarePointSlot::zeroed());
            }
        }
        true
    }

    #[inline]
    fn header(&self) -> *const FirmwareShmHeader {
        self.base.cast()
    }

    #[inline]
    fn header_mut(&mut self) -> *mut FirmwareShmHeader {
        self.base.cast()
    }

    #[inline]
    fn slot(&self, index: u32) -> Option<*mut FirmwarePointSlot> {
        if index >= self.slot_count {
            return None;
        }
        let offset = firmware_slot_offset(index)?;
        let slot_end = offset.checked_add(FIRMWARE_SLOT_SIZE)?;
        if slot_end > self.region_size {
            return None;
        }
        // SAFETY: from_raw guarantees the checked region is accessible and
        // 8-byte aligned; offset and slot_end were checked above.
        Some(unsafe { self.base.add(offset).cast() })
    }

    /// Return whether the region has this firmware ABI and exact slot count.
    pub fn is_valid(&self) -> bool {
        // SAFETY: from_raw guarantees the header is readable and aligned.
        unsafe { (*self.header()).is_valid(self.slot_count) }
    }
}

impl ShmOps for RawPtrShm {
    fn slot_count(&self) -> u32 {
        self.slot_count
    }

    fn is_slot_valid(&self, index: u32) -> bool {
        self.slot(index).is_some_and(|slot| {
            // SAFETY: slot() returns only in-bounds pointers into the region.
            unsafe { (*slot).is_present() }
        })
    }

    fn read_slot(&self, index: u32) -> Option<(f64, u64, u8)> {
        let slot = self.slot(index)?;
        // SAFETY: slot() returns only in-bounds pointers into the region.
        unsafe { (*slot).try_read() }
    }

    fn write_slot(&mut self, index: u32, value: f64, timestamp: u64, quality_code: u8) -> bool {
        let Some(slot) = self.slot(index) else {
            return false;
        };
        // SAFETY: slot() returns only in-bounds pointers. Every concurrently
        // accessed field is atomic; &mut self identifies this logical writer.
        unsafe { (*slot).write(value, timestamp, quality_code) };
        true
    }
}

impl ShmOpsExt for RawPtrShm {
    fn slot_point_id(&self, index: u32) -> Option<u32> {
        let slot = self.slot(index)?;
        // SAFETY: slot() returns only in-bounds pointers into the region.
        unsafe { Some((*slot).point_id()) }
    }

    fn slot_instance_id(&self, index: u32) -> Option<u32> {
        let slot = self.slot(index)?;
        // SAFETY: slot() returns only in-bounds pointers into the region.
        unsafe { Some((*slot).instance_id()) }
    }

    fn slot_point_type(&self, index: u32) -> Option<u8> {
        let slot = self.slot(index)?;
        // SAFETY: slot() returns only in-bounds pointers into the region.
        unsafe { Some((*slot).point_type()) }
    }

    fn set_slot_metadata(
        &mut self,
        index: u32,
        point_id: u32,
        instance_id: u32,
        point_type: u8,
    ) -> bool {
        let Some(slot) = self.slot(index) else {
            return false;
        };
        // SAFETY: slot() returns only in-bounds pointers. Every concurrently
        // accessed field is atomic; &mut self identifies this logical writer.
        unsafe { (*slot).set_metadata(point_id, instance_id, point_type) };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn aligned_region(slot_count: u32) -> Vec<u64> {
        let byte_count = RawPtrShm::required_size(slot_count).expect("representable test region");
        vec![0; byte_count.div_ceil(core::mem::size_of::<u64>())]
    }

    fn raw_view(backing: &mut [u64], slot_count: u32) -> RawPtrShm {
        // SAFETY: Vec<u64> provides 8-byte alignment and aligned_region allocates
        // at least required_size(slot_count) bytes for the test lifetime.
        unsafe { RawPtrShm::from_raw(backing.as_mut_ptr().cast(), slot_count) }
            .expect("representable test view")
    }

    #[test]
    fn init_publishes_firmware_identity_and_exact_capacity() {
        let slot_count = 10;
        let mut backing = aligned_region(slot_count);
        let mut shm = raw_view(&mut backing, slot_count);

        assert!(!shm.is_valid());
        assert!(shm.init());

        assert!(shm.is_valid());
        assert_eq!(shm.slot_count(), slot_count);
        assert_eq!(backing.len() * core::mem::size_of::<u64>(), 336);
    }

    #[test]
    fn read_write_preserves_value_timestamp_and_quality() {
        let slot_count = 10;
        let mut backing = aligned_region(slot_count);
        let mut shm = raw_view(&mut backing, slot_count);
        assert!(shm.init());

        assert_eq!(shm.read_slot(0), None);
        assert!(shm.write_slot(0, 42.5, 1_234_567_890, 7));

        assert_eq!(shm.read_slot(0), Some((42.5, 1_234_567_890, 7)));
        assert!(shm.is_slot_valid(0));
    }

    #[test]
    fn metadata_is_local_to_each_slot() {
        let slot_count = 10;
        let mut backing = aligned_region(slot_count);
        let mut shm = raw_view(&mut backing, slot_count);
        assert!(shm.init());

        assert!(shm.set_slot_metadata(0, 100, 200, 1));

        assert_eq!(shm.slot_point_id(0), Some(100));
        assert_eq!(shm.slot_instance_id(0), Some(200));
        assert_eq!(shm.slot_point_type(0), Some(1));
        assert!(!shm.is_slot_valid(0));
    }

    #[test]
    fn bounds_are_enforced_without_touching_the_region() {
        let slot_count = 10;
        let mut backing = aligned_region(slot_count);
        let mut shm = raw_view(&mut backing, slot_count);
        assert!(shm.init());

        assert!(shm.write_slot(slot_count - 1, 42.5, 123, 0));
        assert!(shm.set_slot_metadata(slot_count - 1, 1, 2, 3));
        assert!(!shm.write_slot(slot_count, 42.5, 123, 0));
        assert!(!shm.set_slot_metadata(slot_count, 1, 2, 3));

        assert_eq!(shm.read_slot(slot_count), None);
        assert_eq!(shm.slot_point_id(slot_count), None);
        assert!(!shm.is_slot_valid(slot_count));
        assert_eq!(shm.read_slot(slot_count - 1), Some((42.5, 123, 0)));
        assert_eq!(shm.slot_point_id(slot_count - 1), Some(1));
    }

    #[test]
    fn a_view_rejects_a_header_with_a_different_capacity() {
        let mut backing = aligned_region(10);
        let mut ten_slots = raw_view(&mut backing, 10);
        assert!(ten_slots.init());
        assert!(ten_slots.is_valid());

        let nine_slots = raw_view(&mut backing, 9);
        assert!(!nine_slots.is_valid());
    }
}
