//! UDS event notification integration tests
//!
//! Verifies the UDS event-driven notification mechanism from automation to io
//!
//! ## Test Coverage
//!
//! 1. UDS notification send/receive correctness
//! 2. End-to-end latency verification < 5ms
//! 3. Degradation scenario: non-blocking when UDS is unavailable

#![allow(clippy::disallowed_methods)] // Integration test - unwrap is acceptable

use aether_model::PointType;
use aether_rtdb_shm::{ShmNotification, ShmNotifier};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

mod common;

/// Test 1: UDS notification send/receive correctness
///
/// Verifies that notifications sent by ShmNotifier can be correctly received and parsed
#[tokio::test]
async fn test_uds_notification_roundtrip() {
    // 1. Create test UDS path
    let test_uds_path = "/tmp/aether-test-roundtrip.sock";
    let _ = std::fs::remove_file(test_uds_path);

    // 2. Start local UDS listener
    let listener = UnixListener::bind(test_uds_path).unwrap();
    let (tx, mut rx) = mpsc::channel::<ShmNotification>(10);

    let listener_handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; ShmNotification::SIZE];
        while stream.read_exact(&mut buf).await.is_ok() {
            let notif = ShmNotification::from_bytes(&buf);
            if tx.send(notif).await.is_err() {
                break;
            }
        }
    });

    // Wait for listener to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // 3. Create ShmNotifier and connect
    let mut notifier = ShmNotifier::connect(test_uds_path).await.unwrap();
    assert!(notifier.is_connected(), "Notifier should be connected");

    // 4. Send Control notification
    let result = notifier
        .notify(
            1001,
            PointType::Control,
            42,
            12.5,
            1_700_000_001,
            1_700_005_001,
        )
        .await;
    assert!(result.uds_sent, "UDS notification should be sent");

    // 5. Verify reception
    let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("Timeout waiting for notification")
        .expect("Channel closed unexpectedly");

    assert_eq!(received.channel_id, 1001);
    assert_eq!(received.point_id, 42);
    assert_eq!(received.value(), 12.5);
    assert_eq!(received.timestamp_ms, 1_700_000_001);
    assert_eq!(
        received.get_point_type(),
        Some(PointType::Control),
        "Point type should be Control"
    );

    // 6. Send Adjustment notification
    let result = notifier
        .notify(
            2002,
            PointType::Adjustment,
            99,
            88.0,
            1_700_000_002,
            1_700_005_002,
        )
        .await;
    assert!(result.uds_sent, "UDS notification should be sent");

    let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("Timeout waiting for notification")
        .expect("Channel closed unexpectedly");

    assert_eq!(received.channel_id, 2002);
    assert_eq!(received.point_id, 99);
    assert_eq!(received.value(), 88.0);
    assert_eq!(received.timestamp_ms, 1_700_000_002);
    assert_eq!(
        received.get_point_type(),
        Some(PointType::Adjustment),
        "Point type should be Adjustment"
    );

    // Cleanup
    listener_handle.abort();
    let _ = std::fs::remove_file(test_uds_path);
}

/// Test 2: End-to-end latency verification < 5ms
///
/// Sends 100 notifications and verifies average latency is within 5ms
#[tokio::test]
async fn test_uds_latency_under_5ms() {
    let test_uds_path = "/tmp/aether-latency-test.sock";
    let _ = std::fs::remove_file(test_uds_path);

    let listener = UnixListener::bind(test_uds_path).unwrap();
    let (tx, mut rx) = mpsc::channel::<Instant>(100);

    let listener_handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; ShmNotification::SIZE];
        while stream.read_exact(&mut buf).await.is_ok() {
            if tx.send(Instant::now()).await.is_err() {
                break;
            }
        }
    });

    // Wait for listener to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    let mut notifier = ShmNotifier::connect(test_uds_path).await.unwrap();
    assert!(notifier.is_connected());

    // Send 100 notifications and measure latency
    let mut latencies = vec![];
    for i in 0..100u32 {
        let start = Instant::now();
        let result = notifier
            .notify(
                1001,
                PointType::Adjustment,
                i,
                i as f64,
                1_700_000_000 + i as u64,
                1_700_005_000 + i as u64,
            )
            .await;
        assert!(result.uds_sent, "UDS notification should be sent");
        let recv_time = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("Timeout")
            .expect("Channel closed");
        latencies.push(recv_time.duration_since(start));
    }

    let avg_latency = latencies.iter().sum::<Duration>() / latencies.len() as u32;
    let max_latency = latencies.iter().max().unwrap();
    let min_latency = latencies.iter().min().unwrap();

    println!("=== UDS Latency Results ===");
    println!("  Samples: {}", latencies.len());
    println!("  Min latency: {:?}", min_latency);
    println!("  Avg latency: {:?}", avg_latency);
    println!("  Max latency: {:?}", max_latency);

    assert!(
        avg_latency < Duration::from_millis(5),
        "Average latency {:?} exceeded 5ms threshold",
        avg_latency
    );

    listener_handle.abort();
    let _ = std::fs::remove_file(test_uds_path);
}

