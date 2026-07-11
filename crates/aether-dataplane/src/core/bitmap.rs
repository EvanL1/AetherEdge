//! Business-neutral slot bitmap allocator.
//!
//! Provides dynamic slot allocation and deallocation for the unified slot pool.
//! Uses a bitmap to track which slots are allocated/free.
//!
//! ## Design
//!
//! - **Storage**: Bitmap stored in shared memory, visible to all processes
//! - **Allocation**: First-Fit strategy for contiguous allocation
//! - **Synchronization**: External synchronization required (caller's responsibility)
//!
//! ## Memory Layout
//!
//! ```text
//! SlotBitmapHeader (64 bytes, cache-line aligned):
//! ┌────────────────────────────────────────────────┐
//! │ total_slots: u32     | allocated_count: u32   │
//! │ first_free_hint: u32 | _reserved: [u8; 52]    │
//! └────────────────────────────────────────────────┘
//!
//! Bitmap data (ceil(total_slots / 64) * 8 bytes):
//! ┌────────────────────────────────────────────────┐
//! │ [u64][u64][u64]...                             │
//! │ bit = 1: allocated, bit = 0: free              │
//! └────────────────────────────────────────────────┘
//! ```

use std::sync::atomic::{AtomicU32, Ordering};

/// Bitmap header stored in shared memory
#[repr(C, align(64))]
pub struct SlotBitmapHeader {
    /// Total number of slots in the pool
    pub total_slots: u32,
    /// Number of currently allocated slots
    pub allocated_count: AtomicU32,
    /// Hint for first free slot (optimization)
    pub first_free_hint: AtomicU32,
    /// Reserved for future use
    pub _reserved: [u8; 52],
}

impl SlotBitmapHeader {
    /// Header size in bytes
    pub const SIZE: usize = 64;

    /// Initialize a new header
    pub fn init(&mut self, total_slots: u32) {
        self.total_slots = total_slots;
        self.allocated_count = AtomicU32::new(0);
        self.first_free_hint = AtomicU32::new(0);
        self._reserved = [0; 52];
    }
}

/// Calculate bitmap data size in bytes
pub const fn bitmap_data_size(total_slots: usize) -> usize {
    // Round up to u64 boundary
    total_slots.div_ceil(64) * 8
}

/// Calculate total bitmap size (header + data)
pub const fn total_bitmap_size(total_slots: usize) -> usize {
    SlotBitmapHeader::SIZE + bitmap_data_size(total_slots)
}

/// Slot allocation result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotAllocation {
    /// Starting slot index
    pub base_slot: usize,
    /// Number of slots allocated
    pub count: usize,
}

/// Bitmap view for reading/writing
///
/// This is a view into shared memory, not an owned structure.
/// Caller must ensure the memory is valid and properly synchronized.
pub struct SlotBitmapView<'a> {
    header: &'a SlotBitmapHeader,
    bitmap: &'a mut [u64],
}

impl<'a> SlotBitmapView<'a> {
    /// Create a view from raw memory pointers
    ///
    /// # Safety
    /// - `header_ptr` must point to valid SlotBitmapHeader
    /// - `bitmap_ptr` must point to valid bitmap data of sufficient size
    /// - Memory must remain valid for lifetime 'a
    /// - Caller must ensure exclusive access during mutation
    pub unsafe fn from_raw(header_ptr: *const SlotBitmapHeader, bitmap_ptr: *mut u64) -> Self {
        unsafe {
            // SAFETY: Caller guarantees header_ptr is valid, aligned, and lives for 'a.
            let header = &*header_ptr;
            let word_count = (header.total_slots as usize).div_ceil(64);
            // SAFETY: Caller guarantees bitmap_ptr points to at least word_count u64 values
            // and the memory remains valid and exclusively accessible for lifetime 'a.
            let bitmap = std::slice::from_raw_parts_mut(bitmap_ptr, word_count);
            Self { header, bitmap }
        }
    }

    /// Get total slots
    pub fn total_slots(&self) -> usize {
        self.header.total_slots as usize
    }

    /// Get allocated count
    pub fn allocated_count(&self) -> usize {
        self.header.allocated_count.load(Ordering::Acquire) as usize
    }

    /// Get free count
    pub fn free_count(&self) -> usize {
        self.total_slots() - self.allocated_count()
    }

