#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointKind, PointQuality,
    TimestampMs,
};
use aether_shm_bridge::{
    ChannelPointManifest, PointWatchEventListener, PointWatchPublisher, ShmRuntimeConfig,
    ShmWriterHandle, SubscriptionBitmap,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn typed_acquisition_commit_emits_the_existing_point_watch_wire_frame() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("automation.sock");
    let shutdown = CancellationToken::new();
    let (listener, mut events) = PointWatchEventListener::new(&socket, shutdown.clone());
    let listener_task = tokio::spawn(listener.run());
    for _ in 0..20 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory().expect("in-memory bitmap"));
    bitmap.set_watched(0);
    let (publisher, publisher_task) =
        PointWatchPublisher::new_with_fanout(vec![(bitmap, socket)], shutdown.clone());
    let manifest = Arc::new(ChannelPointManifest::from_map(BTreeMap::from([(
        7,
        [1, 0, 0, 0],
    )])));
    let handle = ShmWriterHandle::create_published_with_observer(
        ShmRuntimeConfig::new(directory.path().join("aether.shm"), 8),
        Arc::clone(&manifest),
        None,
        Some(publisher),
    )
    .expect("publish SHM generation");
    let address =
        ChannelPointAddress::new(ChannelId::new(7), PointKind::Telemetry, PointId::new(0))
            .expect("acquisition address");
    let sample = AcquiredPointSample::new(
        address,
        12.5,
        125.0,
        TimestampMs::new(4_200),
        PointQuality::Good,
    )
    .expect("sample");
    handle
        .generation()
        .expect("generation")
        .acquisition_writer()
        .commit_batch(&[sample])
        .expect("commit sample");

    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("point-watch delivery timeout")
        .expect("listener channel");
    assert_eq!(event.channel_id(), 7);
    assert_eq!(event.point_kind(), Some(PointKind::Telemetry));
    assert_eq!(event.point_id(), 0);
    assert_eq!(event.slot_index(), 0);
    assert_eq!(event.value(), 12.5);
    assert_eq!(event.raw(), 125.0);
    assert_eq!(event.timestamp_ms(), 4_200);
    assert!(event.matches_manifest(&manifest));

    shutdown.cancel();
    listener_task
        .await
        .expect("listener task")
        .expect("listener");
    publisher_task.await.expect("publisher task");
}
