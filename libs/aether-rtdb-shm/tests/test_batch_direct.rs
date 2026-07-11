//! Integration tests for batch_direct write path
//!
//! Tests the high-performance `write_channel_batch_direct` function which combines:
//! - Direct SHM slot writes via ChannelToSlotIndex
//! - no secondary live-state write
//! - C2C routing with cycle detection

#![allow(clippy::disallowed_methods)]

use aether_model::PointType;
use aether_routing::RoutingCache;
use aether_routing::batch::ChannelPointUpdate;
use aether_rtdb_shm::{
    ChannelPointCounts, ChannelToSlotIndex, SharedConfig, UnifiedReader, UnifiedWriter,
    batch_direct::write_channel_batch_direct,
};
use std::collections::{BTreeMap, HashMap};
use tempfile::tempdir;

// ============================================================================
// Test Helpers
// ============================================================================

/// Create test SharedConfig with temp directory
fn test_config(dir: &std::path::Path) -> SharedConfig {
    SharedConfig::default()
        .with_path(dir.join("test.shm"))
        .with_max_slots(2000)
}

/// Create ChannelPointCounts for two channels:
///   channel 1001: T:0-9 (10), S:0-4 (5)
///   channel 1002: T:0-4 (5)
fn test_channel_points() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(1001u32, [10u32, 5, 0, 0]);
    map.insert(1002u32, [5u32, 0, 0, 0]);
    ChannelPointCounts::from_map(map)
}

/// Create routing cache with C2M mappings for two channels.
fn test_routing_cache() -> RoutingCache {
    let mut c2m = HashMap::new();

    // Channel 1001: T:0-9 → instance 23:M:0-9
    for i in 0..10 {
        c2m.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
    }
    // Channel 1001: S:0-4 → instance 23:M:10-14
    for i in 0..5 {
        c2m.insert(format!("1001:S:{}", i), format!("23:M:{}", 10 + i));
    }

    // Channel 1002: T:0-4 → instance 24:M:0-4
    for i in 0..5 {
        c2m.insert(format!("1002:T:{}", i), format!("24:M:{}", i));
    }

    RoutingCache::from_maps(c2m, HashMap::new(), HashMap::new())
}

/// Create ChannelPointCounts for C2C test:
///   channel 1001: T:0, channel 1002: T:0, channel 1003: T:0
fn test_channel_points_with_c2c() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(1001u32, [1u32, 0, 0, 0]);
    map.insert(1002u32, [1u32, 0, 0, 0]);
    map.insert(1003u32, [1u32, 0, 0, 0]);
    ChannelPointCounts::from_map(map)
}

/// Create routing cache with C2C forwarding rules
fn test_routing_cache_with_c2c() -> RoutingCache {
    let mut c2m = HashMap::new();
    let mut c2c = HashMap::new();

    // Channel 1001 T:0 → instance 23:M:0
    c2m.insert("1001:T:0".to_string(), "23:M:0".to_string());
    // Channel 1002 T:0 → instance 24:M:0
    c2m.insert("1002:T:0".to_string(), "24:M:0".to_string());
    // Channel 1003 T:0 → instance 25:M:0
    c2m.insert("1003:T:0".to_string(), "25:M:0".to_string());

    // C2C: 1001:T:0 → 1002:T:0 (forward from channel 1001 to 1002)
    c2c.insert("1001:T:0".to_string(), "1002:T:0".to_string());

    RoutingCache::from_maps(c2m, HashMap::new(), c2c)
}

/// Create ChannelPointCounts for C2C cycle test:
///   channel 1001: T:0, channel 1002: T:0
fn test_channel_points_with_c2c_cycle() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(1001u32, [1u32, 0, 0, 0]);
    map.insert(1002u32, [1u32, 0, 0, 0]);
    ChannelPointCounts::from_map(map)
}

/// Create routing cache with C2C cycle: A → B → A
fn test_routing_cache_with_c2c_cycle() -> RoutingCache {
    let mut c2m = HashMap::new();
    let mut c2c = HashMap::new();

    c2m.insert("1001:T:0".to_string(), "23:M:0".to_string());
    c2m.insert("1002:T:0".to_string(), "24:M:0".to_string());

    // Cycle: 1001:T:0 → 1002:T:0 → 1001:T:0
    c2c.insert("1001:T:0".to_string(), "1002:T:0".to_string());
    c2c.insert("1002:T:0".to_string(), "1001:T:0".to_string());

    RoutingCache::from_maps(c2m, HashMap::new(), c2c)
}

