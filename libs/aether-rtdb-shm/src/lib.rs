//! AetherEMS Shared Memory Subsystem
//!
//! Provides shared memory (SHM) and IPC components for zero-latency
//! cross-process data sharing between io and automation containers.
//!
//! # Module Boundary
//!
//! - **core::** — pure SHM infrastructure (slot storage + bitmap). No business
//!   knowledge. Must stay free of `aether-model` / `aether-routing` deps so
//!   it can be promoted to a standalone `aether-shm-slots` crate later.
//! - everything else — business adapters (channel/instance/routing/M2C
//!   dispatch). These will be progressively pushed out of this crate.
//!
//! # Key Components
//!
//! - **core::slot**: Vector-based point storage with atomic PointSlot
//! - **core::bitmap**: Dynamic slot allocation bitmap
//! - **notification**: M2C command notification struct
//! - **notifier**: UDS event notification for M2C dispatch
//! - **instance_index**: Dynamic instance management with shared slots
//! - **unified_shm**: Unified shared memory (Header + PointSlots)
//! - **channel_index**: Dynamic channel management with ArcSwap
//! - **snapshot**: Periodic SHM snapshot management
//! - **shared_config**: SharedConfig, ChannelToSlotIndex, utility functions

// The physical SHM data plane now lives in its own database-free crate. Keep
// this module path as a compatibility re-export while business adapters migrate.
pub use aether_dataplane::core;

// Backward-compat shims for the old top-level module paths. External
// consumers that imported `aether_rtdb_shm::vec_impl::PointSlot` or
// `aether_rtdb_shm::slot_bitmap::SlotBitmap` keep working.
pub mod vec_impl {
    pub use crate::core::slot::*;
}
pub mod slot_bitmap {
    pub use crate::core::bitmap::*;
}

pub mod layout;

pub mod channel_points;

pub mod notification;

#[cfg(unix)]
pub mod notifier;

// PointWatch: cross-process event-driven notification (io → automation)
#[cfg(unix)]
pub mod point_watch;
pub mod point_watch_event;
#[cfg(unix)]
pub mod point_watch_listener;
#[cfg(unix)]
pub mod subscription_bitmap;

pub mod instance_index;

pub mod unified_shm;

pub mod channel_index;

pub mod snapshot;

pub mod shared_config;

pub mod reverse_index;

pub mod shm_handle;

pub mod batch_direct;

#[cfg(unix)]
pub mod dispatch;

// Re-exports for convenience
pub use core::bitmap::{BitmapStats, SlotAllocation, SlotBitmap, SlotBitmapHeader};
pub use core::reader::SlotReader;
pub use core::slot::PointSlot;
pub use core::slot_io::{SlotIo, SlotIoWrite, SlotRead};
pub use core::writer::SlotWriter;
pub use instance_index::{DynamicInstanceLayout, InstanceIndex, SharedSlotRef};
pub use notification::ShmNotification;
#[cfg(unix)]
pub use notifier::{DEFAULT_UDS_PATH, NotifyResult, ShmNotifier};

// Channel point counts (routing-independent SHM layout data source)
pub use channel_points::ChannelPointCounts;

// Channel slot layout (business adapter, not part of core)
pub use layout::{ChannelLayout, allocate_layouts};

// Unified shared memory
pub use unified_shm::{
    ActionWriter, UNIFIED_MAGIC, UNIFIED_VERSION, UnifiedHeader, UnifiedReader,
    UnifiedReaderHandle, UnifiedWriter, calculate_file_size,
};

// Channel index for dynamic channel management
pub use channel_index::{ChannelIndex, DynamicChannelLayout};

// Snapshot management
pub use snapshot::{SnapshotConfig, SnapshotManager, snapshot_exists};

// Shared memory configuration and utilities
pub use shared_config::{
    ChannelToSlotIndex, DEFAULT_SHM_PATH, SHARED_MAGIC, SharedConfig, default_shm_path,
    is_shm_available, timestamp_ms,
};

// Reverse slot index: slot → (channel_id, point_type, point_id)
pub use reverse_index::{ReverseSlotIndex, SlotOrigin};

// Runtime-swappable SHM handle
pub use shm_handle::{ShmHandle, ShmLayout};

// Direct authoritative SHM batch write with in-memory routing.
pub use batch_direct::write_channel_batch_direct;

// SHM/UDS action dispatch — shared by automation HTTP path and rules executor
#[cfg(unix)]
pub use dispatch::{ActionDispatch, DispatchOutcome, NoopDispatch, ShmDispatch};

// PointWatch public API
#[cfg(unix)]
pub use point_watch::{AUTOMATION_POINT_WATCH_UDS_PATH, PointWatchSignaler};
pub use point_watch_event::PointWatchEvent;
#[cfg(unix)]
pub use point_watch_listener::PointWatchListener;
#[cfg(unix)]
pub use subscription_bitmap::{
    SubscriptionBitmap, WATCH_BITMAP_SIZE, WATCH_WORDS_COUNT, automation_bitmap_path_from_shm,
    bitmap_path_for_consumer,
};
