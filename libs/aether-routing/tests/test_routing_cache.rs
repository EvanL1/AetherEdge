//! Integration tests for RoutingCache
//!
//! Supplements the 18 inline unit tests in routing_cache.rs with:
//! - Thread-safety under concurrent updates
//! - Large-scale routing table performance
//! - Serialization roundtrip (content hash determinism)

#![allow(clippy::disallowed_methods)]

use aether_model::PointType;
use aether_routing::RoutingCache;
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Concurrent Update Safety Tests
// ============================================================================

#[test]
fn test_concurrent_reads_during_update() {
    let cache = Arc::new(RoutingCache::new());

    // Set up initial data
    let mut c2m = HashMap::new();
    for i in 0..100 {
        c2m.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
    }
    cache.update(c2m, HashMap::new(), HashMap::new());

    // Spawn readers and one writer concurrently
    let mut handles = vec![];

    // 8 reader threads
    for _ in 0..8 {
        let cache_clone = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..1000 {
                let point_id = i % 100;
                // Lookup should never panic, even during concurrent updates
                let _ = cache_clone.lookup_c2m_by_parts(1001, PointType::Telemetry, point_id);
            }
        }));
    }

    // 2 writer threads — each writes their own channel data
    // Note: update() is a full replacement, not a merge
    for t in 0..2 {
        let cache_clone = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..50 {
                let mut c2m = HashMap::new();
                let base = (t * 1000 + i) * 100;
                for j in 0..100 {
                    c2m.insert(
                        format!("{}:T:{}", 2000 + t, j),
                        format!("{}:M:{}", 50 + t, base + j),
                    );
                }
                cache_clone.update(c2m, HashMap::new(), HashMap::new());
            }
        }));
    }

    for h in handles {
        h.join().expect("Thread panicked during concurrent access");
    }

    // After concurrent updates, the cache should be in a consistent state.
    // update() is a full replacement, so the final C2M table is from the last writer.
    // We just verify no panics occurred and the cache is queryable.
    let entries = cache.c2m_iter();
    assert!(
        !entries.is_empty(),
        "Cache should have entries from the last writer"
    );
}

#[test]
fn test_concurrent_c2c_operations() {
    let cache = Arc::new(RoutingCache::new());

    let mut c2m = HashMap::new();
    let mut c2c = HashMap::new();
    for i in 0..50 {
        c2m.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
        c2m.insert(format!("1002:T:{}", i), format!("24:M:{}", i));
        c2c.insert(format!("1001:T:{}", i), format!("1002:T:{}", i));
    }
    cache.update(c2m, HashMap::new(), c2c);

    // Concurrent C2C lookups
    let mut handles = vec![];
    for _ in 0..8 {
        let cache_clone = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..1000 {
                let pid = i % 50;
                let _ = cache_clone.lookup_c2c_by_parts(1001, PointType::Telemetry, pid);
                let _ = cache_clone.lookup_c2m_by_parts(1002, PointType::Telemetry, pid);
            }
        }));
    }

    for h in handles {
        h.join().expect("C2C concurrent access panicked");
    }
}

// ============================================================================
// Large-Scale Routing Table Tests
// ============================================================================

#[test]
fn test_large_routing_table_1000_entries() {
    let mut c2m = HashMap::new();
    let mut m2c = HashMap::new();

    // 100 channels × 10 points each = 1000 C2M entries
    // M2C keys use valid PointType (A = Adjustment), not "M"
    for ch in 0..100 {
        let channel_id = 1000 + ch;
        let instance_id = 100 + ch;
        for pid in 0..10 {
            c2m.insert(
                format!("{}:T:{}", channel_id, pid),
                format!("{}:M:{}", instance_id, pid),
            );
            m2c.insert(
                format!("{}:A:{}", instance_id, pid),
                format!("{}:A:{}", channel_id, pid),
            );
        }
    }

    let cache = RoutingCache::from_maps(c2m, m2c, HashMap::new());

    // Verify all entries are accessible
    for ch in 0..100 {
        let channel_id = 1000 + ch;
        let instance_id = 100 + ch;
        for pid in 0..10 {
            let c2m_result = cache.lookup_c2m_by_parts(channel_id, PointType::Telemetry, pid);
            assert!(
                c2m_result.is_some(),
                "Missing C2M for ch={}, pid={}",
                channel_id,
                pid
            );
            let target = c2m_result.unwrap();
            assert_eq!(target.instance_id, instance_id);
            assert_eq!(target.point_id, pid);

            let m2c_result = cache.lookup_m2c_by_parts(instance_id, PointType::Adjustment, pid);
            assert!(
                m2c_result.is_some(),
                "Missing M2C for inst={}, pid={}",
                instance_id,
                pid
            );
        }
    }
}

