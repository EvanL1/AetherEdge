//! Firmware-local raw shared-memory ABI.
//!
//! This layout is for memory supplied by an embedded HAL, such as retained or
//! dual-core SRAM. It is independent from the Linux live-state mmap ABI.

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// Eight-byte identity for the Aether firmware shared-memory ABI.
pub const FIRMWARE_SHM_MAGIC: [u8; 8] = *b"AETHFWSM";

/// Current firmware shared-memory ABI version.
pub const FIRMWARE_SHM_VERSION: u32 = 2;

/// Exact byte size of the firmware shared-memory header.
pub const FIRMWARE_HEADER_SIZE: usize = 16;

/// Exact byte size of one firmware point slot.
pub const FIRMWARE_SLOT_SIZE: usize = 32;

/// Largest slot count whose complete region fits in this target's address size.
pub const MAX_FIRMWARE_SLOT_COUNT: u32 = max_slot_count_for_size_limit(usize::MAX);

/// Header at the beginning of a firmware-owned shared-memory region.
#[repr(C, align(8))]
pub(crate) struct FirmwareShmHeader {
    magic: [u8; 8],
    version: u32,
    slot_count: u32,
}

impl FirmwareShmHeader {
    pub(crate) const fn new(slot_count: u32) -> Self {
        Self {
            magic: FIRMWARE_SHM_MAGIC,
            version: FIRMWARE_SHM_VERSION,
            slot_count,
        }
    }

    #[inline]
    pub(crate) fn is_valid(&self, slot_count: u32) -> bool {
        self.magic == FIRMWARE_SHM_MAGIC
            && self.version == FIRMWARE_SHM_VERSION
            && self.slot_count == slot_count
    }
}

const _: () = assert!(core::mem::size_of::<FirmwareShmHeader>() == FIRMWARE_HEADER_SIZE);
const _: () = assert!(core::mem::align_of::<FirmwareShmHeader>() == 8);
const _: () = assert!(core::mem::offset_of!(FirmwareShmHeader, magic) == 0);
const _: () = assert!(core::mem::offset_of!(FirmwareShmHeader, version) == 8);
const _: () = assert!(core::mem::offset_of!(FirmwareShmHeader, slot_count) == 12);

/// One firmware point value and its stable point metadata.
///
/// `sequence` is a seqlock counter: odd while the sole writer is updating the
/// slot and even when readers may copy a consistent value.
#[repr(C, align(8))]
pub(crate) struct FirmwarePointSlot {
    sequence: AtomicU32,
    point_id: AtomicU32,
    instance_id: AtomicU32,
    value_low: AtomicU32,
    value_high: AtomicU32,
    timestamp_low: AtomicU32,
    timestamp_high: AtomicU32,
    quality_code: AtomicU8,
    point_type: AtomicU8,
    present: AtomicU8,
    _reserved: AtomicU8,
}

