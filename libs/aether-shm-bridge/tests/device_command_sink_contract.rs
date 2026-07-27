#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_dataplane::{AuthorityWriteGuard, SlotIo, SlotWriter};
use aether_domain::{
    ChannelCommandAddress, ChannelId, CommandId, PhysicalDeviceCommand, PointId, PointKind,
    TimestampMs,
};
use aether_ports::{DeviceCommandSink, PortErrorKind};
use aether_shm_bridge::{
    ChannelPointManifest, CommandMirrorObserver, PhysicalPointAddress, ShmDeviceCommandSink,
};
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn command(kind: PointKind, point_id: u32, value: f64) -> PhysicalDeviceCommand {
    let issued_at = now_ms();
    PhysicalDeviceCommand::new(
        CommandId::new(91),
        ChannelCommandAddress::new(ChannelId::new(7), kind, PointId::new(point_id))
            .expect("command-owned address"),
        value,
        TimestampMs::new(issued_at),
        TimestampMs::new(issued_at + 5_000),
    )
    .expect("physical command")
}

fn generation(directory: &tempfile::TempDir) -> (Arc<SlotWriter>, Arc<ChannelPointManifest>) {
    let manifest = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 1, 1])]));
    let writer = Arc::new(
        SlotWriter::create(
            directory.path().join("commands.shm"),
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create command SHM"),
    );
    (writer, manifest)
}

#[tokio::test]
async fn unknown_command_slot_is_rejected_before_any_shm_write() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(Arc::clone(&writer), Arc::clone(&manifest))
        .expect("publish generation");

    let error = sink
        .send(command(PointKind::Action, 9, 42.0))
        .await
        .expect_err("unknown A slot must fail");

    assert_eq!(error.kind(), PortErrorKind::NotFound);
    let known_slot = manifest
        .slot_for(PhysicalPointAddress::from_legacy_raw(
            7,
            PointKind::Action,
            0,
        ))
        .expect("known action slot");
    assert!(
        writer
            .read_slot(known_slot)
            .expect("known action sample")
            .value
            .is_nan(),
        "failed resolution must perform zero SHM writes"
    );
}

#[tokio::test]
async fn uds_degradation_is_typed_and_never_returns_an_acceptance_receipt() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(Arc::clone(&writer), Arc::clone(&manifest))
        .expect("publish generation");
    sink.configure_notifier(directory.path().join("missing.sock"))
        .await
        .expect("configure self-healing notifier");

    let error = sink
        .send(command(PointKind::Command, 0, 12.5))
        .await
        .expect_err("failed UDS must not report accepted");

    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    let slot = manifest
        .slot_for(PhysicalPointAddress::from_legacy_raw(
            7,
            PointKind::Command,
            0,
        ))
        .expect("known command slot");
    assert_eq!(
        writer.read_slot(slot).expect("mirrored command").value,
        12.5,
        "SHM-before-UDS ordering remains part of the protocol"
    );
}

#[tokio::test]
async fn successful_send_preserves_the_existing_56_byte_command_wire() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let (writer, manifest) = generation(&directory);
    let canonical = writer.path().clone();
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(AssertCommandLeaseHeld { canonical }));
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");
    let physical = command(PointKind::Action, 0, -3.25);

    let receive = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        let mut bytes = [0_u8; 56];
        stream
            .read_exact(&mut bytes)
            .await
            .expect("read command frame");
        bytes
    });
    let receipt = sink
        .send(physical)
        .await
        .expect("transport accepted command");
    let bytes = receive.await.expect("join listener");

    assert_eq!(receipt.command_id(), physical.id());
    assert_eq!(
        u32::from_ne_bytes(bytes[0..4].try_into().expect("channel")),
        7
    );
    assert_eq!(
        u32::from_ne_bytes(bytes[4..8].try_into().expect("point")),
        0
    );
    assert_eq!(bytes[8], 3, "Action retains legacy Adjustment wire code");
    assert_eq!(&bytes[9..16], &[0; 7]);
    assert_eq!(
        f64::from_bits(u64::from_ne_bytes(bytes[16..24].try_into().expect("value"))),
        -3.25
    );
    assert_eq!(
        u64::from_ne_bytes(bytes[24..32].try_into().expect("issued")),
        physical.issued_at().get()
    );
    assert_eq!(
        u64::from_ne_bytes(bytes[32..40].try_into().expect("expires")),
        physical.expires_at().get()
    );
    assert_ne!(
        u64::from_ne_bytes(bytes[40..48].try_into().expect("producer")),
        0
    );
    assert_eq!(
        u64::from_ne_bytes(bytes[48..56].try_into().expect("sequence")),
        1
    );
}

