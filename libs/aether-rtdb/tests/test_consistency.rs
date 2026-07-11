//! MemoryRtdb behavior tests
//!
//! Tests for in-memory RTDB implementation.

#![allow(clippy::disallowed_methods)]

use aether_rtdb::{MemoryRtdb, Rtdb};
use bytes::Bytes;

// ============================================================================
// Basic Key-Value Operations
// ============================================================================

#[tokio::test]
async fn test_memory_hash_set_get() {
    let rtdb = MemoryRtdb::new();
    rtdb.hash_set("test:hash", "field1", Bytes::from("value1"))
        .await
        .unwrap();

    let result = rtdb.hash_get("test:hash", "field1").await.unwrap();
    assert_eq!(result, Some(Bytes::from("value1")));
}

// ============================================================================
// Increment Operations
// ============================================================================

#[tokio::test]
async fn test_memory_hincrby_new_field() {
    let rtdb = MemoryRtdb::new();

    // hincrby on non-existent field should initialize to 0 and then increment
    let result = rtdb.hincrby("test:incr", "counter", 5).await.unwrap();
    assert_eq!(result, 5);

    // Second increment
    let result = rtdb.hincrby("test:incr", "counter", 3).await.unwrap();
    assert_eq!(result, 8);
}

#[tokio::test]
async fn test_memory_incrbyfloat_new_key() {
    let rtdb = MemoryRtdb::new();

    // incrbyfloat on non-existent key should initialize to 0.0 and then increment
    let result = rtdb.incrbyfloat("test:float", 2.5).await.unwrap();
    assert!((result - 2.5).abs() < 0.001);

    // Second increment
    let result = rtdb.incrbyfloat("test:float", 1.5).await.unwrap();
    assert!((result - 4.0).abs() < 0.001);
}

// ============================================================================
// Hash Multi-Get Operations
// ============================================================================

#[tokio::test]
async fn test_memory_hash_mget() {
    let rtdb = MemoryRtdb::new();

    rtdb.hash_set("test:mget", "f1", Bytes::from("v1"))
        .await
        .unwrap();
    rtdb.hash_set("test:mget", "f2", Bytes::from("v2"))
        .await
        .unwrap();

    let results = rtdb
        .hash_mget("test:mget", &["f1", "f2", "f3"])
        .await
        .unwrap();

    assert_eq!(results.len(), 3);
    assert_eq!(results[0], Some(Bytes::from("v1")));
    assert_eq!(results[1], Some(Bytes::from("v2")));
    assert_eq!(results[2], None); // f3 doesn't exist
}

// ============================================================================
// Delete Operations
// ============================================================================

#[tokio::test]
async fn test_memory_hash_del() {
    let rtdb = MemoryRtdb::new();

    rtdb.hash_set("test:del", "f1", Bytes::from("v1"))
        .await
        .unwrap();
    rtdb.hash_set("test:del", "f2", Bytes::from("v2"))
        .await
        .unwrap();

    // Delete one field
    let deleted = rtdb.hash_del("test:del", "f1").await.unwrap();
    assert!(deleted);

    // Verify f1 is gone, f2 remains
    assert_eq!(rtdb.hash_get("test:del", "f1").await.unwrap(), None);
    assert_eq!(
        rtdb.hash_get("test:del", "f2").await.unwrap(),
        Some(Bytes::from("v2"))
    );
}

// ============================================================================
// Exists Operations
// ============================================================================

#[tokio::test]
async fn test_memory_exists() {
    let rtdb = MemoryRtdb::new();

    assert!(!rtdb.exists("nonexistent").await.unwrap());

    rtdb.set("exists_key", Bytes::from("value")).await.unwrap();
    assert!(rtdb.exists("exists_key").await.unwrap());
}