impl FirmwarePointSlot {
    pub(crate) const fn zeroed() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            point_id: AtomicU32::new(0),
            instance_id: AtomicU32::new(0),
            value_low: AtomicU32::new(0),
            value_high: AtomicU32::new(0),
            timestamp_low: AtomicU32::new(0),
            timestamp_high: AtomicU32::new(0),
            quality_code: AtomicU8::new(0),
            point_type: AtomicU8::new(0),
            present: AtomicU8::new(0),
            _reserved: AtomicU8::new(0),
        }
    }

    #[inline]
    pub(crate) fn is_present(&self) -> bool {
        self.present.load(Ordering::Acquire) != 0
    }

    #[inline]
    fn begin_write(&self) {
        self.sequence.fetch_add(1, Ordering::AcqRel);
        core::sync::atomic::fence(Ordering::Release);
    }

    #[inline]
    fn end_write(&self) {
        core::sync::atomic::fence(Ordering::Release);
        self.sequence.fetch_add(1, Ordering::Release);
    }

    #[inline]
    pub(crate) fn try_read(&self) -> Option<(f64, u64, u8)> {
        let sequence_before = self.sequence.load(Ordering::Acquire);
        if sequence_before & 1 != 0 || self.present.load(Ordering::Relaxed) == 0 {
            return None;
        }

        let value_bits = u64::from(self.value_low.load(Ordering::Relaxed))
            | (u64::from(self.value_high.load(Ordering::Relaxed)) << 32);
        let timestamp = u64::from(self.timestamp_low.load(Ordering::Relaxed))
            | (u64::from(self.timestamp_high.load(Ordering::Relaxed)) << 32);
        let quality_code = self.quality_code.load(Ordering::Relaxed);
        core::sync::atomic::fence(Ordering::Acquire);

        let sequence_after = self.sequence.load(Ordering::Acquire);
        (sequence_before == sequence_after).then_some((
            f64::from_bits(value_bits),
            timestamp,
            quality_code,
        ))
    }

    pub(crate) fn write(&self, value: f64, timestamp: u64, quality_code: u8) {
        let value_bits = value.to_bits();
        self.begin_write();
        self.value_low.store(value_bits as u32, Ordering::Relaxed);
        self.value_high
            .store((value_bits >> 32) as u32, Ordering::Relaxed);
        self.timestamp_low
            .store(timestamp as u32, Ordering::Relaxed);
        self.timestamp_high
            .store((timestamp >> 32) as u32, Ordering::Relaxed);
        self.quality_code.store(quality_code, Ordering::Relaxed);
        self.present.store(1, Ordering::Relaxed);
        self.end_write();
    }

    #[inline]
    pub(crate) fn point_id(&self) -> u32 {
        self.point_id.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn instance_id(&self) -> u32 {
        self.instance_id.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn point_type(&self) -> u8 {
        self.point_type.load(Ordering::Acquire)
    }

    pub(crate) fn set_metadata(&self, point_id: u32, instance_id: u32, point_type: u8) {
        self.begin_write();
        self.point_id.store(point_id, Ordering::Relaxed);
        self.instance_id.store(instance_id, Ordering::Relaxed);
        self.point_type.store(point_type, Ordering::Relaxed);
        self.end_write();
    }
}

const _: () = assert!(core::mem::size_of::<FirmwarePointSlot>() == FIRMWARE_SLOT_SIZE);
const _: () = assert!(core::mem::align_of::<FirmwarePointSlot>() == 8);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, sequence) == 0);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, point_id) == 4);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, instance_id) == 8);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, value_low) == 12);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, value_high) == 16);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, timestamp_low) == 20);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, timestamp_high) == 24);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, quality_code) == 28);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, point_type) == 29);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, present) == 30);
const _: () = assert!(core::mem::offset_of!(FirmwarePointSlot, _reserved) == 31);

const fn max_slot_count_for_size_limit(size_limit: usize) -> u32 {
    if size_limit < FIRMWARE_HEADER_SIZE {
        return 0;
    }

    let addressable_slots = (size_limit - FIRMWARE_HEADER_SIZE) / FIRMWARE_SLOT_SIZE;
    if usize::BITS > u32::BITS && addressable_slots > u32::MAX as usize {
        u32::MAX
    } else {
        addressable_slots as u32
    }
}

const fn firmware_shm_size_with_limit(slot_count: u32, size_limit: usize) -> Option<usize> {
    let slot_bytes = match (slot_count as usize).checked_mul(FIRMWARE_SLOT_SIZE) {
        Some(slot_bytes) => slot_bytes,
        None => return None,
    };
    let region_size = match FIRMWARE_HEADER_SIZE.checked_add(slot_bytes) {
        Some(region_size) => region_size,
        None => return None,
    };
    if region_size > size_limit {
        None
    } else {
        Some(region_size)
    }
}