    /// Check if a slot is allocated
    pub fn is_allocated(&self, slot: usize) -> bool {
        if slot >= self.total_slots() {
            return false;
        }
        let word_idx = slot / 64;
        let bit_idx = slot % 64;
        (self.bitmap[word_idx] & (1u64 << bit_idx)) != 0
    }

    /// Allocate contiguous slots using First-Fit strategy
    ///
    /// Returns None if no contiguous block of sufficient size is available.
    pub fn alloc_contiguous(&mut self, count: usize) -> Option<SlotAllocation> {
        if count == 0 {
            return None;
        }

        let total = self.total_slots();
        if count > total - self.allocated_count() {
            return None; // Not enough free slots
        }

        // Start search from hint
        let hint = self.header.first_free_hint.load(Ordering::Relaxed) as usize;
        let start_pos = hint.min(total.saturating_sub(count));

        // First-Fit: find first contiguous free block
        if let Some(base) = self.find_contiguous_free(start_pos, count) {
            self.mark_allocated(base, count);
            return Some(SlotAllocation {
                base_slot: base,
                count,
            });
        }

        // Wrap around: search from beginning
        if start_pos > 0
            && let Some(base) = self.find_contiguous_free(0, count)
        {
            self.mark_allocated(base, count);
            return Some(SlotAllocation {
                base_slot: base,
                count,
            });
        }

        None
    }

    /// Free previously allocated slots
    ///
    /// # Panics
    /// Debug builds panic if slots were not allocated
    pub fn free(&mut self, base_slot: usize, count: usize) {
        debug_assert!(
            base_slot + count <= self.total_slots(),
            "free: slots out of range"
        );

        for slot in base_slot..base_slot + count {
            let word_idx = slot / 64;
            let bit_idx = slot % 64;

            debug_assert!(
                (self.bitmap[word_idx] & (1u64 << bit_idx)) != 0,
                "free: slot {} was not allocated",
                slot
            );

            self.bitmap[word_idx] &= !(1u64 << bit_idx);
        }

        // Update counts
        self.header
            .allocated_count
            .fetch_sub(count as u32, Ordering::Release);

        // Update hint if freed block is before current hint
        let current_hint = self.header.first_free_hint.load(Ordering::Relaxed) as usize;
        if base_slot < current_hint {
            self.header
                .first_free_hint
                .store(base_slot as u32, Ordering::Relaxed);
        }
    }

    /// Find a contiguous block of free slots starting from `start_pos`
    fn find_contiguous_free(&self, start_pos: usize, count: usize) -> Option<usize> {
        let total = self.total_slots();
        let mut consecutive = 0usize;
        let mut block_start = start_pos;

        for slot in start_pos..total {
            if self.is_allocated(slot) {
                consecutive = 0;
                block_start = slot + 1;
            } else {
                consecutive += 1;
                if consecutive >= count {
                    return Some(block_start);
                }
            }
        }

        None
    }

    /// Mark slots as allocated
    fn mark_allocated(&mut self, base_slot: usize, count: usize) {
        for slot in base_slot..base_slot + count {
            let word_idx = slot / 64;
            let bit_idx = slot % 64;
            self.bitmap[word_idx] |= 1u64 << bit_idx;
        }

        // Update counts
        self.header
            .allocated_count
            .fetch_add(count as u32, Ordering::Release);

        // Update hint: next search starts after this block
        let next_hint = base_slot + count;
        if next_hint < self.total_slots() {
            self.header
                .first_free_hint
                .store(next_hint as u32, Ordering::Relaxed);
        }
    }

    /// Initialize all slots as free
    pub fn init_all_free(&mut self) {
        for word in self.bitmap.iter_mut() {
            *word = 0;
        }
        self.header.allocated_count.store(0, Ordering::Release);
        self.header.first_free_hint.store(0, Ordering::Relaxed);
    }

    /// Get bitmap utilization stats
    pub fn stats(&self) -> BitmapStats {
        let total = self.total_slots();
        let allocated = self.allocated_count();
        BitmapStats {
            total_slots: total,
            allocated_slots: allocated,
            free_slots: total - allocated,
            utilization_pct: if total > 0 {
                (allocated * 100) / total
            } else {
                0
            },
        }
    }
}

