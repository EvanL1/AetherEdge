//! AetherEMS Realtime Database Abstraction
//!
//! Provides a unified interface for realtime data storage,
//! supporting multiple backends (Redis, in-memory, etc.)
//!
//! # Key Components
//!
//! - **Rtdb trait**: Core trait for realtime database operations
//! - **KeySpaceConfig**: Redis key naming configuration
//! - **WriteBuffer**: Deferred batch write buffer

pub mod traits;

#[cfg(feature = "redis-backend")]
pub mod redis_impl;

pub mod memory_impl;

pub mod error;

pub mod cleanup;

pub mod time;

pub mod write_buffer;

pub mod numfmt;

// Re-exports
pub use bytes::Bytes;
pub use traits::Rtdb;

// KeySpace (canonical location: aether_model)
pub use aether_model::KeySpaceConfig;

#[cfg(feature = "redis-backend")]
pub use redis_impl::RedisRtdb;

pub use memory_impl::{MemoryRtdb, MemoryStats};

pub use cleanup::{CleanupProvider, cleanup_invalid_keys};

pub use time::{FixedTimeProvider, SystemTimeProvider, TimeProvider};

pub use write_buffer::{
    WriteBuffer, WriteBufferConfig, WriteBufferStats, WriteBufferStatsSnapshot,
};

/// Helper functions for common operations
pub mod helpers {
    use super::numfmt::{f64_to_bytes, i64_to_bytes, precomputed};
    use super::{KeySpaceConfig, MemoryRtdb, Rtdb, WriteBuffer};
    use aether_model::PointType;
    use anyhow::{Context, Result};
    use std::sync::Arc;

    // ==================== Test Support ====================

    /// Create an in-memory RTDB for unit testing
    ///
    /// This creates a MemoryRtdb that doesn't require any external services.
    /// Suitable for unit tests that should not depend on Redis.
    ///
    /// # Example
    /// ```
    /// use aether_rtdb::helpers::create_test_rtdb;
    ///
    /// let rtdb = create_test_rtdb();
    /// // Use rtdb in tests...
    /// ```
    pub fn create_test_rtdb() -> Arc<MemoryRtdb> {
        Arc::new(MemoryRtdb::new())
    }

    // ==================== Batch Helpers ====================

    /// Batch write channel points to Redis
    ///
    /// Writes multiple points to three separate hashes:
    /// - `{channel_key}`     → engineering values
    /// - `{channel_key}:ts`  → timestamps
    /// - `{channel_key}:raw` → raw values
    pub async fn write_channel_points<R>(
        rtdb: &R,
        channel_key: &str,
        points: Vec<(u32, f64, f64)>,
        timestamp_ms: i64,
    ) -> Result<usize>
    where
        R: Rtdb,
    {
        if points.is_empty() {
            return Ok(0);
        }

        let count = points.len();
        let timestamp_bytes = i64_to_bytes(timestamp_ms);

        let mut values = Vec::with_capacity(count);
        let mut timestamps = Vec::with_capacity(count);
        let mut raw_values = Vec::with_capacity(count);

        for (point_id, value, raw_value) in points {
            let field: Arc<str> = precomputed::get_point_id_str_or_alloc(point_id);
            values.push((Arc::clone(&field), f64_to_bytes(value)));
            timestamps.push((Arc::clone(&field), timestamp_bytes.clone()));
            raw_values.push((field, f64_to_bytes(raw_value)));
        }

        let ts_key = format!("{}:ts", channel_key);
        let raw_key = format!("{}:raw", channel_key);

        rtdb.pipeline_hash_mset(vec![
            (channel_key.to_string(), values),
            (ts_key, timestamps),
            (raw_key, raw_values),
        ])
        .await
        .context("Failed to write channel points")?;

        Ok(count)
    }

    /// Buffer channel points for deferred write (via WriteBuffer).
    ///
    /// Returns the number of points successfully buffered. If the
    /// WriteBuffer hits its `max_pending_keys` ceiling, the per-layer
    /// `BufferOverflow` errors are logged and the failed-layer counts
    /// subtracted from the return. A caller observing `returned < count`
    /// (or the cumulative `overflow_drops` stat) can react to the loss
    /// instead of silently moving on as before.
    pub fn buffer_channel_points(
        write_buffer: &WriteBuffer,
        channel_key: &str,
        points: Vec<(u32, f64, f64)>,
        timestamp_ms: i64,
    ) -> usize {
        if points.is_empty() {
            return 0;
        }

        let count = points.len();
        let timestamp_bytes = i64_to_bytes(timestamp_ms);

        let mut values = Vec::with_capacity(count);
        let mut timestamps = Vec::with_capacity(count);
        let mut raw_values = Vec::with_capacity(count);

        for (point_id, value, raw_value) in points {
            let field: Arc<str> = precomputed::get_point_id_str_or_alloc(point_id);
            values.push((Arc::clone(&field), f64_to_bytes(value)));
            timestamps.push((Arc::clone(&field), timestamp_bytes.clone()));
            raw_values.push((field, f64_to_bytes(raw_value)));
        }

        let ts_key = format!("{}:ts", channel_key);
        let raw_key = format!("{}:raw", channel_key);

        let mut buffered = count * 3;
        for result in [
            write_buffer.buffer_hash_mset(channel_key, values),
            write_buffer.buffer_hash_mset(&ts_key, timestamps),
            write_buffer.buffer_hash_mset(&raw_key, raw_values),
        ] {
            if let Err(e) = result {
                tracing::warn!("buffer_channel_points dropped: {}", e);
                buffered = buffered.saturating_sub(e.dropped_fields);
            }
        }
        buffered / 3
    }

    /// Write channel point to Hash only (no M2C notification trigger)
    pub async fn write_channel_hash_only<R>(
        rtdb: &R,
        config: &KeySpaceConfig,
        channel_id: u32,
        point_type: PointType,
        point_id: u32,
        value: f64,
        timestamp_ms: i64,
    ) -> Result<()>
    where
        R: Rtdb,
    {
        let channel_key = config.channel_key(channel_id, point_type);

        write_channel_points(
            rtdb,
            &channel_key,
            vec![(point_id, value, value)],
            timestamp_ms,
        )
        .await?;

        Ok(())
    }
}
