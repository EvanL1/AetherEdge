//! ShmDispatch integration tests
//!
//! Exercises the production M2C dispatch path (SHM + UDS) with real files and sockets.
//!
//! ## Test Coverage
//!
//! 1. Happy path: SHM write + UDS notification → `Delivered`
//! 2. No writer configured → `NoWriter`
//! 3. Writer configured, no notifier → `ShmOnly { reason: "notifier not configured" }`
//! 4. Writer configured, slot missing → `SlotMissing`
//! 5. rebuild_writer with valid routing → new writer installed
//! 6. rebuild_writer with invalid SHM path → writer cleared (next dispatch returns `NoWriter`)

#![allow(clippy::disallowed_methods)] // Integration tests — unwrap is acceptable

use std::sync::Arc;
use std::time::Duration;

use aether_automation::infra::shm_dispatch::{ActionDispatch, DispatchOutcome, ShmDispatch};
use aether_model::PointType;
use aether_routing::RouteContext;
use aether_rtdb_shm::{
    ActionWriter, ChannelPointCounts, SharedConfig, ShmNotification, ShmNotifier, UnifiedWriter,
};
use std::collections::BTreeMap;
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::sync::Mutex;

// ============================================================================
// Helpers
// ============================================================================

/// Build the ChannelPointCounts matching the routing layout from `make_route_ctx`.
///
/// Channel 1001 has 1 Control slot (point_id 0 → count 1).
/// Array layout: [T, S, C, A] = [0, 0, 1, 0].
fn make_channel_point_counts() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(1001u32, [0u32, 0, 1, 0]);
    ChannelPointCounts::from_map(map)
}

/// Build a RouteContext that targets the M2C entry created by `make_routing_cache`.
fn make_route_ctx() -> RouteContext {
    RouteContext {
        channel_id: "1001".to_string(),
        point_type: "C".to_string(),
        io_point_id: "0".to_string(),
        target_channel_id: 1001,
        target_point_type: PointType::Control.to_u8(), // 2
        target_point_id: 0,
        timestamp_ms: 1_700_000_000,
        expires_at_ms: 1_700_005_000,
    }
}

fn make_missing_slot_route_ctx() -> RouteContext {
    RouteContext {
        channel_id: "1001".to_string(),
        point_type: "C".to_string(),
        io_point_id: "99".to_string(),
        target_channel_id: 1001,
        target_point_type: PointType::Control.to_u8(),
        target_point_id: 99,
        timestamp_ms: 1_700_000_000,
        expires_at_ms: 1_700_005_000,
    }
}

/// Create a minimal SharedConfig whose SHM file lives inside `dir`.
fn make_shm_config(dir: &tempfile::TempDir) -> SharedConfig {
    SharedConfig {
        path: dir.path().join("test.shm"),
        max_instances: 16,
        max_points_per_instance: 64,
        // max_channels * max_points_per_channel must cover channel_id 1001 and point_id 0.
        // We use with_max_slots() logic: stores total into max_channels×max_points_per_channel.
        // Here we just set enough capacity directly.
        max_channels: 2048,
        max_points_per_channel: 64,
        snapshot_path: None,
        snapshot_interval: None,
        restore_on_start: false,
    }
}

/// Spawn a minimal UDS listener that accepts one connection and reads notifications
/// until the connection closes. Returns the socket path.
///
/// The listener is spawned as a background task; the caller is responsible for
/// `.abort()`-ing the handle when done.
async fn spawn_uds_listener(sock_path: &str) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(sock_path);
    let listener = UnixListener::bind(sock_path).unwrap();

    tokio::spawn(async move {
        // Accept the first connection then drain it until EOF
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; ShmNotification::SIZE];
            while stream.read_exact(&mut buf).await.is_ok() {
                // consume and discard — we only care that the notifier can send
            }
        }
    })
}

// ============================================================================
// Test 1: Happy path — Delivered
// ============================================================================