/// Point-slot allocation statistics.
#[derive(Debug, Clone, Copy)]
pub struct BitmapStats {
    /// Total number of slots represented by the bitmap.
    pub total_slots: usize,
    /// Number of slots currently allocated.
    pub allocated_slots: usize,
    /// Number of slots currently available.
    pub free_slots: usize,
    /// Integer utilization percentage in the inclusive range `0..=100`.
    pub utilization_pct: usize,
}

/// In-memory bitmap for testing and simple use cases
#[derive(Clone)]
pub struct SlotBitmap {
    total_slots: usize,
    allocated_count: usize,
    first_free_hint: usize,
    bitmap: Vec<u64>,
}

impl SlotBitmap {
    /// Create a new bitmap with all slots free
    pub fn new(total_slots: usize) -> Self {
        let word_count = total_slots.div_ceil(64);
        Self {
            total_slots,
            allocated_count: 0,
            first_free_hint: 0,
            bitmap: vec![0; word_count],
        }
    }

    /// Get total slots
    pub fn total_slots(&self) -> usize {
        self.total_slots
    }

    /// Get allocated count
    pub fn allocated_count(&self) -> usize {
        self.allocated_count
    }

    /// Get free count
    pub fn free_count(&self) -> usize {
        self.total_slots - self.allocated_count
    }

    /// Check if a slot is allocated
    pub fn is_allocated(&self, slot: usize) -> bool {
        if slot >= self.total_slots {
            return false;
        }
        let word_idx = slot / 64;
        let bit_idx = slot % 64;
        (self.bitmap[word_idx] & (1u64 << bit_idx)) != 0
    }

    /// Allocate contiguous slots using First-Fit strategy
    pub fn alloc_contiguous(&mut self, count: usize) -> Option<SlotAllocation> {
        if count == 0 {
            return None;
        }

        if count > self.free_count() {
            return None;
        }

        // Start search from hint
        let start_pos = self
            .first_free_hint
            .min(self.total_slots.saturating_sub(count));

        // First-Fit
        if let Some(base) = self.find_contiguous_free(start_pos, count) {
            self.mark_allocated(base, count);
            return Some(SlotAllocation {
                base_slot: base,
                count,
            });
        }

        // Wrap around
        if start_pos > 0
            && let Some(base) = self.find_contiguous_free(0, count)
        {
            self.mark_allocated(base, count);
            return Some(SlotAllocation {
                base_slot: base,
                count,
            });
        }

        None
    }

    /// Free previously allocated slots
    pub fn free(&mut self, base_slot: usize, count: usize) {
        debug_assert!(
            base_slot + count <= self.total_slots,
            "free: slots out of range"
        );

        for slot in base_slot..base_slot + count {
            let word_idx = slot / 64;
            let bit_idx = slot % 64;
            self.bitmap[word_idx] &= !(1u64 << bit_idx);
        }

        self.allocated_count -= count;

        if base_slot < self.first_free_hint {
            self.first_free_hint = base_slot;
        }
    }

    fn find_contiguous_free(&self, start_pos: usize, count: usize) -> Option<usize> {
        let mut consecutive = 0usize;
        let mut block_start = start_pos;

        for slot in start_pos..self.total_slots {
            if self.is_allocated(slot) {
                consecutive = 0;
                block_start = slot + 1;
            } else {
                consecutive += 1;
                if consecutive >= count {
                    return Some(block_start);
                }
            }
        }

        None
    }

    fn mark_allocated(&mut self, base_slot: usize, count: usize) {
        for slot in base_slot..base_slot + count {
            let word_idx = slot / 64;
            let bit_idx = slot % 64;
            self.bitmap[word_idx] |= 1u64 << bit_idx;
        }

        self.allocated_count += count;

        let next_hint = base_slot + count;
        if next_hint < self.total_slots {
            self.first_free_hint = next_hint;
        }
    }

