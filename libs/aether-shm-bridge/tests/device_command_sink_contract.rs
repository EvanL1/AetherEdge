#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_dataplane::{AuthorityWriteGuard, SlotIo, SlotWriter};
use aether_domain::{
    ChannelCommandAddress, ChannelId, CommandId, PhysicalDeviceCommand, PointId, PointKind,
    TimestampMs,
};
use aether_ports::{DeviceCommandSink, PortErrorKind};
use aether_shm_bridge::{
    ChannelPointManifest, CommandAckStatus, CommandHello, CommandLedgerStateCode,
    CommandMirrorObserver, DeviceCommandAck, DeviceCommandFrame, PhysicalPointAddress,
    ShmDeviceCommandSink,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn advertise_ready(stream: &mut tokio::net::UnixStream) {
    stream
        .write_all(&CommandHello::new().to_bytes())
        .await
        .expect("write command hello");
}

fn command(kind: PointKind, point_id: u32, value: f64) -> PhysicalDeviceCommand {
    command_with_id(91, kind, point_id, value)
}

fn command_with_id(
    command_id: u128,
    kind: PointKind,
    point_id: u32,
    value: f64,
) -> PhysicalDeviceCommand {
    let issued_at = now_ms();
    PhysicalDeviceCommand::new(
        CommandId::new(command_id),
        ChannelCommandAddress::new(ChannelId::new(7), kind, PointId::new(point_id))
            .expect("command-owned address"),
        value,
        TimestampMs::new(issued_at),
        TimestampMs::new(issued_at + 5_000),
    )
    .expect("physical command")
}

fn generation(directory: &tempfile::TempDir) -> (Arc<SlotWriter>, Arc<ChannelPointManifest>) {
    let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
        7,
        [1, 0, 1, 1],
    )]));
    let writer = Arc::new(
        SlotWriter::create(
            directory.path().join("commands.shm"),
            manifest.slot_count(),
            manifest.layout_hash(),
            1,
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
        .slot_for(PhysicalPointAddress::from_raw_ids(7, PointKind::Action, 0))
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
    let initial = sink.notifier_status();
    assert!(initial.configured());
    assert!(!initial.connected());
    assert!(initial.last_failure_at_ms().is_some());

    let error = sink
        .send(command(PointKind::Command, 0, 12.5))
        .await
        .expect_err("failed UDS must not report accepted");

    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    let slot = manifest
        .slot_for(PhysicalPointAddress::from_raw_ids(7, PointKind::Command, 0))
        .expect("known command slot");
    assert_eq!(
        writer.read_slot(slot).expect("mirrored command").value,
        12.5,
        "SHM-before-UDS ordering remains part of the protocol"
    );
}

#[tokio::test]
async fn background_probe_recovers_when_io_listener_starts_late() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("late-io.sock");
    let sink = ShmDeviceCommandSink::new();
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier before IO starts");
    assert!(!sink.notifier_status().connected());

    let listener = UnixListener::bind(&socket).expect("start IO listener late");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept probe connection");
        advertise_ready(&mut stream).await;
        stream
    });
    let status = sink
        .probe_notifier()
        .await
        .expect("bounded background reconnect");

    assert!(status.connected());
    let accepted = tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("probe connected before readiness timeout")
        .expect("join probe server");
    drop(accepted);
}

#[tokio::test]
async fn successful_send_uses_the_acknowledged_command_wire() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let (writer, manifest) = generation(&directory);
    let canonical = directory.path().join("commands.shm");
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(AssertCommandLeaseHeld { canonical }));
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    let physical = command(PointKind::Action, 0, -3.25);

    let receive = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        advertise_ready(&mut stream).await;
        let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
        stream
            .read_exact(&mut bytes)
            .await
            .expect("read command frame");
        let frame = DeviceCommandFrame::from_bytes(&bytes).expect("valid command frame");
        let ack = DeviceCommandAck::new(
            frame.command_id(),
            CommandAckStatus::Accepted,
            CommandLedgerStateCode::Queued,
            123_456,
        );
        stream
            .write_all(&ack.to_bytes())
            .await
            .expect("write command acknowledgement");
        frame
    });
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");
    assert!(sink.notifier_status().connected());
    let receipt = sink
        .send(physical)
        .await
        .expect("transport accepted command");
    let frame = receive.await.expect("join listener");

    assert_eq!(receipt.command_id(), physical.id());
    assert_eq!(receipt.accepted_at(), TimestampMs::new(123_456));
    assert_eq!(frame.command_id(), physical.id());
    assert_eq!(frame.channel_id(), 7);
    assert_eq!(frame.point_id(), 0);
    assert_eq!(frame.point_kind(), PointKind::Action);
    assert_eq!(frame.value(), -3.25);
    assert_eq!(frame.issued_at_ms(), physical.issued_at().get());
    assert_eq!(frame.expires_at_ms(), physical.expires_at().get());
}

