//! SharedMemory Direct Write Tests
//!
//! Tests for direct SharedMemory write path:
//! - UnifiedWriter write operations
//! - ChannelToSlotIndex lookup
//! - Protocol → SharedMemory integration

#![allow(clippy::disallowed_methods)]

use aether_rtdb_shm::{
    ChannelPointCounts, ChannelToSlotIndex, SharedConfig, UnifiedReader, UnifiedWriter,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::tempdir;

// ============================================================================
// Test Helpers
// ============================================================================

/// Create test SharedConfig
fn test_config(dir: &std::path::Path) -> SharedConfig {
    SharedConfig::default()
        .with_path(dir.join("test.shm"))
        .with_max_slots(1000)
}

/// Create ChannelPointCounts with predefined channels:
///   channel 1001: T:0-9 (10), S:0-4 (5), C:0-2 (3)
///   channel 1002: T:0-4 (5), S:0-2 (3)
fn test_channel_points() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(1001u32, [10u32, 5, 3, 0]);
    map.insert(1002u32, [5u32, 3, 0, 0]);
    ChannelPointCounts::from_map(map)
}

// ============================================================================
// Basic Write Tests
// ============================================================================

#[test]
fn test_shm_write_single_point() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let now = 1704067200000u64;

    // Write value: channel 1001, T:0
    assert!(writer.set(1001, 0, 0, 123.456, 1234.0, now));

    writer.flush().unwrap();

    // Read back and verify
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (value, ts) = reader.get_channel(1001, 0, 0).unwrap();

    assert!((value - 123.456).abs() < 0.001);
    assert_eq!(ts, now);
}

#[test]
fn test_shm_write_all_point_types() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let now = 1704067200000u64;

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

    // Write all point types for channel 1001
    // T=0 (Telemetry), S=1 (Signal), C=2 (Control)
    let test_cases = [
        (0u8, 0u32, 100.0), // T:0
        (1u8, 0u32, 1.0),   // S:0
        (2u8, 0u32, 50.0),  // C:0
    ];

    for (pt, pid, value) in test_cases {
        assert!(writer.set(1001, pt, pid, value, value, now));
    }

    writer.flush().unwrap();

    // Verify all
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    for (pt, pid, expected_value) in test_cases {
        let (value, _) = reader.get_channel(1001, pt, pid).unwrap();
        assert!(
            (value - expected_value).abs() < 0.001,
            "type={} point={} value mismatch: {} vs {}",
            pt,
            pid,
            value,
            expected_value
        );
    }
}

#[test]
fn test_shm_write_batch() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let now = 1704067200000u64;

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

    // Write 10 telemetry points
    for pid in 0..10 {
        assert!(writer.set(1001, 0, pid, pid as f64 * 10.0, pid as f64 * 100.0, now));
    }

    writer.flush().unwrap();

    // Verify all
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    for pid in 0..10 {
        let (value, _) = reader.get_channel(1001, 0, pid).unwrap();
        assert!((value - pid as f64 * 10.0).abs() < 0.001);
    }
}

// ============================================================================
// ChannelToSlotIndex Tests
// ============================================================================

#[test]
fn test_channel_to_slot_index_lookup() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Verify mapped points exist
    assert!(
        index
            .lookup(1001, aether_model::PointType::Telemetry, 0)
            .is_some()
    );
    assert!(
        index
            .lookup(1001, aether_model::PointType::Signal, 0)
            .is_some()
    );
    assert!(
        index
            .lookup(1001, aether_model::PointType::Control, 0)
            .is_some()
    );
    assert!(
        index
            .lookup(1002, aether_model::PointType::Telemetry, 0)
            .is_some()
    );

    // Verify unmapped points return None
    assert!(
        index
            .lookup(9999, aether_model::PointType::Telemetry, 0)
            .is_none()
    ); // Unknown channel
    assert!(
        index
            .lookup(1001, aether_model::PointType::Telemetry, 100)
            .is_none()
    ); // Out of range
}

#[test]
fn test_channel_to_slot_index_direct_write() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let now = 1704067200000u64;

    let writer = Arc::new(UnifiedWriter::create(&config, &channel_points).unwrap());
    let index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Get slot offset and write directly
    let _slot_offset = index
        .lookup(1001, aether_model::PointType::Telemetry, 0)
        .unwrap();

    // Verify we can use writer.lookup to get slot index
    let slot = writer.lookup(1001, 0, 0).unwrap();
    writer.set_direct(slot, 42.0, 420.0, now);

    writer.flush().unwrap();

    // Verify
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (value, _) = reader.get_channel(1001, 0, 0).unwrap();
    assert!((value - 42.0).abs() < 0.001);
}

// ============================================================================
// Timestamp Tests
// ============================================================================

#[test]
fn test_shm_timestamp_update() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

    // Write with increasing timestamps
    let ts1 = 1704067200000u64;
    let ts2 = 1704067201000u64;
    let ts3 = 1704067202000u64;

    writer.set(1001, 0, 0, 100.0, 100.0, ts1);
    writer.flush().unwrap();

    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (_, ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert_eq!(ts, ts1);

    writer.set(1001, 0, 0, 200.0, 200.0, ts2);
    let (_, ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert_eq!(ts, ts2);

    writer.set(1001, 0, 0, 300.0, 300.0, ts3);
    let (_, ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert_eq!(ts, ts3);
}

// ============================================================================
// Multi-Channel Tests
// ============================================================================

#[test]
fn test_shm_multi_channel_write() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let now = 1704067200000u64;

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

    // Write to both channels
    assert!(writer.set(1001, 0, 0, 111.0, 111.0, now));
    assert!(writer.set(1002, 0, 0, 222.0, 222.0, now));

    writer.flush().unwrap();

    // Verify isolation
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    let (v1, _) = reader.get_channel(1001, 0, 0).unwrap();
    let (v2, _) = reader.get_channel(1002, 0, 0).unwrap();

    assert!((v1 - 111.0).abs() < 0.001);
    assert!((v2 - 222.0).abs() < 0.001);
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_shm_write_unmapped_returns_false() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let now = 1704067200000u64;

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

    // Unknown channel
    assert!(!writer.set(9999, 0, 0, 100.0, 100.0, now));

    // Point ID out of range
    assert!(!writer.set(1001, 0, 100, 100.0, 100.0, now));
}

#[test]
fn test_shm_reader_unmapped_returns_none() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    writer.flush().unwrap();

    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    // Unknown channel
    assert!(reader.get_channel(9999, 0, 0).is_none());

    // Point ID out of range
    assert!(reader.get_channel(1001, 0, 100).is_none());
}

// ============================================================================
// Performance Hint Tests
// ============================================================================

#[test]
fn test_shm_direct_write_performance() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let now = 1704067200000u64;

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();

    // Pre-lookup slot for hot path
    let slot = writer.lookup(1001, 0, 0).unwrap();

    // Direct write (no lookup overhead)
    for i in 0..1000 {
        writer.set_direct(slot, i as f64, i as f64, now + i);
    }

    writer.flush().unwrap();

    // Verify last value
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (value, ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert!((value - 999.0).abs() < 0.001);
    assert_eq!(ts, now + 999);
}
