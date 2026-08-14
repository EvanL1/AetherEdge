//! Integration tests for the single durable M2C command listener.

#![allow(clippy::disallowed_methods)] // Test code may use expect/panic.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_domain::{
    ChannelCommandAddress, ChannelId, CommandId, PhysicalDeviceCommand, PointId, PointKind,
    TimestampMs,
};
use aether_io::core::channels::ShmCommandListener;
use aether_io::core::channels::command_ledger::CommandLedger;
use aether_io::core::channels::types::ChannelCommand;
use aether_shm_bridge::{CommandAckStatus, CommandHello, DeviceCommandAck, DeviceCommandFrame};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

struct RunningListener {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    listener: Arc<ShmCommandListener>,
}

impl RunningListener {
    async fn stop(self) {
        let _ = self.shutdown.send(true);
        self.task
            .await
            .expect("join command listener")
            .expect("stop command listener");
    }
}

async fn start_listener(
    uds_path: &str,
    channel_id: u32,
) -> (RunningListener, UnixStream, mpsc::Receiver<ChannelCommand>) {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open command ledger");
    let ledger = Arc::new(
        CommandLedger::initialize(pool)
            .await
            .expect("initialize command ledger"),
    );
    let (shutdown, shutdown_rx) = watch::channel(false);
    let listener = Arc::new(ShmCommandListener::new(Some(uds_path), shutdown_rx, ledger));
    let (sender, receiver) = mpsc::channel(8);
    listener.register_channel(channel_id, sender);
    let task_listener = Arc::clone(&listener);
    let task = tokio::spawn(async move { task_listener.run().await });

    let mut stream = loop {
        match UnixStream::connect(uds_path).await {
            Ok(stream) => break stream,
            Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    };
    let mut hello_bytes = [0_u8; CommandHello::SIZE];
    stream
        .read_exact(&mut hello_bytes)
        .await
        .expect("read command readiness");
    CommandHello::from_bytes(&hello_bytes).expect("parse command readiness");

    (
        RunningListener {
            shutdown,
            task,
            listener,
        },
        stream,
        receiver,
    )
}

fn command_frame(
    command_id: u128,
    channel_id: u32,
    point_id: u32,
    kind: PointKind,
    value: f64,
    issued_at_ms: u64,
    expires_at_ms: u64,
) -> DeviceCommandFrame {
    let address =
        ChannelCommandAddress::new(ChannelId::new(channel_id), kind, PointId::new(point_id))
            .expect("command address");
    let command = PhysicalDeviceCommand::new(
        CommandId::new(command_id),
        address,
        value,
        TimestampMs::new(issued_at_ms),
        TimestampMs::new(expires_at_ms),
    )
    .expect("physical command");
    DeviceCommandFrame::new(command).expect("command frame")
}

async fn exchange(stream: &mut UnixStream, frame: DeviceCommandFrame) -> DeviceCommandAck {
    stream
        .write_all(&frame.to_bytes())
        .await
        .expect("write command frame");
    let mut ack_bytes = [0_u8; DeviceCommandAck::SIZE];
    stream
        .read_exact(&mut ack_bytes)
        .await
        .expect("read command acknowledgement");
    DeviceCommandAck::from_bytes(&ack_bytes).expect("parse command acknowledgement")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn temp_uds_path() -> (tempfile::TempDir, String) {
    let directory = tempfile::Builder::new()
        .prefix("aether-command-")
        .tempdir_in("/tmp")
        .expect("command socket directory");
    let path = directory.path().join("m2c.sock");
    (directory, path.to_string_lossy().into_owned())
}

#[tokio::test]
async fn command_is_acknowledged_and_dispatched_with_its_durable_identity() {
    let (_directory, path) = temp_uds_path();
    let (running, mut stream, mut receiver) = start_listener(&path, 1001).await;
    let issued_at_ms = now_ms();
    let frame = command_frame(
        0x1234,
        1001,
        42,
        PointKind::Command,
        123.45,
        issued_at_ms,
        issued_at_ms + 5_000,
    );

    let ack = exchange(&mut stream, frame).await;
    assert_eq!(ack.status(), CommandAckStatus::Accepted);
    let command = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("command dispatch timeout")
        .expect("command queue closed");
    assert_eq!(command.durable_command_id(), Some(CommandId::new(0x1234)));
    assert!(matches!(
        command,
        ChannelCommand::Control {
            point_id: 42,
            value,
            ..
        } if (value - 123.45).abs() < f64::EPSILON
    ));
    assert_eq!(running.listener.stats().frames_total, 1);
    assert!(running.listener.stats().last_frame_at_ms.is_some());

    drop(stream);
    running.stop().await;
}

#[tokio::test]
async fn same_id_is_idempotent_and_different_payload_conflicts() {
    let (_directory, path) = temp_uds_path();
    let (running, mut stream, mut receiver) = start_listener(&path, 7).await;
    let issued_at_ms = now_ms();
    let frame = command_frame(
        0x5678,
        7,
        1,
        PointKind::Command,
        1.0,
        issued_at_ms,
        issued_at_ms + 5_000,
    );

    assert_eq!(
        exchange(&mut stream, frame).await.status(),
        CommandAckStatus::Accepted
    );
    let _ = receiver.recv().await.expect("first command");
    assert_eq!(
        exchange(&mut stream, frame).await.status(),
        CommandAckStatus::Duplicate
    );
    let conflict = command_frame(
        0x5678,
        7,
        1,
        PointKind::Command,
        2.0,
        issued_at_ms,
        issued_at_ms + 5_000,
    );
    assert_eq!(
        exchange(&mut stream, conflict).await.status(),
        CommandAckStatus::Conflict
    );
    assert!(receiver.try_recv().is_err());

    drop(stream);
    running.stop().await;
}

#[tokio::test]
async fn unknown_channel_is_rejected_without_dispatch() {
    let (_directory, path) = temp_uds_path();
    let (running, mut stream, mut receiver) = start_listener(&path, 7).await;
    let issued_at_ms = now_ms();
    let frame = command_frame(
        0x9abc,
        9999,
        1,
        PointKind::Action,
        -4.0,
        issued_at_ms,
        issued_at_ms + 5_000,
    );

    assert_eq!(
        exchange(&mut stream, frame).await.status(),
        CommandAckStatus::Unavailable
    );
    assert!(receiver.try_recv().is_err());

    drop(stream);
    running.stop().await;
}
