use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{InstanceId, PointAddress, PointId, PointKind, PointQuality, TimestampMs};
use aether_ports::LiveState;
use aether_shm_bridge::{
    ChannelHealthManifest, ChannelPointManifest, PointSlotResolver, PointWatchEvent,
    PointWatchEventListener, ReconnectingSlotSource, ShmChannelHealthReader,
    ShmChannelHealthWriter, ShmClientConfig, ShmLiveState, SlotSnapshot, SlotSource,
    StaticSlotResolver, channel_health_path_from_shm, point_watch_socket_for_consumer,
};

#[derive(Debug)]
struct StubSlots {
    slots: Vec<Option<SlotSnapshot>>,
}

impl SlotSource for StubSlots {
    fn slot_count(&self) -> aether_ports::PortResult<usize> {
        Ok(self.slots.len())
    }

    fn read_slot(&self, index: usize) -> aether_ports::PortResult<Option<SlotSnapshot>> {
        Ok(self.slots.get(index).copied().flatten())
    }
}

fn address(point_id: u32) -> PointAddress {
    PointAddress::new(
        InstanceId::new(10),
        PointKind::Telemetry,
        PointId::new(point_id),
    )
}

fn bridge(
    slots: Vec<Option<SlotSnapshot>>,
    mappings: impl IntoIterator<Item = (PointAddress, usize)>,
) -> ShmLiveState {
    ShmLiveState::new(
        Arc::new(StubSlots { slots }),
        Arc::new(StaticSlotResolver::from_entries(mappings)),
    )
}

#[tokio::test]
async fn mapped_legacy_slot_is_exposed_as_a_domain_sample() {
    let state = bridge(
        vec![Some(SlotSnapshot::new(48.5, 1_500))],
        [(address(7), 0)],
    );

    let sample = state.read(address(7)).await.unwrap().unwrap();
    assert_eq!(sample.address(), address(7));
    assert_eq!(sample.value(), 48.5);
    assert_eq!(sample.timestamp(), TimestampMs::new(1_500));
    assert_eq!(sample.quality(), PointQuality::Good);
}

#[tokio::test]
async fn missing_mapping_and_unwritten_nan_are_absent_values() {
    let state = bridge(
        vec![Some(SlotSnapshot::new(f64::NAN, 0))],
        [(address(7), 0)],
    );

    assert_eq!(state.read(address(8)).await.unwrap(), None);
    assert_eq!(state.read(address(7)).await.unwrap(), None);
}

#[tokio::test]
async fn seqlock_contention_is_retryable_but_invalid_mapping_is_not() {
    let contended = bridge(vec![None], [(address(7), 0)]);
    let contention = contended
        .read(address(7))
        .await
        .expect_err("empty resolved slot represents a torn read");
    assert!(contention.is_retryable());

    let invalid = bridge(vec![], [(address(7), 1)]);
    let invalid_mapping = invalid
        .read(address(7))
        .await
        .expect_err("out-of-bounds slot mapping is invalid");
    assert!(!invalid_mapping.is_retryable());
}

#[test]
fn resolver_trait_is_independent_of_legacy_routing_types() {
    let resolver = StaticSlotResolver::from_entries([(address(7), 3)]);
    assert_eq!(resolver.resolve(address(7)), Some(3));
    assert_eq!(resolver.resolve(address(8)), None);

    let domain_map: HashMap<PointAddress, usize> = [(address(9), 4)].into_iter().collect();
    let resolver = StaticSlotResolver::from_map(domain_map);
    assert_eq!(resolver.resolve(address(9)), Some(4));
}

#[test]
fn channel_manifest_preserves_deterministic_padded_layout() {
    let manifest = ChannelPointManifest::from_entries([(1, [3, 0, 1, 1]), (2, [1, 1, 0, 0])]);

    assert_eq!(manifest.slot(1, PointKind::Telemetry, 0), Some(0));
    assert_eq!(manifest.slot(1, PointKind::Command, 0), Some(4));
    assert_eq!(manifest.slot(1, PointKind::Action, 0), Some(5));
    assert_eq!(manifest.slot(2, PointKind::Telemetry, 0), Some(6));
    assert_eq!(manifest.slot(2, PointKind::Status, 0), Some(7));
    assert_eq!(manifest.slot_count(), 8);
    assert_ne!(manifest.layout_hash(), 0);
}

fn write_managed_shm(path: &std::path::Path, layout_hash: u64, generation: u64, value: f64) {
    let mut image = vec![0_u8; aether_dataplane::calculate_file_size(1)];
    image[0..8].copy_from_slice(&aether_dataplane::UNIFIED_MAGIC.to_ne_bytes());
    image[8..12].copy_from_slice(&aether_dataplane::UNIFIED_VERSION.to_ne_bytes());
    image[12..16].copy_from_slice(&1_u32.to_ne_bytes());
    image[16..20].copy_from_slice(&1_u32.to_ne_bytes());
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    image[32..40].copy_from_slice(&now_ms.to_ne_bytes());
    image[40..48].copy_from_slice(&layout_hash.to_ne_bytes());
    image[48..56].copy_from_slice(&generation.to_ne_bytes());
    image[64..72].copy_from_slice(&value.to_bits().to_ne_bytes());
    image[72..80].copy_from_slice(&now_ms.to_ne_bytes());
    image[80..88].copy_from_slice(&value.to_bits().to_ne_bytes());
    std::fs::write(path, image).expect("write managed SHM image");
}