    /// Get statistics
    pub fn stats(&self) -> BitmapStats {
        BitmapStats {
            total_slots: self.total_slots,
            allocated_slots: self.allocated_count,
            free_slots: self.free_count(),
            utilization_pct: if self.total_slots > 0 {
                (self.allocated_count * 100) / self.total_slots
            } else {
                0
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_new() {
        let bm = SlotBitmap::new(100);
        assert_eq!(bm.total_slots(), 100);
        assert_eq!(bm.allocated_count(), 0);
        assert_eq!(bm.free_count(), 100);
    }

    #[test]
    fn test_bitmap_alloc_single() {
        let mut bm = SlotBitmap::new(100);

        let alloc = bm.alloc_contiguous(10).unwrap();
        assert_eq!(alloc.base_slot, 0);
        assert_eq!(alloc.count, 10);
        assert_eq!(bm.allocated_count(), 10);

        // Slots 0-9 should be allocated
        for i in 0..10 {
            assert!(bm.is_allocated(i));
        }
        for i in 10..100 {
            assert!(!bm.is_allocated(i));
        }
    }

    #[test]
    fn test_bitmap_alloc_multiple() {
        let mut bm = SlotBitmap::new(100);

        let a1 = bm.alloc_contiguous(10).unwrap();
        let a2 = bm.alloc_contiguous(20).unwrap();
        let a3 = bm.alloc_contiguous(5).unwrap();

        assert_eq!(a1.base_slot, 0);
        assert_eq!(a2.base_slot, 10);
        assert_eq!(a3.base_slot, 30);
        assert_eq!(bm.allocated_count(), 35);
    }

    #[test]
    fn test_bitmap_free_and_realloc() {
        let mut bm = SlotBitmap::new(100);

        let a1 = bm.alloc_contiguous(10).unwrap(); // 0-9
        let a2 = bm.alloc_contiguous(10).unwrap(); // 10-19
        let _a3 = bm.alloc_contiguous(10).unwrap(); // 20-29

        // Free middle block
        bm.free(a2.base_slot, a2.count);
        assert_eq!(bm.allocated_count(), 20);
        assert_eq!(bm.free_count(), 80);

        // Reallocate - should fill the gap
        let a4 = bm.alloc_contiguous(10).unwrap();
        assert_eq!(a4.base_slot, 10); // Reused the freed slot

        // Free first block
        bm.free(a1.base_slot, a1.count);

        // Allocate smaller - should use first block
        let a5 = bm.alloc_contiguous(5).unwrap();
        assert_eq!(a5.base_slot, 0);
    }

    #[test]
    fn test_bitmap_alloc_fail_no_space() {
        let mut bm = SlotBitmap::new(100);

        let _a1 = bm.alloc_contiguous(50).unwrap();
        let _a2 = bm.alloc_contiguous(50).unwrap();

        // No space left
        assert!(bm.alloc_contiguous(1).is_none());
    }

    #[test]
    fn test_bitmap_alloc_fail_fragmented() {
        let mut bm = SlotBitmap::new(100);

        // Allocate alternating blocks
        let a1 = bm.alloc_contiguous(30).unwrap(); // 0-29
        let _a2 = bm.alloc_contiguous(30).unwrap(); // 30-59
        let a3 = bm.alloc_contiguous(30).unwrap(); // 60-89
        // Remaining: slots 90-99 (10 slots) are unallocated

        // Free first and third blocks
        bm.free(a1.base_slot, a1.count);
        bm.free(a3.base_slot, a3.count);

        // Free slots: 0-29 (30) + 60-89 (30) + 90-99 (10) = 70 total
        // But max contiguous block is 40 (60-99)
        assert_eq!(bm.free_count(), 70);
        assert!(bm.alloc_contiguous(50).is_none()); // Can't find 50 contiguous (max is 40)
        assert!(bm.alloc_contiguous(40).is_some()); // But 40 works (60-99)
    }

    #[test]
    fn test_bitmap_edge_cases() {
        let mut bm = SlotBitmap::new(64); // Exactly one u64 word

        let a = bm.alloc_contiguous(64).unwrap();
        assert_eq!(a.base_slot, 0);
        assert_eq!(bm.free_count(), 0);

        bm.free(0, 64);
        assert_eq!(bm.free_count(), 64);
    }

    #[test]
    fn test_bitmap_zero_alloc() {
        let mut bm = SlotBitmap::new(100);
        assert!(bm.alloc_contiguous(0).is_none());
    }

    #[test]
    fn test_bitmap_header_size() {
        assert_eq!(std::mem::size_of::<SlotBitmapHeader>(), 64);
    }

    #[test]
    fn test_bitmap_data_size() {
        assert_eq!(bitmap_data_size(1), 8);
        assert_eq!(bitmap_data_size(64), 8);
        assert_eq!(bitmap_data_size(65), 16);
        assert_eq!(bitmap_data_size(128), 16);
        assert_eq!(bitmap_data_size(1000), 128); // ceil(1000/64) * 8 = 16 * 8
    }
}
