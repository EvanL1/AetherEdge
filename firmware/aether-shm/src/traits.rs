//! Shared memory operation traits.

/// Shared memory operations.
///
/// This trait provides a platform-agnostic interface for reading and writing
/// point data to shared memory.
pub trait ShmOps {
    /// Get the number of slots.
    fn slot_count(&self) -> u32;

    /// Check if a slot contains valid data.
    fn is_slot_valid(&self, index: u32) -> bool;

    /// Read a slot value.
    ///
    /// Returns `Some((value, timestamp_ms, quality))` if successful,
    /// `None` if the read was interrupted or the slot is invalid.
    fn read_slot(&self, index: u32) -> Option<(f64, u64, u8)>;

    /// Read a slot value, spinning until successful.
    ///
    /// Use with caution in interrupt contexts.
    fn read_slot_spin(&self, index: u32) -> (f64, u64, u8);

    /// Write a value to a slot.
    fn write_slot(&mut self, index: u32, value: f64, timestamp: u64, quality: u8);

    /// Get the last update timestamp.
    fn last_update(&self) -> u64;
}

/// Extended operations for slot metadata.
pub trait ShmOpsExt: ShmOps {
    /// Get the point ID for a slot.
    fn slot_point_id(&self, index: u32) -> Option<u32>;

    /// Get the instance ID for a slot.
    fn slot_instance_id(&self, index: u32) -> Option<u32>;

    /// Get the point type for a slot.
    fn slot_point_type(&self, index: u32) -> Option<u8>;

    /// Set slot metadata (point_id, instance_id, point_type).
    fn set_slot_metadata(&mut self, index: u32, point_id: u32, instance_id: u32, point_type: u8);
}