struct AssertCommandLeaseHeld {
    canonical: PathBuf,
}

impl CommandMirrorObserver for AssertCommandLeaseHeld {
    fn after_shm_write(&self, _command: PhysicalDeviceCommand, _slot: usize) {}

    fn after_transport_write(&self, _command: PhysicalDeviceCommand) {
        assert!(
            AuthorityWriteGuard::try_acquire(&self.canonical)
                .expect("try exclusive replacement lease")
                .is_none(),
            "command must retain its shared lease through transport and receipt formation"
        );
    }
}

struct SwapGenerationAfterWrite {
    writer: Arc<SlotWriter>,
}

impl CommandMirrorObserver for SwapGenerationAfterWrite {
    fn after_shm_write(&self, _command: PhysicalDeviceCommand, _slot: usize) {
        self.writer
            .header()
            .writer_generation
            .fetch_add(2, Ordering::AcqRel);
    }
}

#[tokio::test]
async fn generation_change_after_shm_write_fails_closed_and_triggers_rebuild() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(SwapGenerationAfterWrite {
        writer: Arc::clone(&writer),
    }));
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    let rebuild = sink.rebuild_trigger();

    let error = sink
        .send(command(PointKind::Action, 0, 8.0))
        .await
        .expect_err("generation swap must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
        .await
        .expect("generation mismatch must request rebuild");
    assert!(!sink.is_writer_available());
}

#[tokio::test]
async fn generation_change_before_shm_write_fails_closed_and_triggers_rebuild() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(Arc::clone(&writer), Arc::clone(&manifest))
        .expect("publish generation");
    writer
        .header()
        .writer_generation
        .fetch_add(2, Ordering::AcqRel);
    let rebuild = sink.rebuild_trigger();

    let error = sink
        .send(command(PointKind::Action, 0, 8.0))
        .await
        .expect_err("stale generation must fail before write");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
        .await
        .expect("generation mismatch must request rebuild");
    let slot = manifest
        .slot_for(PhysicalPointAddress::from_legacy_raw(
            7,
            PointKind::Action,
            0,
        ))
        .expect("action slot");
    assert!(writer.read_slot(slot).expect("action value").value.is_nan());
}

struct SlowMirrorObserver;

impl CommandMirrorObserver for SlowMirrorObserver {
    fn after_shm_write(&self, _command: PhysicalDeviceCommand, _slot: usize) {
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[tokio::test]
async fn command_expiring_after_shm_mirror_is_rejected_before_wire_send() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("expiry.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(SlowMirrorObserver));
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");
    let issued_at = now_ms();
    let expiring = PhysicalDeviceCommand::new(
        CommandId::new(92),
        ChannelCommandAddress::new(ChannelId::new(7), PointKind::Action, PointId::new(0))
            .expect("action address"),
        4.0,
        TimestampMs::new(issued_at),
        TimestampMs::new(issued_at + 10),
    )
    .expect("short-lived command");

    let error = sink
        .send(expiring)
        .await
        .expect_err("expired command must not reach the wire");
    assert_eq!(error.kind(), PortErrorKind::Rejected);

    let (mut stream, _) = listener.accept().await.expect("accept configured notifier");
    let mut bytes = [0_u8; 56];
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.read_exact(&mut bytes))
            .await
            .is_err(),
        "no command frame may be sent after expiry"
    );
}

#[test]
fn reloadable_manifest_source_tracks_each_published_generation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let sink = ShmDeviceCommandSink::new();
    let source = sink.manifest_source();
    let first = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 1, 1])]));
    let first_writer = Arc::new(
        SlotWriter::create(
            directory.path().join("first.shm"),
            16,
            first.slot_count(),
            first.layout_hash(),
        )
        .expect("first generation"),
    );
    sink.publish_generation(first_writer, Arc::clone(&first))
        .expect("publish first generation");
    assert_eq!(
        source.load().expect("first manifest").layout_hash(),
        first.layout_hash()
    );

    let second = Arc::new(ChannelPointManifest::from_entries([
        (7, [1, 0, 1, 1]),
        (9, [2, 0, 0, 1]),
    ]));
    let second_writer = Arc::new(
        SlotWriter::create(
            directory.path().join("second.shm"),
            32,
            second.slot_count(),
            second.layout_hash(),
        )
        .expect("second generation"),
    );
    sink.publish_generation(second_writer, Arc::clone(&second))
        .expect("publish second generation");

    let current = source.load().expect("latest manifest");
    assert_eq!(current.layout_hash(), second.layout_hash());
    assert!(
        current
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                9,
                PointKind::Action,
                0,
            ))
            .is_some()
    );
}