/// Test 3: Degradation scenario - non-blocking when UDS is unavailable
///
/// When UDS connection fails, ShmNotifier should degrade gracefully without panicking
#[tokio::test]
async fn test_uds_graceful_degradation() {
    // Connect to a non-existent UDS path
    let notifier = ShmNotifier::connect("/tmp/nonexistent-socket-12345.sock")
        .await
        .unwrap();

    // Should return a successful but unconnected notifier
    assert!(
        !notifier.is_connected(),
        "Should not be connected to non-existent socket"
    );

    // Sending notifications should silently succeed (stream is None, uses fallback/disabled)
    let mut notifier = notifier;
    let result = notifier
        .notify(
            1001,
            PointType::Control,
            1,
            1.0,
            1_700_000_003,
            1_700_005_003,
        )
        .await;
    // When not connected, UDS is disabled - notify still "succeeds" but doesn't send
    assert!(
        !result.uds_sent || result.disabled,
        "notify() should indicate not sent or disabled when not connected"
    );
}

/// Test 4: Batch notification sending
///
/// Verifies correctness of sending multiple notifications in batch
#[tokio::test]
async fn test_uds_batch_notifications() {
    let test_uds_path = "/tmp/aether-batch-test.sock";
    let _ = std::fs::remove_file(test_uds_path);

    let listener = UnixListener::bind(test_uds_path).unwrap();
    let (tx, mut rx) = mpsc::channel::<ShmNotification>(100);

    let listener_handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; ShmNotification::SIZE];
        while stream.read_exact(&mut buf).await.is_ok() {
            let notif = ShmNotification::from_bytes(&buf);
            if tx.send(notif).await.is_err() {
                break;
            }
        }
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    let mut notifier = ShmNotifier::connect(test_uds_path).await.unwrap();

    // Send 50 notifications in batch
    let batch_size = 50u32;
    for i in 0..batch_size {
        let point_type = if i % 2 == 0 {
            PointType::Control
        } else {
            PointType::Adjustment
        };
        let result = notifier
            .notify(
                1000 + i,
                point_type,
                i * 10,
                i as f64,
                1_700_000_100 + i as u64,
                1_700_005_100 + i as u64,
            )
            .await;
        assert!(result.uds_sent, "UDS notification {} should be sent", i);
    }

    // Verify reception
    let mut received_count = 0u32;
    while let Ok(Some(notif)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
        assert_eq!(notif.channel_id, 1000 + received_count);
        assert_eq!(notif.point_id, received_count * 10);
        received_count += 1;
        if received_count >= batch_size {
            break;
        }
    }

    assert_eq!(
        received_count, batch_size,
        "Should receive all {} notifications",
        batch_size
    );

    listener_handle.abort();
    let _ = std::fs::remove_file(test_uds_path);
}

/// Test 5: ShmNotification serialization/deserialization
///
/// Verifies correctness of the fixed-size notification protocol
#[test]
fn test_notification_serialization() {
    let notification = ShmNotification::new(
        12345,
        PointType::Control,
        67890,
        321.5,
        1_700_000_010,
        1_700_005_010,
        77,
        9,
    );

    // Verify fields
    assert_eq!(notification.channel_id, 12345);
    assert_eq!(notification.point_id, 67890);
    assert_eq!(notification.value(), 321.5);
    assert_eq!(notification.timestamp_ms, 1_700_000_010);
    assert_eq!(notification.expires_at_ms, 1_700_005_010);
    assert_eq!(notification.producer_id, 77);
    assert_eq!(notification.seq, 9);
    assert_eq!(notification.get_point_type(), Some(PointType::Control));

    // Serialize
    let bytes = notification.to_bytes();
    assert_eq!(bytes.len(), ShmNotification::SIZE);
    assert_eq!(bytes.len(), 56, "Notification should be exactly 56 bytes");

    // Deserialize
    let restored = ShmNotification::from_bytes(&bytes);
    assert_eq!(restored.channel_id, 12345);
    assert_eq!(restored.point_id, 67890);
    assert_eq!(restored.value(), 321.5);
    assert_eq!(restored.timestamp_ms, 1_700_000_010);
    assert_eq!(restored.expires_at_ms, 1_700_005_010);
    assert_eq!(restored.producer_id, 77);
    assert_eq!(restored.seq, 9);
    assert_eq!(restored.get_point_type(), Some(PointType::Control));
}

/// Test 7: Serialization of different PointTypes
#[test]
fn test_notification_point_types() {
    // Control
    let control = ShmNotification::new(1, PointType::Control, 1, 1.0, 11, 12, 21, 31);
    assert_eq!(control.get_point_type(), Some(PointType::Control));

    // Adjustment
    let adjustment = ShmNotification::new(1, PointType::Adjustment, 1, 2.0, 12, 13, 22, 32);
    assert_eq!(adjustment.get_point_type(), Some(PointType::Adjustment));

    // Round-trip test
    let bytes = control.to_bytes();
    let restored = ShmNotification::from_bytes(&bytes);
    assert_eq!(restored.get_point_type(), Some(PointType::Control));

    let bytes = adjustment.to_bytes();
    let restored = ShmNotification::from_bytes(&bytes);
    assert_eq!(restored.get_point_type(), Some(PointType::Adjustment));
}