#[test]
fn managed_source_reopens_after_atomic_generation_swap() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("current.shm");
    let replacement = dir.path().join("replacement.shm");
    write_managed_shm(&path, 77, 2, 10.0);

    let source = ReconnectingSlotSource::new(
        ShmClientConfig::new(&path, 77)
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(60)),
    );
    assert_eq!(
        source
            .read_slot(0)
            .expect("first read")
            .expect("first slot")
            .value(),
        10.0
    );

    write_managed_shm(&replacement, 77, 4, 20.0);
    std::fs::rename(&replacement, &path).expect("atomically replace SHM image");

    assert_eq!(
        source
            .read_slot(0)
            .expect("read after swap")
            .expect("replacement slot")
            .value(),
        20.0
    );
}

#[test]
fn managed_source_classifies_missing_writer_as_retryable() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let source = ReconnectingSlotSource::new(ShmClientConfig::new(
        dir.path().join("not-created-yet.shm"),
        77,
    ));

    let error = source.read_slot(0).expect_err("missing writer must fail");

    assert!(error.is_retryable());
}

#[test]
fn channel_health_manifest_is_sparse_and_order_independent() {
    let first = ChannelHealthManifest::from_channel_ids([20, 3, 20]);
    let second = ChannelHealthManifest::from_channel_ids([3, 20]);

    assert_eq!(first.slot_count(), 21);
    assert!(first.contains(3));
    assert!(!first.contains(4));
    assert_eq!(first.layout_hash(), second.layout_hash());
    assert_eq!(
        channel_health_path_from_shm(std::path::Path::new("/dev/shm/aether-rtdb.shm")),
        std::path::PathBuf::from("/dev/shm/aether-rtdb-health.shm")
    );
}

#[test]
fn channel_health_roundtrips_without_redis() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("health.shm");
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([10, 20]));
    let writer = ShmChannelHealthWriter::create(&path, Arc::clone(&manifest))
        .expect("create channel health writer");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    writer
        .set_online(10, true, now_ms)
        .expect("write online state");

    let reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&path, manifest.layout_hash())
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(60)),
        manifest,
    );
    let health = reader
        .read_channel(10)
        .expect("read channel health")
        .expect("known online state");

    assert!(health.online());
    assert_eq!(health.timestamp_ms(), now_ms);
    assert_eq!(reader.read_channel(20).expect("unknown state"), None);
    assert_eq!(reader.read_channel(99).expect("unconfigured channel"), None);
}

#[test]
fn channel_health_reader_reopens_after_writer_process_restart() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("restart-health.shm");
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([10]));
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    let first_writer = ShmChannelHealthWriter::create(&path, Arc::clone(&manifest))
        .expect("create first health generation");
    first_writer
        .set_online(10, true, now_ms)
        .expect("write first generation");
    let reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&path, manifest.layout_hash())
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(60)),
        Arc::clone(&manifest),
    );
    assert!(reader.read_channel(10).unwrap().unwrap().online());

    let second_writer = ShmChannelHealthWriter::create(&path, manifest)
        .expect("atomically publish second health generation");
    second_writer
        .set_online(10, false, now_ms + 1)
        .expect("write second generation");

    let reopened = reader
        .read_channel(10)
        .expect("reader reopens canonical health path")
        .expect("second generation state");
    assert!(!reopened.online());
    assert_eq!(reopened.timestamp_ms(), now_ms + 1);
}

#[test]
fn point_watch_wire_frame_is_explicit_little_endian() {
    let event = PointWatchEvent::new(10, PointKind::Telemetry, 7, 42, 12.5, 125.0, 1_000, 99);

    let bytes = event.to_bytes();
    let decoded = PointWatchEvent::from_bytes(&bytes);

    assert_eq!(decoded, event);
    assert_eq!(&bytes[0..4], &10_u32.to_le_bytes());
    assert_eq!(decoded.value(), 12.5);
    assert_eq!(decoded.slot_index(), 42);
    assert_eq!(
        point_watch_socket_for_consumer("alarm"),
        aether_shm_bridge::default_shm_path()
            .parent()
            .expect("SHM parent")
            .join("aether-point-watch-alarm.sock")
    );
}

#[tokio::test]
async fn point_watch_listener_delivers_hints_on_an_isolated_socket() {
    use tokio::io::AsyncWriteExt;

    let socket = std::path::PathBuf::from(format!(
        "/tmp/aether-shm-bridge-pw-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let (listener, mut events) = PointWatchEventListener::new(&socket, shutdown.clone());
    let mut task = tokio::spawn(listener.run());

    let mut stream = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if task.is_finished() {
                let outcome = (&mut task).await;
                panic!("listener exited before accepting a connection: {outcome:?}");
            }
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(stream) => break stream,
                Err(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("listener bind timeout");
    let event = PointWatchEvent::new(10, PointKind::Status, 3, 8, 1.0, 1.0, 2_000, 100);
    stream
        .write_all(&event.to_bytes())
        .await
        .expect("write event frame");

    let received = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("event timeout")
        .expect("event channel open");
    assert_eq!(received, event);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("listener shutdown timeout")
        .expect("listener task joins")
        .expect("listener stops cleanly");
}