#[test]
fn test_large_routing_table_update_performance() {
    let cache = RoutingCache::new();

    // Initial load: 500 entries
    let mut c2m = HashMap::new();
    for i in 0..500 {
        c2m.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
    }
    cache.update(c2m, HashMap::new(), HashMap::new());

    // Verify count via iterator
    let entries = cache.c2m_iter();
    assert_eq!(entries.len(), 500, "Expected 500 C2M entries");

    // Bulk update: full replacement with 1000 entries
    // update() replaces the entire table, old entries are discarded
    let mut c2m = HashMap::new();
    for i in 0..1000 {
        c2m.insert(format!("2001:T:{}", i), format!("50:M:{}", i));
    }
    cache.update(c2m, HashMap::new(), HashMap::new());

    // Old entries are gone (update = full replacement via ArcSwap)
    let result_old = cache.lookup_c2m_by_parts(1001, PointType::Telemetry, 0);
    assert!(
        result_old.is_none(),
        "Old entries should be replaced after update"
    );

    // New entries should be accessible
    let result_new = cache.lookup_c2m_by_parts(2001, PointType::Telemetry, 999);
    assert!(result_new.is_some(), "New entries should be accessible");

    // Verify full replacement count
    let entries = cache.c2m_iter();
    assert_eq!(
        entries.len(),
        1000,
        "Expected 1000 C2M entries after replacement"
    );
}

// ============================================================================
// Content Hash Determinism Tests
// ============================================================================

#[test]
fn test_content_hash_determinism() {
    // Two caches with same data in different insertion order should have same hash
    let mut c2m_a = HashMap::new();
    let mut c2m_b = HashMap::new();

    for i in 0..50 {
        c2m_a.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
    }
    // Insert in reverse order
    for i in (0..50).rev() {
        c2m_b.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
    }

    let cache_a = RoutingCache::from_maps(c2m_a, HashMap::new(), HashMap::new());
    let cache_b = RoutingCache::from_maps(c2m_b, HashMap::new(), HashMap::new());

    assert_eq!(
        cache_a.content_hash(),
        cache_b.content_hash(),
        "Same data in different order should produce same content hash"
    );
}

#[test]
fn test_content_hash_changes_on_update() {
    let cache = RoutingCache::new();

    let mut c2m = HashMap::new();
    c2m.insert("1001:T:0".to_string(), "23:M:0".to_string());
    cache.update(c2m, HashMap::new(), HashMap::new());

    let hash_before = cache.content_hash();

    // Add a new entry
    let mut c2m = HashMap::new();
    c2m.insert("1001:T:1".to_string(), "23:M:1".to_string());
    cache.update(c2m, HashMap::new(), HashMap::new());

    let hash_after = cache.content_hash();

    assert_ne!(
        hash_before, hash_after,
        "Content hash should change after update"
    );
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_all_point_types_routing() {
    let mut c2m = HashMap::new();
    c2m.insert("1001:T:0".to_string(), "23:M:0".to_string());
    c2m.insert("1001:S:0".to_string(), "23:M:1".to_string());
    c2m.insert("1001:C:0".to_string(), "23:M:2".to_string());
    c2m.insert("1001:A:0".to_string(), "23:M:3".to_string());

    let cache = RoutingCache::from_maps(c2m, HashMap::new(), HashMap::new());

    assert!(
        cache
            .lookup_c2m_by_parts(1001, PointType::Telemetry, 0)
            .is_some()
    );
    assert!(
        cache
            .lookup_c2m_by_parts(1001, PointType::Signal, 0)
            .is_some()
    );
    assert!(
        cache
            .lookup_c2m_by_parts(1001, PointType::Control, 0)
            .is_some()
    );
    assert!(
        cache
            .lookup_c2m_by_parts(1001, PointType::Adjustment, 0)
            .is_some()
    );

    // Non-existent point should return None
    assert!(
        cache
            .lookup_c2m_by_parts(1001, PointType::Telemetry, 999)
            .is_none()
    );
}

#[test]
fn test_m2c_routing_lookups() {
    let mut m2c = HashMap::new();
    // instance 23, Adjustment points → channel 1001, Adjustment points
    // M2C key uses valid PointType (A), not "M"
    for i in 0..5 {
        m2c.insert(format!("23:A:{}", i), format!("1001:A:{}", i));
    }

    let cache = RoutingCache::from_maps(HashMap::new(), m2c, HashMap::new());

    for i in 0..5 {
        let result = cache.lookup_m2c_by_parts(23, PointType::Adjustment, i);
        assert!(result.is_some(), "M2C lookup failed for pid={}", i);
        let target = result.unwrap();
        assert_eq!(target.channel_id, 1001);
        assert_eq!(target.point_type, PointType::Adjustment);
        assert_eq!(target.point_id, i);
    }

    // Non-existent instance
    assert!(
        cache
            .lookup_m2c_by_parts(999, PointType::Adjustment, 0)
            .is_none()
    );
}
