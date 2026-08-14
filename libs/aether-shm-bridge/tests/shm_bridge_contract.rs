use std::sync::Arc;
use std::time::Duration;

use aether_domain::PointKind;
use aether_shm_bridge::{
    ChannelHealthManifest, ChannelPointManifest, PhysicalPointAddress, PointWatchEvent,
    PointWatchEventListener, ReconnectingSlotSource, ShmChannelHealthReader,
    ShmChannelHealthWriterHandle, ShmClientConfig, SlotSource, channel_health_path_from_shm,
    point_watch_socket_for_consumer,
};

#[test]
fn channel_manifest_preserves_only_the_writer_ownership_padding() {
    let manifest = ChannelPointManifest::dense_test_fixture([(1, [3, 0, 1, 1]), (2, [1, 1, 0, 0])]);

    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::from_raw_ids(
            1,
            PointKind::Telemetry,
            0,
        )),
        Some(0)
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::from_raw_ids(1, PointKind::Command, 0,)),
        Some(4)
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::from_raw_ids(1, PointKind::Action, 0,)),
        Some(5)
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::from_raw_ids(
            2,
            PointKind::Telemetry,
            0,
        )),
        Some(6)
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::from_raw_ids(2, PointKind::Status, 0,)),
        Some(7)
    );
    assert_eq!(manifest.slot_count(), 8);
    assert_ne!(manifest.layout_hash(), 0);
}

fn write_managed_shm(path: &std::path::Path, layout_hash: u64, generation: u64, value: f64) {
    let mut image = vec![0_u8; aether_dataplane::calculate_file_size(1)];
    image[0..8].copy_from_slice(&aether_dataplane::AETHER_SHM_MAGIC.to_ne_bytes());
    image[12..16].copy_from_slice(&1_u32.to_ne_bytes());
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    image[16..24].copy_from_slice(&now_ms.to_ne_bytes());
    image[24..32].copy_from_slice(&layout_hash.to_ne_bytes());
    image[32..40].copy_from_slice(&generation.to_ne_bytes());
    image[40..48].copy_from_slice(&1_u64.to_ne_bytes());
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
fn channel_health_manifest_is_dense_and_order_independent() {
    let first = ChannelHealthManifest::compile([20, 3], 2).expect("dense health manifest");
    let second = ChannelHealthManifest::compile([3, 20], 2).expect("canonical health manifest");

    assert_eq!(first.slot_count(), 2);
    assert_eq!(first.slot_for(3), Some(0));
    assert_eq!(first.slot_for(20), Some(1));
    assert!(first.contains(3));
    assert!(!first.contains(4));
    assert_eq!(first.layout_hash(), second.layout_hash());
    assert!(ChannelHealthManifest::compile([3, 3], 2).is_err());
    assert!(ChannelHealthManifest::compile([3, 20], 1).is_err());
    assert_eq!(
        channel_health_path_from_shm(std::path::Path::new("/dev/shm/aether-live-state.shm")),
        std::path::PathBuf::from("/dev/shm/aether-live-state-health.shm")
    );
}

#[test]
fn channel_health_roundtrips_without_redis() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("health.shm");
    let manifest = Arc::new(ChannelHealthManifest::test_fixture([10, 20]));
    let writer = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&manifest), 1)
        .expect("create channel health writer");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    writer
        .set_online(10, true, now_ms)
        .expect("write online state");
    writer.update_heartbeat(now_ms).expect("publish heartbeat");

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
    assert_eq!(health.observed_at().get(), now_ms);
    assert_eq!(reader.read_channel(20).expect("unknown state"), None);
    assert_eq!(reader.read_channel(99).expect("unconfigured channel"), None);
}