fn make_update(
    channel_id: u32,
    point_type: PointType,
    point_id: u32,
    value: f64,
) -> ChannelPointUpdate {
    ChannelPointUpdate {
        channel_id,
        point_type,
        point_id,
        value,
        raw_value: None,
        cascade_depth: 0,
    }
}

fn make_update_with_raw(
    channel_id: u32,
    point_type: PointType,
    point_id: u32,
    value: f64,
    raw_value: f64,
) -> ChannelPointUpdate {
    ChannelPointUpdate {
        channel_id,
        point_type,
        point_id,
        value,
        raw_value: Some(raw_value),
        cascade_depth: 0,
    }
}

// ============================================================================
// Test 1: Single point direct SHM write
// ============================================================================

#[test]
fn test_single_point_direct_write() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let routing = test_routing_cache();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);
    let updates = vec![make_update(1001, PointType::Telemetry, 0, 123.456)];

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);

    // One authoritative SHM write; C2M reads alias that slot through routing.
    assert_eq!(result.channel_writes, 1, "Expected 1 SHM write");
    assert_eq!(result.c2c_forwards, 0, "No C2C expected");
    assert_eq!(result.cycles_detected, 0, "No cycles expected");

    // Verify SHM data via UnifiedReader
    writer.flush().unwrap();
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (value, _ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert!(
        (value - 123.456).abs() < 0.001,
        "SHM value mismatch: got {}",
        value
    );
}

// ============================================================================
// Test 2: Multi-point batch write (100 points)
// ============================================================================

#[test]
fn test_batch_write_100_points() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let routing = test_routing_cache();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Create 10 telemetry points × 1 channel + 5 signal points × 1 channel = 15 mapped points
    // Plus 5 telemetry from channel 1002 = 20 total mapped
    let mut updates = Vec::with_capacity(20);

    for i in 0..10 {
        updates.push(make_update(
            1001,
            PointType::Telemetry,
            i,
            (i as f64) * 10.0,
        ));
    }
    for i in 0..5 {
        updates.push(make_update(1001, PointType::Signal, i, (i as f64) * 100.0));
    }
    for i in 0..5 {
        updates.push(make_update(1002, PointType::Telemetry, i, (i as f64) * 5.0));
    }

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);

    // All 20 points should be written to SHM
    assert_eq!(result.channel_writes, 20, "Expected 20 SHM writes");
    // C2M is an alias lookup, not a duplicated write.
    assert_eq!(result.c2m_writes, 0);

    // Verify data integrity via reader
    writer.flush().unwrap();
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    // Verify channel 1001 T:5
    let (value, _ts) = reader.get_channel(1001, 0, 5).unwrap();
    assert!((value - 50.0).abs() < 0.001, "T:5 value mismatch");

    // Verify channel 1001 S:3
    let (value, _ts) = reader.get_channel(1001, 1, 3).unwrap();
    assert!((value - 300.0).abs() < 0.001, "S:3 value mismatch");

    // Verify channel 1002 T:4
    let (value, _ts) = reader.get_channel(1002, 0, 4).unwrap();
    assert!((value - 20.0).abs() < 0.001, "Ch1002 T:4 value mismatch");
}

// ============================================================================
// Test 3: C2C routing forward (cascade_depth increment)
// ============================================================================

#[test]
fn test_c2c_routing_forward() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points_with_c2c();
    let routing = test_routing_cache_with_c2c();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Write to channel 1001:T:0 which has C2C → 1002:T:0
    let updates = vec![make_update(1001, PointType::Telemetry, 0, 42.0)];

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);

    // Should see the original write + C2C forward
    assert!(result.channel_writes >= 1, "Expected at least 1 SHM write");
    assert_eq!(result.c2c_forwards, 1, "Expected 1 C2C forward");
    assert_eq!(result.cycles_detected, 0, "No cycles in linear chain");

    // Verify the forwarded value is written to channel 1002
    writer.flush().unwrap();
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    // Original write: 1001:T:0
    let (value, _ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert!((value - 42.0).abs() < 0.001, "Original value mismatch");

    // Forwarded write: 1002:T:0 should also have value 42.0
    let (value, _ts) = reader.get_channel(1002, 0, 0).unwrap();
    assert!((value - 42.0).abs() < 0.001, "Forwarded value mismatch");
}

