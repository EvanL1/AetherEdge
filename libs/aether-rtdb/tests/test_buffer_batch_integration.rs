//! WriteBuffer Integration Tests
//!
//! Tests for WriteBuffer performance optimizations:
//! - Large-scale batch writes (100+/1000+ points)
//! - Flush loop timing behavior
//! - Graceful shutdown
//! - Concurrent write safety

#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use aether_rtdb::{MemoryRtdb, Rtdb, WriteBuffer, WriteBufferConfig};
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;

/// Creates a test RTDB
fn create_test_rtdb() -> Arc<MemoryRtdb> {
    Arc::new(MemoryRtdb::new())
}

// ============================================================================
// Basic Flush Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_basic_flush() {
    let config = WriteBufferConfig::default();
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Buffer some writes
    buffer
        .buffer_hash_set("io:1001:T", Arc::from("1"), Bytes::from("100.5"))
        .unwrap();
    buffer
        .buffer_hash_set("io:1001:T", Arc::from("2"), Bytes::from("200.3"))
        .unwrap();
    buffer
        .buffer_hash_set("io:1001:S", Arc::from("1"), Bytes::from("1"))
        .unwrap();

    assert_eq!(buffer.pending_keys(), 2);
    assert_eq!(buffer.pending_fields(), 3);

    // Flush
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 3);

    // Verify data in RTDB
    let value = rtdb.hash_get("io:1001:T", "1").await.unwrap();
    assert_eq!(value, Some(Bytes::from("100.5")));

    let value = rtdb.hash_get("io:1001:S", "1").await.unwrap();
    assert_eq!(value, Some(Bytes::from("1")));

    // Verify stats
    let stats = buffer.stats().snapshot();
    assert_eq!(stats.buffered_writes, 3);
    assert_eq!(stats.flush_count, 1);
    assert_eq!(stats.fields_flushed, 3);
}

// ============================================================================
// Large-scale Batch Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_batch_100_points() {
    let config = WriteBufferConfig::default();
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Buffer 100 telemetry points
    for point_id in 1..=100 {
        let field = Arc::from(point_id.to_string());
        let value = Bytes::from(format!("{}.5", point_id));
        buffer.buffer_hash_set("io:1001:T", field, value).unwrap();
    }

    assert_eq!(buffer.pending_keys(), 1);
    assert_eq!(buffer.pending_fields(), 100);

    // Flush
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 100);

    // Verify random samples
    let value = rtdb.hash_get("io:1001:T", "1").await.unwrap();
    assert_eq!(value, Some(Bytes::from("1.5")));

    let value = rtdb.hash_get("io:1001:T", "50").await.unwrap();
    assert_eq!(value, Some(Bytes::from("50.5")));

    let value = rtdb.hash_get("io:1001:T", "100").await.unwrap();
    assert_eq!(value, Some(Bytes::from("100.5")));

    // Verify stats
    let stats = buffer.stats().snapshot();
    assert_eq!(stats.buffered_writes, 100);
    assert_eq!(stats.fields_flushed, 100);
}

#[tokio::test]
async fn test_write_buffer_batch_1000_points() {
    let config = WriteBufferConfig::high_throughput(); // Use high throughput config
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Buffer 1000 telemetry points across multiple channels
    for channel_id in 1001..=1010 {
        let key = format!("io:{}:T", channel_id);
        for point_id in 1..=100 {
            let field = Arc::from(point_id.to_string());
            let value = Bytes::from(format!("{}.{}", channel_id, point_id));
            buffer.buffer_hash_set(&key, field, value).unwrap();
        }
    }

    assert_eq!(buffer.pending_keys(), 10);
    assert_eq!(buffer.pending_fields(), 1000);

    // Flush
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 1000);

    // Verify samples from different channels
    let value = rtdb.hash_get("io:1001:T", "1").await.unwrap();
    assert_eq!(value, Some(Bytes::from("1001.1")));

    let value = rtdb.hash_get("io:1005:T", "50").await.unwrap();
    assert_eq!(value, Some(Bytes::from("1005.50")));

    let value = rtdb.hash_get("io:1010:T", "100").await.unwrap();
    assert_eq!(value, Some(Bytes::from("1010.100")));

    // Verify stats
    let stats = buffer.stats().snapshot();
    assert_eq!(stats.buffered_writes, 1000);
    assert_eq!(stats.fields_flushed, 1000);
}

