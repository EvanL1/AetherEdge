//! Business-neutral shared-memory data plane.
//!
//! This crate owns the physical SHM layout, seqlock slots, mmap readers and
//! writers, point quality, and snapshot serialization. It deliberately has
//! no device, protocol, routing, database, or service concepts.

mod error;

pub mod core;
mod watch_bitmap;

pub use error::{DataplaneError, DataplaneResult};

pub use core::authority::{AuthorityReadGuard, AuthorityWriteGuard, authority_lock_path};
pub use core::header::{
    AETHER_SHM_MAGIC, HeaderSnapshot, SHM_LAYOUT_VERSION, ShmHeader, calculate_file_size,
};
pub use core::reader::SlotReader;
pub use core::slot_io::{SlotIo, SlotIoWrite, SlotRead};
pub use core::snapshot_format::{SNAPSHOT_MAGIC, SNAPSHOT_VERSION, SnapshotHeader};
pub use core::snapshot_load::SnapshotImage;
pub use core::writer::{GenerationInvalidation, SlotWriter};
pub use watch_bitmap::{SubscriptionBitmap, bitmap_path_for_consumer};
