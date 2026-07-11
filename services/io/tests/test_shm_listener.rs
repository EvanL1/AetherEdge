//! Integration tests for ShmCommandListener (M2C receive-and-dispatch loop)
//!
//! These tests exercise the full ShmNotifier → ShmCommandListener roundtrip via UDS,
//! covering the dispatch loop that previously had zero test coverage.

#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use std::collections::HashMap;
use std::time::Duration;

use aether_io::core::channels::ShmCommandListener;
use aether_io::core::channels::types::ChannelCommand;
use aether_model::PointType;
use aether_rtdb_shm::ShmNotification;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Helper: create a temporary UDS socket path using tempdir
fn temp_uds_path() -> (tempfile::TempDir, String) {
    // Unix-domain socket paths are short on macOS; keep this below sun_path.
    let dir = tempfile::Builder::new()
        .prefix("aether-")
        .tempdir_in("/tmp")
        .expect("tempdir failed");
    let path = dir.path().join("test-m2c.sock");
    let path_str = path.to_string_lossy().to_string();
    (dir, path_str)
}

/// Helper: start the ShmCommandListener in a background task.
///
/// Returns the shutdown sender so the caller can stop the listener.
/// The listener is given a custom UDS path via `Some(path)`.
fn start_listener(
    uds_path: &str,
    senders: HashMap<u32, mpsc::Sender<ChannelCommand>>,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let listener = ShmCommandListener::new(Some(uds_path), shutdown_rx);
    for (channel_id, tx) in senders {
        listener.register_channel(channel_id, tx);
    }
    let handle = tokio::spawn(async move { listener.run().await });
    (shutdown_tx, handle)
}

/// Helper: write a raw ShmNotification to a UDS socket at `path`.
///
/// Connects once, sends the bytes, then drops the connection.
async fn send_notification(path: &str, notif: ShmNotification) {
    let mut stream = tokio::net::UnixStream::connect(path)
        .await
        .expect("UDS connect failed");
    stream
        .write_all(&notif.to_bytes())
        .await
        .expect("UDS write failed");
    // Flush and drop so the listener sees EOF
    stream.flush().await.ok();
}