// ============================================================================
// Forced Flush Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_forced_flush() {
    // Low threshold to trigger forced flush
    let config = WriteBufferConfig {
        flush_interval_ms: 1000, // Long interval
        max_fields_per_key: 5,   // Low threshold
        ..Default::default()
    };
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Buffer 4 fields (below threshold)
    for i in 1..=4 {
        let field = Arc::from(i.to_string());
        buffer
            .buffer_hash_set("test:key", field, Bytes::from("value"))
            .unwrap();
    }
    assert_eq!(buffer.stats().snapshot().forced_flushes, 0);

    // 5th field triggers forced flush notification
    buffer
        .buffer_hash_set("test:key", Arc::from("5"), Bytes::from("value"))
        .unwrap();
    assert_eq!(buffer.stats().snapshot().forced_flushes, 1);

    // Verify pending data is still there (flush_notify just sends notification)
    assert_eq!(buffer.pending_fields(), 5);

    // Manual flush
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 5);
}

// ============================================================================
// Stats Accuracy Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_stats_accuracy() {
    let config = WriteBufferConfig::default();
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Initial stats
    let stats = buffer.stats().snapshot();
    assert_eq!(stats.buffered_writes, 0);
    assert_eq!(stats.flush_count, 0);
    assert_eq!(stats.fields_flushed, 0);
    assert_eq!(stats.forced_flushes, 0);
    assert_eq!(stats.flush_errors, 0);

    // Buffer 50 writes
    for i in 1..=50 {
        let field = Arc::from(i.to_string());
        buffer
            .buffer_hash_set("key1", field, Bytes::from("value"))
            .unwrap();
    }

    let stats = buffer.stats().snapshot();
    assert_eq!(stats.buffered_writes, 50);
    assert_eq!(stats.flush_count, 0); // Not flushed yet

    // First flush
    buffer.flush(&*rtdb).await.unwrap();

    let stats = buffer.stats().snapshot();
    assert_eq!(stats.flush_count, 1);
    assert_eq!(stats.fields_flushed, 50);

    // Buffer more and flush again
    for i in 51..=75 {
        let field = Arc::from(i.to_string());
        buffer
            .buffer_hash_set("key2", field, Bytes::from("value"))
            .unwrap();
    }

    buffer.flush(&*rtdb).await.unwrap();

    let stats = buffer.stats().snapshot();
    assert_eq!(stats.buffered_writes, 75); // Cumulative
    assert_eq!(stats.flush_count, 2);
    assert_eq!(stats.fields_flushed, 75); // Cumulative
}

// ============================================================================
// Concurrent Write Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_concurrent_writes() {
    let config = WriteBufferConfig::default();
    let buffer = Arc::new(WriteBuffer::new(config));
    let rtdb = create_test_rtdb();

    // Spawn multiple tasks writing concurrently
    let mut handles = vec![];

    for task_id in 0..10 {
        let buffer = buffer.clone();
        let handle = tokio::spawn(async move {
            for i in 0..100 {
                let field = Arc::from(format!("{}_{}", task_id, i));
                let value = Bytes::from(format!("value_{}_{}", task_id, i));
                buffer
                    .buffer_hash_set("concurrent:key", field, value)
                    .unwrap();
            }
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Should have 1000 unique fields (10 tasks * 100 fields each)
    assert_eq!(buffer.pending_fields(), 1000);
    assert_eq!(buffer.stats().buffered_writes.load(Ordering::Relaxed), 1000);

    // Flush and verify
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 1000);

    // Verify some samples
    let value = rtdb.hash_get("concurrent:key", "0_0").await.unwrap();
    assert_eq!(value, Some(Bytes::from("value_0_0")));

    let value = rtdb.hash_get("concurrent:key", "9_99").await.unwrap();
    assert_eq!(value, Some(Bytes::from("value_9_99")));
}

// ============================================================================
// Flush Loop Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_flush_loop_timing() {
    let config = WriteBufferConfig {
        flush_interval_ms: 50, // 50ms interval
        max_fields_per_key: 1000,
        ..Default::default()
    };
    let buffer = Arc::new(WriteBuffer::new(config));
    let rtdb = create_test_rtdb();
    let shutdown = Arc::new(Notify::new());

    // Start flush loop
    let buffer_clone = buffer.clone();
    let rtdb_clone = rtdb.clone();
    let shutdown_clone = shutdown.clone();
    let flush_handle = tokio::spawn(async move {
        buffer_clone
            .flush_loop_with_shutdown(&*rtdb_clone, shutdown_clone)
            .await;
    });

    // Buffer some data
    buffer
        .buffer_hash_set("test:key", Arc::from("field1"), Bytes::from("value1"))
        .unwrap();

    // Wait for automatic flush (should happen within ~50ms)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify data was flushed
    let value = rtdb.hash_get("test:key", "field1").await.unwrap();
    assert_eq!(value, Some(Bytes::from("value1")));

    // Verify stats show at least one flush
    assert!(buffer.stats().snapshot().flush_count >= 1);

    // Shutdown
    shutdown.notify_one();
    flush_handle.await.unwrap();
}