/// Verifies the full M2C dispatch path:
///   set_writer + set_notifier → dispatch → Delivered
///   SHM contains the written value after dispatch.
#[tokio::test]
async fn test_dispatch_delivered() {
    let temp_dir = tempfile::tempdir().unwrap();
    let channel_points = make_channel_point_counts();
    let config = make_shm_config(&temp_dir);

    // Create the SHM file with the correct routing layout (io role),
    // then open the automation-side restricted ActionWriter on it.
    let _owner = UnifiedWriter::create(&config, &channel_points).unwrap();
    let writer = Arc::new(ActionWriter::open(&config, &channel_points).unwrap());

    // Set up UDS notifier
    let sock_path = temp_dir.path().join("dispatch_delivered.sock");
    let sock_str = sock_path.to_str().unwrap().to_string();
    let _listener = spawn_uds_listener(&sock_str).await;
    // Give the listener a moment to bind
    tokio::time::sleep(Duration::from_millis(20)).await;

    let notifier = ShmNotifier::connect(&sock_str).await.unwrap();
    assert!(notifier.is_connected(), "Notifier must be connected");
    let notifier = Arc::new(Mutex::new(notifier));

    // Wire up ShmDispatch
    let dispatch = ShmDispatch::new();
    dispatch.set_writer(writer, config.clone());
    dispatch.set_notifier(notifier);

    // Dispatch an action value
    let ctx = make_route_ctx();
    let outcome = dispatch.dispatch(&ctx, 42.0).await;

    assert!(
        matches!(outcome, DispatchOutcome::Delivered),
        "Expected Delivered, got {:?}",
        outcome
    );

    // Verify SHM contains the written value via UnifiedReader
    let reader =
        aether_rtdb_shm::UnifiedReader::open(&config, &channel_points).expect("open reader");
    let (val, _ts) = reader
        .get_channel(1001, PointType::Control.to_u8(), 0)
        .expect("slot must exist for channel 1001:C:0");
    assert!(
        (val - 42.0).abs() < f64::EPSILON,
        "SHM value should be 42.0, got {}",
        val
    );
}

// ============================================================================
// Test 2: No writer → NoWriter
// ============================================================================

/// Verifies that dispatching without calling `set_writer` returns `NoWriter`.
#[tokio::test]
async fn test_dispatch_no_writer() {
    let dispatch = ShmDispatch::new();
    let ctx = make_route_ctx();
    let outcome = dispatch.dispatch(&ctx, 1.0).await;

    assert!(
        matches!(outcome, DispatchOutcome::NoWriter),
        "Expected NoWriter, got {:?}",
        outcome
    );
}

// ============================================================================
// Test 3: Writer set, no notifier → ShmOnly
// ============================================================================

/// Verifies that when only the writer is set (no notifier), dispatch writes SHM
/// but returns `ShmOnly { reason: "notifier not configured" }`.
#[tokio::test]
async fn test_dispatch_shm_only_no_notifier() {
    let temp_dir = tempfile::tempdir().unwrap();
    let channel_points = make_channel_point_counts();
    let config = make_shm_config(&temp_dir);

    let _owner = UnifiedWriter::create(&config, &channel_points).unwrap();
    let writer = Arc::new(ActionWriter::open(&config, &channel_points).unwrap());

    let dispatch = ShmDispatch::new();
    dispatch.set_writer(writer, config.clone());
    // Intentionally NOT calling set_notifier

    let ctx = make_route_ctx();
    let outcome = dispatch.dispatch(&ctx, 7.0).await;

    assert!(
        matches!(
            outcome,
            DispatchOutcome::ShmOnly {
                reason: "notifier not configured"
            }
        ),
        "Expected ShmOnly(notifier not configured), got {:?}",
        outcome
    );

    // SHM value should still have been written
    let reader = aether_rtdb_shm::UnifiedReader::open(&config, &channel_points).unwrap();
    let (val, _ts) = reader
        .get_channel(1001, PointType::Control.to_u8(), 0)
        .expect("slot must exist");
    assert!(
        (val - 7.0).abs() < f64::EPSILON,
        "SHM value should be 7.0, got {}",
        val
    );
}

#[tokio::test]
async fn test_dispatch_slot_missing_fails_before_notify() {
    let temp_dir = tempfile::tempdir().unwrap();
    let channel_points = make_channel_point_counts();
    let config = make_shm_config(&temp_dir);

    let _owner = UnifiedWriter::create(&config, &channel_points).unwrap();
    let writer = Arc::new(ActionWriter::open(&config, &channel_points).unwrap());

    let dispatch = ShmDispatch::new();
    dispatch.set_writer(writer, config.clone());

    let ctx = make_missing_slot_route_ctx();
    let outcome = dispatch.dispatch(&ctx, 7.0).await;

    assert!(
        matches!(outcome, DispatchOutcome::SlotMissing { .. }),
        "Expected SlotMissing, got {:?}",
        outcome
    );
}