#[tokio::test]
async fn receipt_is_returned_only_after_matching_durable_acknowledgement() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    let physical = command(PointKind::Command, 0, 7.5);

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        advertise_ready(&mut stream).await;
        let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
        stream.read_exact(&mut bytes).await.expect("read frame");
        let frame = DeviceCommandFrame::from_bytes(&bytes).expect("validate frame");
        let ack = DeviceCommandAck::new(
            frame.command_id(),
            CommandAckStatus::Accepted,
            CommandLedgerStateCode::Queued,
            123_456,
        );
        stream
            .write_all(&ack.to_bytes())
            .await
            .expect("write durable admission ack");
        frame
    });

    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");

    let receipt = sink.send(physical).await.expect("durable admission");
    let frame = server.await.expect("join listener");

    assert_eq!(frame.command_id(), physical.id());
    assert_eq!(frame.channel_id(), 7);
    assert_eq!(frame.point_kind(), PointKind::Command);
    assert_eq!(receipt.command_id(), physical.id());
    assert_eq!(receipt.accepted_at(), TimestampMs::new(123_456));
}

#[tokio::test]
async fn nack_fails_closed_without_a_second_transport() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    let physical = command(PointKind::Command, 0, 8.5);

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        advertise_ready(&mut stream).await;
        let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
        stream.read_exact(&mut bytes).await.expect("read frame");
        let frame = DeviceCommandFrame::from_bytes(&bytes).expect("validate frame");
        let ack = DeviceCommandAck::new(
            frame.command_id(),
            CommandAckStatus::Conflict,
            CommandLedgerStateCode::Unknown,
            now_ms(),
        );
        stream.write_all(&ack.to_bytes()).await.expect("write nack");
    });

    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");

    let error = sink.send(physical).await.expect_err("conflict must fail");
    server.await.expect("join server");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
}

#[tokio::test]
async fn lost_ack_is_ambiguous_and_retries_the_same_command_id() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut stream = stream;
                advertise_ready(&mut stream).await;
                let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
                if stream.read_exact(&mut bytes).await.is_ok() {
                    // Simulate IO committing admission and losing the ACK.
                    // Keep this connection open while the accept loop serves
                    // the producer's idempotent reconnect.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        }
    });
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(writer, manifest)
        .expect("publish generation");
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");

    let error = sink
        .send(command_with_id(0xa11, PointKind::Command, 0, 5.0))
        .await
        .expect_err("lost durable ACK is an ambiguous transport error");
    assert_eq!(error.kind(), PortErrorKind::Timeout);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn mismatched_ack_poisons_stream_and_next_command_reconnects() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let (writer, manifest) = generation(&directory);
    let sink = ShmDeviceCommandSink::new();
    sink.publish_generation(writer, manifest)
        .expect("publish generation");

    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.expect("first connection");
        advertise_ready(&mut first).await;
        let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
        first.read_exact(&mut bytes).await.expect("first frame");
        let first_frame = DeviceCommandFrame::from_bytes(&bytes).expect("parse first frame");
        let wrong_ack = DeviceCommandAck::new(
            CommandId::new(first_frame.command_id().get() + 1),
            CommandAckStatus::Accepted,
            CommandLedgerStateCode::Queued,
            now_ms(),
        );
        first
            .write_all(&wrong_ack.to_bytes())
            .await
            .expect("write mismatched ack");
        drop(first);

        let (mut second, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("next command reconnects")
            .expect("second connection");
        advertise_ready(&mut second).await;
        second.read_exact(&mut bytes).await.expect("second frame");
        let second_frame = DeviceCommandFrame::from_bytes(&bytes).expect("parse second frame");
        let ack = DeviceCommandAck::new(
            second_frame.command_id(),
            CommandAckStatus::Accepted,
            CommandLedgerStateCode::Queued,
            now_ms(),
        );
        second.write_all(&ack.to_bytes()).await.expect("write ack");
        (first_frame.command_id(), second_frame.command_id())
    });

    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");

    let first = command_with_id(0x901, PointKind::Command, 0, 1.0);
    let error = sink
        .send(first)
        .await
        .expect_err("mismatched ack must fail closed");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    let second = command_with_id(0x902, PointKind::Command, 0, 2.0);
    sink.send(second).await.expect("fresh connection succeeds");
    let observed = server.await.expect("join server");
    assert_eq!(observed, (first.id(), second.id()));
}