#[tokio::test]
async fn test_write_buffer_graceful_shutdown() {
    let config = WriteBufferConfig {
        flush_interval_ms: 1000, // Long interval
        max_fields_per_key: 1000,
        ..Default::default()
    };
    let buffer = Arc::new(WriteBuffer::new(config));
    let rtdb = create_test_rtdb();
    let shutdown = Arc::new(Notify::new());

    // Start flush loop
    let buffer_clone = buffer.clone();
    let rtdb_clone = rtdb.clone();
    let shutdown_clone = shutdown.clone();
    let flush_handle = tokio::spawn(async move {
        buffer_clone
            .flush_loop_with_shutdown(&*rtdb_clone, shutdown_clone)
            .await;
    });

    // Buffer some data (won't auto-flush due to long interval)
    buffer
        .buffer_hash_set("test:key", Arc::from("field1"), Bytes::from("value1"))
        .unwrap();
    buffer
        .buffer_hash_set("test:key", Arc::from("field2"), Bytes::from("value2"))
        .unwrap();

    // Verify data not yet flushed
    let value = rtdb.hash_get("test:key", "field1").await.unwrap();
    assert!(value.is_none());

    // Trigger shutdown (should perform final flush)
    shutdown.notify_one();
    flush_handle.await.unwrap();

    // Verify data was flushed during shutdown
    let value = rtdb.hash_get("test:key", "field1").await.unwrap();
    assert_eq!(value, Some(Bytes::from("value1")));

    let value = rtdb.hash_get("test:key", "field2").await.unwrap();
    assert_eq!(value, Some(Bytes::from("value2")));
}

// ============================================================================
// Three-Layer Data Tests (value/timestamp/raw)
// ============================================================================

#[tokio::test]
async fn test_write_buffer_three_layer_data() {
    let config = WriteBufferConfig::default();
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Simulate 3-layer write pattern from io
    let channel_id = 1001;
    let point_id: Arc<str> = Arc::from("42");
    let timestamp: Arc<str> = Arc::from("42"); // Same field name for ts hash

    // Layer 1: Engineering value
    buffer
        .buffer_hash_set(
            &format!("io:{}:T", channel_id),
            point_id.clone(),
            Bytes::from("220.5"),
        )
        .unwrap();

    // Layer 2: Timestamp
    buffer
        .buffer_hash_set(
            &format!("io:{}:T:ts", channel_id),
            timestamp.clone(),
            Bytes::from("1704067200000"),
        )
        .unwrap();

    // Layer 3: Raw value
    buffer
        .buffer_hash_set(
            &format!("io:{}:T:raw", channel_id),
            point_id.clone(),
            Bytes::from("2205"),
        )
        .unwrap();

    // Verify 3 keys, 3 fields total
    assert_eq!(buffer.pending_keys(), 3);
    assert_eq!(buffer.pending_fields(), 3);

    // Flush
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 3);

    // Verify all layers
    let eng_value = rtdb.hash_get("io:1001:T", "42").await.unwrap();
    assert_eq!(eng_value, Some(Bytes::from("220.5")));

    let ts_value = rtdb.hash_get("io:1001:T:ts", "42").await.unwrap();
    assert_eq!(ts_value, Some(Bytes::from("1704067200000")));

    let raw_value = rtdb.hash_get("io:1001:T:raw", "42").await.unwrap();
    assert_eq!(raw_value, Some(Bytes::from("2205")));
}

// ============================================================================
// buffer_hash_mset Tests
// ============================================================================

#[tokio::test]
async fn test_write_buffer_mset_batch() {
    let config = WriteBufferConfig::default();
    let buffer = WriteBuffer::new(config);
    let rtdb = create_test_rtdb();

    // Use buffer_hash_mset for efficient batch writes
    let fields: Vec<(Arc<str>, Bytes)> = (1..=50)
        .map(|i| (Arc::from(i.to_string()), Bytes::from(format!("{}.0", i))))
        .collect();

    buffer.buffer_hash_mset("io:1001:T", fields).unwrap();

    assert_eq!(buffer.pending_keys(), 1);
    assert_eq!(buffer.pending_fields(), 50);
    assert_eq!(buffer.stats().snapshot().buffered_writes, 50);

    // Flush
    let flushed = buffer.flush(&*rtdb).await.unwrap();
    assert_eq!(flushed, 50);

    // Verify
    let value = rtdb.hash_get("io:1001:T", "25").await.unwrap();
    assert_eq!(value, Some(Bytes::from("25.0")));
}