/// Helper: wait for the listener socket to become ready (poll up to 200 ms).
async fn wait_for_listener(path: &str) {
    for _ in 0..20 {
        if tokio::net::UnixStream::connect(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("ShmCommandListener did not become ready on {}", path);
}

// ============================================================================
// Test cases
// ============================================================================

/// Send a Control notification via UDS; verify ChannelCommand::Control is dispatched.
#[tokio::test]
async fn test_listener_receives_control_command() {
    let (_dir, uds_path) = temp_uds_path();

    let (tx, mut rx) = mpsc::channel::<ChannelCommand>(8);
    let mut senders = HashMap::new();
    senders.insert(1001u32, tx);

    let (shutdown_tx, _handle) = start_listener(&uds_path, senders);
    wait_for_listener(&uds_path).await;

    let notif = ShmNotification::new(
        1001,                  // channel_id
        PointType::Control,    // point_type
        42,                    // point_id
        123.45,                // value
        1_000_000,             // timestamp_ms
        u64::MAX,              // expires_at_ms
        0xDEAD_BEEF_1234_5678, // producer_id
        1,                     // seq
    );
    send_notification(&uds_path, notif).await;

    let cmd = timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("timed out waiting for command")
        .expect("channel closed before receiving command");

    match cmd {
        ChannelCommand::Control {
            point_id, value, ..
        } => {
            assert_eq!(point_id, 42);
            assert!((value - 123.45).abs() < 1e-9, "value mismatch: {}", value);
        },
        other => panic!("Expected Control, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
}

/// Send an Adjustment notification via UDS; verify ChannelCommand::Adjustment is dispatched.
#[tokio::test]
async fn test_listener_receives_adjustment_command() {
    let (_dir, uds_path) = temp_uds_path();

    let (tx, mut rx) = mpsc::channel::<ChannelCommand>(8);
    let mut senders = HashMap::new();
    senders.insert(2001u32, tx);

    let (shutdown_tx, _handle) = start_listener(&uds_path, senders);
    wait_for_listener(&uds_path).await;

    let notif = ShmNotification::new(
        2001,
        PointType::Adjustment,
        7,
        -99.5,
        2_000_000,
        u64::MAX,
        0xCAFE_BABE_0000_0001,
        1,
    );
    send_notification(&uds_path, notif).await;

    let cmd = timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("timed out waiting for adjustment command")
        .expect("channel closed before receiving command");

    match cmd {
        ChannelCommand::Adjustment {
            point_id, value, ..
        } => {
            assert_eq!(point_id, 7);
            assert!((value - (-99.5)).abs() < 1e-9, "value mismatch: {}", value);
        },
        other => panic!("Expected Adjustment, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
}

/// Send a notification for an unregistered channel_id.
/// The listener should drop it gracefully without panicking.
/// No command should arrive on any registered channel.
#[tokio::test]
async fn test_listener_ignores_unknown_channel() {
    let (_dir, uds_path) = temp_uds_path();

    // Register channel 1001 but send to channel 9999 (unregistered)
    let (tx, mut rx) = mpsc::channel::<ChannelCommand>(8);
    let mut senders = HashMap::new();
    senders.insert(1001u32, tx);

    let (shutdown_tx, _handle) = start_listener(&uds_path, senders);
    wait_for_listener(&uds_path).await;

    let notif = ShmNotification::new(9999, PointType::Control, 1, 1.0, 0, u64::MAX, 42, 1);
    send_notification(&uds_path, notif).await;

    // Give the listener some time to process the notification
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Nothing should arrive on the registered sender
    let result = timeout(Duration::from_millis(50), rx.recv()).await;
    assert!(
        result.is_err(),
        "Expected timeout (no command for ch1001), but got a message"
    );

    let _ = shutdown_tx.send(true);
}

/// Send the same (producer_id, seq) twice for the same point.
/// The listener's dedup logic must dispatch only the first; the second is dropped.
#[tokio::test]
async fn test_listener_dedup_same_sequence() {
    let (_dir, uds_path) = temp_uds_path();

    let (tx, mut rx) = mpsc::channel::<ChannelCommand>(8);
    let mut senders = HashMap::new();
    senders.insert(1001u32, tx);

    let (shutdown_tx, _handle) = start_listener(&uds_path, senders);
    wait_for_listener(&uds_path).await;

    // Same producer_id + seq — second one is a duplicate and must be dropped
    let producer_id = 0x1111_2222_3333_4444u64;
    let seq = 5u64;

    let notif_first = ShmNotification::new(
        1001,
        PointType::Control,
        10,
        1.0,
        0,
        u64::MAX,
        producer_id,
        seq,
    );
    let notif_dup = ShmNotification::new(
        1001,
        PointType::Control,
        10,
        2.0,
        0,
        u64::MAX,
        producer_id,
        seq,
    );

    send_notification(&uds_path, notif_first).await;
    // Small pause to ensure first notification is fully processed before sending duplicate
    tokio::time::sleep(Duration::from_millis(20)).await;
    send_notification(&uds_path, notif_dup).await;

    // First command must arrive
    let cmd = timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("timed out waiting for first command")
        .expect("channel closed");

    match cmd {
        ChannelCommand::Control { value, .. } => {
            assert!(
                (value - 1.0).abs() < 1e-9,
                "expected first value 1.0, got {}",
                value
            );
        },
        other => panic!("Expected Control, got {:?}", other),
    }

    // No second command should arrive (duplicate is dropped)
    tokio::time::sleep(Duration::from_millis(100)).await;
    let dup_result = timeout(Duration::from_millis(50), rx.recv()).await;
    assert!(
        dup_result.is_err(),
        "Duplicate notification was NOT dropped — second command arrived unexpectedly"
    );

    let _ = shutdown_tx.send(true);
}

/// Send a notification carrying a NaN value.
/// The validate_value filter must reject it; no command should be dispatched.
#[tokio::test]
async fn test_listener_rejects_nan_value() {
    let (_dir, uds_path) = temp_uds_path();

    let (tx, mut rx) = mpsc::channel::<ChannelCommand>(8);
    let mut senders = HashMap::new();
    senders.insert(1001u32, tx);

    let (shutdown_tx, _handle) = start_listener(&uds_path, senders);
    wait_for_listener(&uds_path).await;

    // Craft a notification with NaN stored in value_bits
    let nan_notif = ShmNotification {
        channel_id: 1001,
        point_id: 5,
        point_type: PointType::Control as u8,
        _padding: [0; 7],
        value_bits: f64::NAN.to_bits(),
        timestamp_ms: 1234,
        expires_at_ms: u64::MAX,
        producer_id: 99,
        seq: 1,
    };
    send_notification(&uds_path, nan_notif).await;

    // Give the listener time to process
    tokio::time::sleep(Duration::from_millis(100)).await;

    // No command should arrive
    let result = timeout(Duration::from_millis(50), rx.recv()).await;
    assert!(
        result.is_err(),
        "NaN notification was NOT rejected — a command arrived unexpectedly"
    );

    let _ = shutdown_tx.send(true);
}

#[tokio::test]
async fn test_listener_rejects_expired_command() {
    let (_dir, uds_path) = temp_uds_path();
    let (tx, mut rx) = mpsc::channel::<ChannelCommand>(8);
    let mut senders = HashMap::new();
    senders.insert(1001u32, tx);

    let (shutdown_tx, _handle) = start_listener(&uds_path, senders);
    wait_for_listener(&uds_path).await;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as u64;
    let expired = ShmNotification::new(
        1001,
        PointType::Control,
        5,
        1.0,
        now_ms.saturating_sub(10_000),
        now_ms.saturating_sub(1),
        99,
        2,
    );
    send_notification(&uds_path, expired).await;

    assert!(
        timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "expired command must not reach the channel runtime"
    );
    let _ = shutdown_tx.send(true);
}
