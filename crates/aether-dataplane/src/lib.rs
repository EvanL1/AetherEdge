//! Business-neutral shared-memory data plane.
//!
//! This crate owns the physical SHM layout, seqlock slots, mmap readers and
//! writers, dirty tracking, and snapshot serialization. It deliberately has
//! no device, protocol, routing, database, or service concepts.

mod error;

pub mod core;
mod watch_bitmap;

pub use error::{DataplaneError, DataplaneResult};

pub use core::authority::{AuthorityReadGuard, AuthorityWriteGuard, authority_lock_path};
pub use core::bitmap::{BitmapStats, SlotAllocation, SlotBitmap, SlotBitmapHeader};
pub use core::header::{
    DEFAULT_MAX_SLOTS, HeaderSnapshot, UNIFIED_MAGIC, UNIFIED_VERSION, UnifiedHeader,
    calculate_file_size,
};
pub use core::reader::SlotReader;
pub use core::slot::PointSlot;
pub use core::slot_io::{SlotIo, SlotIoWrite, SlotRead};
pub use core::snapshot_load::SnapshotImage;
pub use core::writer::SlotWriter;
pub use watch_bitmap::{
    SubscriptionBitmap, WATCH_BITMAP_SIZE, WATCH_BITMAP_SUFFIX, WATCH_WORDS_COUNT,
    automation_bitmap_path_from_shm, bitmap_path_for_consumer,
};
