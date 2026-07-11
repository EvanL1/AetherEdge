//! Dynamic Slot Runtime
//!
//! Manages dynamic instance slot allocation for the unified SHM pool.
//! Optional — when not configured, all operations are graceful no-ops.

use aether_rtdb_shm::{BitmapStats, InstanceIndex, SlotBitmap};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, warn};

/// Runtime for dynamic SHM slot allocation
///
/// Wraps InstanceIndex + SlotBitmap, both optional.
/// When configured (via `with_dynamic_allocation` builder pattern on InstanceManager),
/// supports hot add/remove of instance slots.
pub struct DynamicSlotRuntime {
    instance_index: Option<Arc<InstanceIndex>>,
    slot_bitmap: Option<Arc<RwLock<SlotBitmap>>>,
}

impl DynamicSlotRuntime {
    /// Create an empty (disabled) runtime
    pub fn new() -> Self {
        Self {
            instance_index: None,
            slot_bitmap: None,
        }
    }

    /// Create a configured runtime with dynamic allocation enabled
    pub fn with_allocation(
        instance_index: Arc<InstanceIndex>,
        slot_bitmap: Arc<RwLock<SlotBitmap>>,
    ) -> Self {
        Self {
            instance_index: Some(instance_index),
            slot_bitmap: Some(slot_bitmap),
        }
    }

    /// Get the InstanceIndex (for external access, e.g., API stats)
    pub fn instance_index(&self) -> Option<&Arc<InstanceIndex>> {
        self.instance_index.as_ref()
    }

    /// Get SlotBitmap stats (for monitoring)
    pub fn bitmap_stats(&self) -> Option<BitmapStats> {
        self.slot_bitmap.as_ref().map(|b| b.read().stats())
    }

    /// Add instance slots to the dynamic pool
    ///
    /// own_counts: [measurement_count, action_count]
    /// Returns Ok(()) on success, or logs warning on failure (non-fatal).
    pub fn add_instance(&self, instance_id: u32, own_counts: [u32; 2]) {
        let total: u32 = own_counts.iter().sum();
        if total == 0 {
            return;
        }
        if let (Some(index), Some(bitmap)) = (&self.instance_index, &self.slot_bitmap) {
            let mut bitmap_guard = bitmap.write();
            match index.add_instance(instance_id, own_counts, Some(&mut bitmap_guard)) {
                Ok(layout) => {
                    debug!(
                        "Inst{} slot allocated: base={}, total={}",
                        instance_id, layout.own_base, layout.own_total
                    );
                },
                Err(e) => {
                    warn!("Inst{} slot allocation failed: {}", instance_id, e);
                },
            }
        }
    }

    /// Remove instance and free its slots from the dynamic pool
    pub fn remove_instance(&self, instance_id: u32) {
        if let (Some(index), Some(bitmap)) = (&self.instance_index, &self.slot_bitmap) {
            let mut bitmap_guard = bitmap.write();
            match index.remove_instance(instance_id, Some(&mut bitmap_guard)) {
                Ok(layout) => {
                    debug!(
                        "Inst{} slot freed: base={}, count={}",
                        instance_id, layout.own_base, layout.own_total
                    );
                },
                Err(e) => {
                    warn!("Inst{} slot deallocation failed: {}", instance_id, e);
                },
            }
        }
    }
}

impl Default for DynamicSlotRuntime {
    fn default() -> Self {
        Self::new()
    }
}