#[tokio::test]
async fn probe_detects_a_closed_verified_stream_instead_of_reporting_ready() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("m2c.sock");
    let listener = UnixListener::bind(&socket).expect("bind command listener");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept connection");
        advertise_ready(&mut stream).await;
        drop(stream);
        drop(listener);
    });
    let sink = ShmDeviceCommandSink::new();
    sink.configure_notifier(&socket)
        .await
        .expect("configure verified notifier");
    assert!(sink.notifier_status().connected());
    server.await.expect("close server");
    tokio::time::sleep(Duration::from_millis(10)).await;

    let error = sink
        .probe_notifier()
        .await
        .expect_err("closed verified stream must fail its readiness probe");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    assert!(!sink.notifier_status().connected());
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
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        advertise_ready(&mut stream).await;
        let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
        tokio::time::timeout(Duration::from_millis(50), stream.read_exact(&mut bytes)).await
    });
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

    assert!(
        server.await.expect("join server").is_err(),
        "no command frame may be sent after expiry"
    );
}

#[test]
fn reloadable_manifest_source_tracks_each_published_generation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let sink = ShmDeviceCommandSink::new();
    let source = sink.manifest_source();
    let first = Arc::new(ChannelPointManifest::dense_test_fixture([(
        7,
        [1, 0, 1, 1],
    )]));
    let first_writer = Arc::new(
        SlotWriter::create(
            directory.path().join("first.shm"),
            first.slot_count(),
            first.layout_hash(),
            1,
        )
        .expect("first generation"),
    );
    sink.publish_generation(first_writer, Arc::clone(&first))
        .expect("publish first generation");
    assert_eq!(
        source.load().expect("first manifest").layout_hash(),
        first.layout_hash()
    );

    let second = Arc::new(ChannelPointManifest::dense_test_fixture([
        (7, [1, 0, 1, 1]),
        (9, [2, 0, 0, 1]),
    ]));
    let second_writer = Arc::new(
        SlotWriter::create(
            directory.path().join("second.shm"),
            second.slot_count(),
            second.layout_hash(),
            2,
        )
        .expect("second generation"),
    );
    sink.publish_generation(second_writer, Arc::clone(&second))
        .expect("publish second generation");

    let current = source.load().expect("latest manifest");
    assert_eq!(current.layout_hash(), second.layout_hash());
    assert!(
        current
            .slot_for(PhysicalPointAddress::from_raw_ids(9, PointKind::Action, 0,))
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
    let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
        7,
        [1, 0, 1, 1],
    )]));
    let old_writer = Arc::new(
        SlotWriter::create(&canonical, manifest.slot_count(), manifest.layout_hash(), 1)
            .expect("create old canonical generation"),
    );
    let replacement =
        SlotWriter::create(&staging, manifest.slot_count(), manifest.layout_hash(), 2)
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
        .slot_for(PhysicalPointAddress::from_raw_ids(7, PointKind::Action, 0))
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
    let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
        7,
        [1, 0, 1, 1],
    )]));
    let old_writer = Arc::new(
        SlotWriter::create(&canonical, manifest.slot_count(), manifest.layout_hash(), 1)
            .expect("create old canonical generation"),
    );
    let _replacement =
        SlotWriter::create(&staging, manifest.slot_count(), manifest.layout_hash(), 2)
            .expect("create replacement generation");
    let sink = ShmDeviceCommandSink::with_observer(Arc::new(ReplaceCanonicalAfterTransport {
        staging,
        canonical,
    }));
    sink.publish_generation(old_writer, manifest)
        .expect("publish old generation");
    let receive = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept notifier");
        advertise_ready(&mut stream).await;
        let mut bytes = [0_u8; DeviceCommandFrame::SIZE];
        stream
            .read_exact(&mut bytes)
            .await
            .expect("read complete command frame");
        let frame = DeviceCommandFrame::from_bytes(&bytes).expect("valid command frame");
        let ack = DeviceCommandAck::new(
            frame.command_id(),
            CommandAckStatus::Accepted,
            CommandLedgerStateCode::Queued,
            now_ms(),
        );
        stream.write_all(&ack.to_bytes()).await.expect("write ack");
        frame
    });
    sink.configure_notifier(&socket)
        .await
        .expect("configure notifier");
    let rebuild = sink.rebuild_trigger();

    let error = sink
        .send(command(PointKind::Action, 0, 8.0))
        .await
        .expect_err("canonical replacement must suppress the acceptance receipt");
    let frame = receive.await.expect("join listener");

    assert_eq!(frame.command_id().get(), 91);
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    tokio::time::timeout(Duration::from_millis(100), rebuild.notified())
        .await
        .expect("post-transport identity mismatch must request reopen");
    assert!(!sink.is_writer_available());
}
