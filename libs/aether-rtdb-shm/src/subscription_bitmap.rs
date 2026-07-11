//! Compatibility exports for the business-neutral PointWatch bitmap.

pub use aether_dataplane::{
    SubscriptionBitmap, WATCH_BITMAP_SIZE, WATCH_BITMAP_SUFFIX, WATCH_WORDS_COUNT,
    automation_bitmap_path_from_shm, bitmap_path_for_consumer,
};