// ============================================================================
// Test 4: C2C cycle detection (A → B → A)
// ============================================================================

#[test]
fn test_c2c_cycle_detection() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points_with_c2c_cycle();
    let routing = test_routing_cache_with_c2c_cycle();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Write to 1001:T:0 → C2C to 1002:T:0 → C2C back to 1001:T:0 (cycle!)
    let updates = vec![make_update(1001, PointType::Telemetry, 0, 99.0)];

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);

    // Should detect the cycle and stop
    assert!(
        result.cycles_detected > 0,
        "Expected cycle detection, got 0"
    );
    assert!(
        result.c2c_forwards >= 1,
        "Expected at least 1 forward before cycle detected"
    );

    // Verify both channels got the value (before the cycle was detected)
    writer.flush().unwrap();
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (v1, _) = reader.get_channel(1001, 0, 0).unwrap();
    assert!((v1 - 99.0).abs() < 0.001, "Source channel value");
}

// ============================================================================
// Test 5: Empty batch (no panic)
// ============================================================================

#[test]
fn test_empty_batch_no_panic() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let routing = test_routing_cache();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, vec![]);

    assert_eq!(result.channel_writes, 0);
    assert_eq!(result.c2m_writes, 0);
    assert_eq!(result.c2c_forwards, 0);
    assert_eq!(result.cycles_detected, 0);
}

// ============================================================================
// Test 6: Unmapped channel falls through (no SHM write)
// ============================================================================

#[test]
fn test_unmapped_channel_no_shm_write() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let routing = test_routing_cache();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Channel 9999 doesn't exist in routing
    let updates = vec![make_update(9999, PointType::Telemetry, 0, 1.0)];

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);

    // No SHM write (channel not in index), no C2M (not in routing)
    assert_eq!(
        result.channel_writes, 0,
        "Unmapped channel should not produce SHM writes"
    );
    assert_eq!(
        result.c2m_writes, 0,
        "Unmapped channel should not produce C2M writes"
    );
}

// ============================================================================
// Test 7: Raw value propagation
// ============================================================================

#[test]
fn test_raw_value_propagation() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let routing = test_routing_cache();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Write with explicit raw value
    let updates = vec![make_update_with_raw(
        1001,
        PointType::Telemetry,
        0,
        23.05,
        2305.0,
    )];

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);
    assert_eq!(result.channel_writes, 1);

    // Verify SHM stores the engineering value
    writer.flush().unwrap();
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();
    let (value, _ts) = reader.get_channel(1001, 0, 0).unwrap();
    assert!((value - 23.05).abs() < 0.001, "Engineering value mismatch");
}

// ============================================================================
// Test 8: Mixed channels in single batch
// ============================================================================

#[test]
fn test_mixed_channels_single_batch() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let channel_points = test_channel_points();
    let routing = test_routing_cache();

    let writer = UnifiedWriter::create(&config, &channel_points).unwrap();
    let channel_index = ChannelToSlotIndex::from_unified_writer(&writer);

    // Mix updates from different channels and point types
    let updates = vec![
        make_update(1001, PointType::Telemetry, 0, 100.0),
        make_update(1002, PointType::Telemetry, 0, 200.0),
        make_update(1001, PointType::Signal, 0, 1.0),
        make_update(1001, PointType::Telemetry, 1, 150.0),
        make_update(1002, PointType::Telemetry, 1, 250.0),
    ];

    let result = write_channel_batch_direct(&writer, &channel_index, &routing, updates);

    assert_eq!(result.channel_writes, 5, "All 5 points should write to SHM");

    // Verify isolation between channels
    writer.flush().unwrap();
    let reader = UnifiedReader::open(&config, &channel_points).unwrap();

    let (v, _) = reader.get_channel(1001, 0, 0).unwrap();
    assert!((v - 100.0).abs() < 0.001);

    let (v, _) = reader.get_channel(1002, 0, 0).unwrap();
    assert!((v - 200.0).abs() < 0.001);

    let (v, _) = reader.get_channel(1001, 1, 0).unwrap();
    assert!((v - 1.0).abs() < 0.001);
}
