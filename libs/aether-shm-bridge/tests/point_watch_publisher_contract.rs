#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointKind, PointQuality,
    TimestampMs,
};
use aether_shm_bridge::{
    AcquisitionCommitObserver, ChannelPointManifest, PointWatchEventListener, PointWatchPublisher,
    ShmRuntimeConfig, ShmWriterHandle, SubscriptionBitmap,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn typed_acquisition_commit_emits_a_compact_point_watch_hint() {
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

    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory(8).expect("in-memory bitmap"));
    bitmap.set_watched(0).expect("subscribe slot");
    let (publisher, publisher_task) =
        PointWatchPublisher::new_with_fanout(vec![(bitmap, socket)], shutdown.clone());
    let manifest = Arc::new(ChannelPointManifest::dense_test_fixture(BTreeMap::from([
        (7, [1, 0, 0, 0]),
    ])));
    let handle = ShmWriterHandle::create(
        ShmRuntimeConfig::new(directory.path().join("aether.shm"), 8),
        Arc::clone(&manifest),
        None,
        Some(publisher),
        1,
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
    assert!(event.matches_manifest(&manifest));

    shutdown.cancel();
    listener_task
        .await
        .expect("listener task")
        .expect("listener");
    publisher_task.await.expect("publisher task");
}

#[tokio::test]
async fn prepared_point_watch_does_not_start_socket_work_before_spawn() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("prepared.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind consumer socket");
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory(1).expect("in-memory bitmap"));
    bitmap.set_watched(0).expect("subscribe slot");
    let (publisher, prepared) = PointWatchPublisher::prepare_with_fanout(vec![(bitmap, socket)]);
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
    publisher.point_committed(0, sample);

    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        "preparation must not connect or spawn a drain task"
    );

    let shutdown = CancellationToken::new();
    let drain = prepared.spawn(shutdown.clone());
    tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .expect("prepared drain did not start")
        .expect("accept prepared drain");
    shutdown.cancel();
    drain.await.expect("prepared drain task");
}

#[tokio::test]
async fn aborting_the_aggregate_point_watch_task_does_not_detach_socket_drains() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("owned-drain.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind consumer socket");
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory(1).expect("in-memory bitmap"));
    bitmap.set_watched(0).expect("subscribe slot");
    let (publisher, prepared) = PointWatchPublisher::prepare_with_fanout(vec![(bitmap, socket)]);
    let address =
        ChannelPointAddress::new(ChannelId::new(7), PointKind::Telemetry, PointId::new(0))
            .expect("acquisition address");
    publisher.point_committed(
        0,
        AcquiredPointSample::new(
            address,
            12.5,
            125.0,
            TimestampMs::new(4_200),
            PointQuality::Good,
        )
        .expect("sample"),
    );

    let aggregate = prepared.spawn(CancellationToken::new());
    let (mut peer, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .expect("point-watch connection timeout")
        .expect("accept point-watch drain");
    aggregate.abort();
    let _ = aggregate.await;

    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut received))
        .await
        .expect("owned socket drain remained detached after aggregate abort")
        .expect("read socket EOF");
}