/// Exact byte size required for `slot_count` firmware point slots.
///
/// Returns `None` when the complete region cannot be represented by this
/// target's `usize`.
#[inline]
pub const fn firmware_shm_size(slot_count: u32) -> Option<usize> {
    firmware_shm_size_with_limit(slot_count, usize::MAX)
}

#[inline]
pub(crate) const fn firmware_slot_offset(slot_index: u32) -> Option<usize> {
    firmware_shm_size(slot_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn firmware_layout_has_its_own_identity_and_exact_sizes() {
        assert_eq!(FIRMWARE_SHM_MAGIC, *b"AETHFWSM");
        assert_eq!(FIRMWARE_SHM_VERSION, 2);
        assert_eq!(core::mem::size_of::<FirmwareShmHeader>(), 16);
        assert_eq!(core::mem::size_of::<FirmwarePointSlot>(), 32);
        assert_eq!(firmware_shm_size(0), Some(16));
        assert_eq!(firmware_shm_size(3), Some(16 + 3 * 32));
        assert_eq!(firmware_slot_offset(2), Some(16 + 2 * 32));
    }

    #[test]
    fn checked_size_accepts_the_limit_capacity_and_rejects_the_next_slot() {
        let simulated_address_limit = u32::MAX as usize;
        let max_slots = max_slot_count_for_size_limit(simulated_address_limit);
        let expected =
            ((simulated_address_limit - FIRMWARE_HEADER_SIZE) / FIRMWARE_SLOT_SIZE) as u32;
        assert_eq!(max_slots, expected);
        assert!(firmware_shm_size_with_limit(max_slots, simulated_address_limit).is_some());
        assert_eq!(
            firmware_shm_size_with_limit(max_slots + 1, simulated_address_limit),
            None
        );

        assert!(firmware_shm_size(MAX_FIRMWARE_SLOT_COUNT).is_some());
        if let Some(unrepresentable_count) = MAX_FIRMWARE_SLOT_COUNT.checked_add(1) {
            assert_eq!(firmware_shm_size(unrepresentable_count), None);
        }
    }

    #[test]
    fn header_rejects_a_different_capacity_or_version() {
        let mut header = FirmwareShmHeader::new(8);
        assert!(header.is_valid(8));
        assert!(!header.is_valid(7));

        header.version += 1;
        assert!(!header.is_valid(8));
    }

    #[test]
    fn point_slot_publishes_presence_and_quality_code() {
        let slot = FirmwarePointSlot::zeroed();
        assert!(!slot.is_present());
        assert_eq!(slot.try_read(), None);

        slot.write(42.5, 123, 7);

        assert!(slot.is_present());
        assert_eq!(slot.try_read(), Some((42.5, 123, 7)));
    }

    #[test]
    fn atomic_split_fields_remain_consistent_during_concurrent_reads() {
        const TIMESTAMP_MASK: u64 = 0xa5a5_5a5a_f0f0_0f0f;
        const WRITES: u64 = 50_000;

        let slot = Arc::new(FirmwarePointSlot::zeroed());
        let writer_slot = Arc::clone(&slot);
        let writer_done = Arc::new(AtomicBool::new(false));
        let writer_done_signal = Arc::clone(&writer_done);

        let writer = thread::spawn(move || {
            for counter in 1..=WRITES {
                writer_slot.write(
                    counter as f64,
                    counter ^ TIMESTAMP_MASK,
                    (counter % 251) as u8,
                );
            }
            writer_done_signal.store(true, Ordering::Release);
        });

        let mut consistent_reads = 0;
        while !writer_done.load(Ordering::Acquire) || consistent_reads < 1_000 {
            let Some((value, encoded_timestamp, quality_code)) = slot.try_read() else {
                core::hint::spin_loop();
                continue;
            };
            let counter = encoded_timestamp ^ TIMESTAMP_MASK;
            assert_eq!(value, counter as f64);
            assert_eq!(quality_code, (counter % 251) as u8);
            consistent_reads += 1;
        }

        writer.join().expect("firmware slot writer");
    }
}