#[test]
fn channel_health_reader_reopens_after_writer_process_restart() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("restart-health.shm");
    let manifest = Arc::new(ChannelHealthManifest::test_fixture([10]));
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    let first_writer = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&manifest), 10)
        .expect("create first health generation");
    first_writer
        .set_online(10, true, now_ms)
        .expect("write first generation");
    first_writer
        .update_heartbeat(now_ms)
        .expect("publish first heartbeat");
    let reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&path, manifest.layout_hash())
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(60)),
        Arc::clone(&manifest),
    );
    assert!(reader.read_channel(10).unwrap().unwrap().online());

    let second_writer = ShmChannelHealthWriterHandle::create(&path, manifest, 11)
        .expect("atomically publish second health generation");
    second_writer
        .set_online(10, false, now_ms + 1)
        .expect("write second generation");
    second_writer
        .update_heartbeat(now_ms)
        .expect("publish second heartbeat");

    let reopened = reader
        .read_channel(10)
        .expect("reader reopens canonical health path")
        .expect("second generation state");
    assert!(!reopened.online());
    assert_eq!(reopened.observed_at().get(), now_ms + 1);
}

#[test]
fn point_watch_wire_frame_is_explicit_little_endian() {
    let event = PointWatchEvent::new(10, PointKind::Telemetry, 7, 42)
        .expect("slot fits the compact wire frame");

    let bytes = event.to_bytes();
    let decoded = PointWatchEvent::from_bytes(&bytes).expect("decode compact event");

    assert_eq!(PointWatchEvent::SIZE, 16);
    assert_eq!(decoded, event);
    assert_eq!(&bytes[0..4], &10_u32.to_le_bytes());
    assert_eq!(&bytes[8..12], &42_u32.to_le_bytes());
    assert_eq!(bytes[12], 0);
    assert_eq!(&bytes[13..16], &[0xA5, 0, 0x5A]);
    assert_eq!(decoded.slot_index(), 42);
    assert_eq!(
        point_watch_socket_for_consumer("alarm"),
        aether_shm_bridge::default_shm_path()
            .parent()
            .expect("SHM parent")
            .join("aether-point-watch-alarm.sock")
    );
}

#[test]
fn point_watch_rejects_invalid_magic_reserved_bytes_and_unknown_kind() {
    let invalid_magic = [0_u8; PointWatchEvent::SIZE];
    assert!(PointWatchEvent::from_bytes(&invalid_magic).is_err());

    let mut nonzero_reserved = PointWatchEvent::new(10, PointKind::Telemetry, 7, 42)
        .expect("event")
        .to_bytes();
    nonzero_reserved[14] = 1;
    assert!(PointWatchEvent::from_bytes(&nonzero_reserved).is_err());

    let mut unknown_kind = PointWatchEvent::new(10, PointKind::Telemetry, 7, 42)
        .expect("event")
        .to_bytes();
    unknown_kind[12] = u8::MAX;
    assert!(PointWatchEvent::from_bytes(&unknown_kind).is_err());
}

#[test]
fn point_watch_event_matches_only_its_typed_address_in_the_current_manifest() {
    let manifest = ChannelPointManifest::dense_test_fixture([(10, [2, 1, 0, 0])]);
    let current = PointWatchEvent::new(10, PointKind::Telemetry, 1, 1).expect("current event");
    let stale_slot =
        PointWatchEvent::new(10, PointKind::Telemetry, 1, 2).expect("stale-slot event");
    let stale_kind = PointWatchEvent::new(10, PointKind::Status, 1, 1).expect("stale-kind event");

    assert!(current.matches_manifest(&manifest));
    assert!(!stale_slot.matches_manifest(&manifest));
    assert!(!stale_kind.matches_manifest(&manifest));
}

#[tokio::test]
async fn point_watch_listener_delivers_hints_on_an_isolated_socket() {
    use std::os::unix::fs::PermissionsExt as _;
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
    let socket_mode = std::fs::symlink_metadata(&socket)
        .expect("PointWatch socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600, "PointWatch socket must be owner-only");
    let event = PointWatchEvent::new(10, PointKind::Status, 3, 8).expect("wire event");
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

#[cfg(target_pointer_width = "64")]
#[test]
fn point_watch_event_rejects_a_slot_wider_than_its_wire_field() {
    let oversized = usize::try_from(u64::from(u32::MAX) + 1).expect("64-bit usize");

    let error = PointWatchEvent::new(10, PointKind::Telemetry, 7, oversized)
        .expect_err("slot conversion must never truncate");

    assert_eq!(error.kind(), aether_ports::PortErrorKind::InvalidData);
}
