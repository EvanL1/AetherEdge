//! Operations supported by firmware-owned shared memory.

/// Shared memory operations.
///
/// This trait exposes value access without tying firmware code to a particular
/// HAL-provided base address.
pub trait ShmOps {
    /// Get the number of slots.
    fn slot_count(&self) -> u32;

    /// Check if a slot contains valid data.
    fn is_slot_valid(&self, index: u32) -> bool;

    /// Read a slot value.
    ///
    /// Returns `Some((value, timestamp_ms, quality_code))` if successful,
    /// `None` if the read was interrupted or the slot is invalid.
    fn read_slot(&self, index: u32) -> Option<(f64, u64, u8)>;

    /// Write a value to a slot, returning `false` when `index` is out of bounds.
    fn write_slot(&mut self, index: u32, value: f64, timestamp: u64, quality_code: u8) -> bool;
}

/// Extended operations for slot metadata.
pub trait ShmOpsExt: ShmOps {
    /// Get the point ID for a slot.
    fn slot_point_id(&self, index: u32) -> Option<u32>;

    /// Get the instance ID for a slot.
    fn slot_instance_id(&self, index: u32) -> Option<u32>;

    /// Get the point type for a slot.
    fn slot_point_type(&self, index: u32) -> Option<u8>;

    /// Set slot metadata, returning `false` when `index` is out of bounds.
    fn set_slot_metadata(
        &mut self,
        index: u32,
        point_id: u32,
        instance_id: u32,
        point_type: u8,
    ) -> bool;
}