#[tokio::test]
async fn missing_writer_requests_rebuild_again_after_a_failed_reopen_cycle() {
    let sink = ShmDeviceCommandSink::new();
    let rebuild = sink.rebuild_trigger();

    for _ in 0..2 {
        let error = sink
            .send(command(PointKind::Action, 0, 8.0))
            .await
            .expect_err("missing writer must fail");
        assert_eq!(error.kind(), PortErrorKind::Unavailable);
        tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
            .await
            .expect("every later command can restart self-healing");
    }
}

#[tokio::test]
async fn canonical_inode_swap_invalidation_fails_closed_until_republished() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    let rebuild = sink.rebuild_trigger();

    sink.invalidate_and_rebuild();

    assert!(!sink.is_writer_available());
    tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
        .await
        .expect("inode swap must request reopen");
    let error = sink
        .send(command(PointKind::Action, 0, 8.0))
        .await
        .expect_err("commands must fail while the canonical path is reopening");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
}

struct ReplaceCanonicalAfterCommandMirror {
    staging: PathBuf,
    canonical: PathBuf,
}

impl CommandMirrorObserver for ReplaceCanonicalAfterCommandMirror {
    fn after_shm_write(&self, _command: PhysicalDeviceCommand, _slot: usize) {
        std::fs::rename(&self.staging, &self.canonical)
            .expect("atomically replace canonical command SHM");
    }
}

#[tokio::test]
async fn canonical_inode_replacement_after_command_mirror_fails_before_receipt() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let canonical = directory.path().join("canonical-commands.shm");
    let staging = directory.path().join("replacement-commands.shm");
    let manifest = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 1, 1])]));
    let old_writer = Arc::new(
        SlotWriter::create(
            &canonical,
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create old canonical generation"),
    );
    let replacement =
        SlotWriter::create(&staging, 16, manifest.slot_count(), manifest.layout_hash())
            .expect("create replacement generation");
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(ReplaceCanonicalAfterCommandMirror {
        staging,
        canonical: canonical.clone(),
    }));
    sink.publish_generation(old_writer, Arc::clone(&manifest))
        .expect("publish old generation");
    let rebuild = sink.rebuild_trigger();

    let error = sink
        .send(command(PointKind::Action, 0, 8.0))
        .await
        .expect_err("a command overlapping canonical replacement must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
        .await
        .expect("identity mismatch must request an immediate reopen");
    assert!(!sink.is_writer_available());
    let replacement_reader =
        SlotWriter::open_existing(&canonical, replacement.slot_count(), manifest.layout_hash())
            .expect("open replacement through canonical path");
    let slot = manifest
        .slot_for(PhysicalPointAddress::from_legacy_raw(
            7,
            PointKind::Action,
            0,
        ))
        .expect("action slot");
    assert!(
        replacement_reader
            .read_slot(slot)
            .expect("replacement action slot")
            .value
            .is_nan(),
        "the stale command mirror must not mutate the replacement authority"
    );
}

struct ReplaceCanonicalAfterTransport {
    staging: PathBuf,
    canonical: PathBuf,
}

impl CommandMirrorObserver for ReplaceCanonicalAfterTransport {
    fn after_shm_write(&self, _command: PhysicalDeviceCommand, _slot: usize) {}

    fn after_transport_write(&self, _command: PhysicalDeviceCommand) {
        std::fs::rename(&self.staging, &self.canonical)
            .expect("atomically replace canonical SHM after transport write");
    }
}

#[tokio::test]
async fn canonical_inode_replacement_after_transport_never_returns_receipt() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let canonical = directory.path().join("transport-canonical.shm");
    let staging = directory.path().join("transport-replacement.shm");
    let socket = directory.path().join("transport.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let manifest = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 1, 1])]));
    let old_writer = Arc::new(
        SlotWriter::create(
            &canonical,
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create old canonical generation"),
    );
    let _replacement =
        SlotWriter::create(&staging, 16, manifest.slot_count(), manifest.layout_hash())
            .expect("create replacement generation");
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(ReplaceCanonicalAfterTransport {
        staging,
        canonical,
    }));
    sink.publish_generation(old_writer, manifest)
        .expect("publish old generation");
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");
    let rebuild = sink.rebuild_trigger();

    let receive = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        let mut bytes = [0_u8; 56];
        stream
            .read_exact(&mut bytes)
            .await
            .expect("read complete command frame");
        bytes
    });
    let error = sink
        .send(command(PointKind::Action, 0, 8.0))
        .await
        .expect_err("canonical replacement must suppress the acceptance receipt");
    let bytes = receive.await.expect("join listener");

    assert_eq!(bytes.len(), 56, "wire framing must remain unchanged");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
        .await
        .expect("post-transport identity mismatch must request reopen");
    assert!(!sink.is_writer_available());
}
