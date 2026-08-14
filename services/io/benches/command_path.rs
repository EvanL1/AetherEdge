//! Deterministic end-to-end benchmark for durable command retries.
//!
//! This is a one-shot benchmark executable rather than a Criterion sampling
//! loop: the contract under measurement is exactly 10,000 retries of one
//! caller-owned CommandId. Count invariants fail fast, while timing values are
//! emitted as structured JSON for comparison between runs.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_domain::{
    ChannelCommandAddress, ChannelId, CommandId, PhysicalDeviceCommand, PointId, PointKind,
    TimestampMs,
};
use aether_io::core::channels::channel_task::command_bench_support::ProductionCommandBenchDriver;
use aether_io::core::channels::command_ledger::{
    CommandLedger, CommandLedgerAdmission, CommandLedgerState, CommandLedgerTransition,
};
use aether_io::core::channels::{ChannelCommand, ShmCommandListener};
use aether_shm_bridge::{
    CommandAckStatus, CommandHello, CommandLedgerStateCode, DeviceCommandAck, DeviceCommandFrame,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

const RETRY_ATTEMPTS: u64 = 10_000;
const CHANNEL_ID: u32 = 7;
const COMMAND_QUEUE_CAPACITY: usize = 8;
const COMMAND_WINDOW: Duration = Duration::from_secs(60 * 60);

#[derive(Serialize)]
struct BenchmarkReport {
    benchmark: &'static str,
    environment: EnvironmentReport,
    configuration: ConfigurationReport,
    units: UnitReport,
    same_command_retry: RetryReport,
    process_reopen_recovery: ProcessReopenRecoveryReport,
}

#[derive(Serialize)]
struct EnvironmentReport {
    os: &'static str,
    architecture: &'static str,
    build_mode: &'static str,
    logical_cpus: usize,
    rustc_version: Option<String>,
    git_head: Option<String>,
    git_dirty: Option<bool>,
}

#[derive(Serialize)]
struct ConfigurationReport {
    transport: &'static str,
    persistence: &'static str,
    retry_attempts: u64,
    command_queue_capacity: usize,
    command_window_ms: u64,
}

#[derive(Serialize)]
struct UnitReport {
    elapsed: &'static str,
    throughput: &'static str,
    counts: &'static str,
}

#[derive(Serialize)]
struct RetryReport {
    command_id: String,
    initial_admission_us: u64,
    retry_elapsed_us: u64,
    retry_operations_per_second: f64,
    retry_attempts: u64,
    accepted_acks: u64,
    duplicate_acks: u64,
    busy_acks: u64,
    conflict_acks: u64,
    ledger_rows: u64,
    queue_deliveries: u64,
    adapter_invocations: u64,
    final_state: &'static str,
}

#[derive(Serialize)]
struct ProcessReopenRecoveryReport {
    recovery_boundary: &'static str,
    seeded_received: u64,
    seeded_queued: u64,
    seeded_dispatching: u64,
    reopen_elapsed_us: u64,
    recovered_failed: u64,
    recovered_possibly_applied: u64,
    duplicate_admissions: u64,
    network_retry_attempts: u64,
    duplicate_acks: u64,
    busy_acks: u64,
    unavailable_acks: u64,
    observed_replay_queue_deliveries: u64,
    observed_adapter_invocations_after_reopen: u64,
}

struct LiveListener {
    shutdown: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl LiveListener {
    async fn stop(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        if let Some(task) = self.task.take() {
            task.await
                .context("join command listener task")?
                .context("run command listener")?;
        }
        Ok(())
    }
}

impl Drop for LiveListener {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("command_path benchmark failed: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let same_command_retry = benchmark_same_command_retry().await?;
    let process_reopen_recovery = benchmark_process_reopen_recovery().await?;
    let logical_cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let report = BenchmarkReport {
        benchmark: "aether_io_durable_command_path",
        environment: EnvironmentReport {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            build_mode: if cfg!(debug_assertions) {
                "debug"
            } else {
                "optimized"
            },
            logical_cpus,
            rustc_version: command_output("rustc", &["--version"]),
            git_head: command_output("git", &["rev-parse", "HEAD"]),
            git_dirty: git_dirty(),
        },
        configuration: ConfigurationReport {
            transport: "unix_domain_socket",
            persistence: "sqlite_file",
            retry_attempts: RETRY_ATTEMPTS,
            command_queue_capacity: COMMAND_QUEUE_CAPACITY,
            command_window_ms: duration_ms(COMMAND_WINDOW),
        },
        units: UnitReport {
            elapsed: "microseconds",
            throughput: "operations_per_second",
            counts: "events",
        },
        same_command_retry,
        process_reopen_recovery,
    };
    let json = serde_json::to_string_pretty(&report).context("serialize benchmark report")?;
    println!("{json}");
    if let Some(output_dir) = std::env::var_os("AETHER_BENCH_OUTPUT_DIR") {
        let output_dir = std::path::PathBuf::from(output_dir);
        std::fs::create_dir_all(&output_dir).with_context(|| {
            format!("create benchmark output directory {}", output_dir.display())
        })?;
        let output_path = output_dir.join("command-path.json");
        std::fs::write(&output_path, format!("{json}\n"))
            .with_context(|| format!("write benchmark artifact {}", output_path.display()))?;
    }
    Ok(())
}

async fn benchmark_same_command_retry() -> Result<RetryReport> {
    let directory = tempfile::tempdir().context("create retry benchmark directory")?;
    let pool = open_file_pool(&directory.path().join("retry-ledger.sqlite")).await?;
    let ledger = Arc::new(
        CommandLedger::initialize(pool.clone())
            .await
            .context("initialize retry benchmark ledger")?,
    );
    let mut consumer = ProductionCommandBenchDriver::new(
        &directory.path().join("retry-consumer.shm"),
        Arc::clone(&ledger),
        CHANNEL_ID,
        1,
    )
    .context("compose production command consumer")?;
    let (listener, mut stream, mut receiver) =
        start_listener(&directory, "retry", Arc::clone(&ledger)).await?;

    let command_id = CommandId::new(0xa37e_1000_0000_0000_0000_0000_0000_0001);
    let frame = command_frame(command_id, 1, 1.0)?;
    let admission_started = Instant::now();
    let accepted = exchange_frame(&mut stream, frame).await?;
    let initial_admission_us = elapsed_us(admission_started.elapsed());
    let accepted_acks = match accepted.status() {
        CommandAckStatus::Accepted => 1,
        status => anyhow::bail!("first command was not accepted: {status:?}"),
    };
    ensure!(
        accepted_acks == 1,
        "first command produced an invalid accepted ACK count"
    );
    ensure!(
        accepted.state() == CommandLedgerStateCode::Queued,
        "first command did not enter Queued: {:?}",
        accepted.state()
    );

    // The ACK is written only after the reserved permit has delivered this
    // item and queue admission has been persisted, so try_recv is deterministic.
    let (queue_deliveries, command) = match receiver.try_recv() {
        Ok(command) => (1_u64, command),
        Err(error) => {
            return Err(error).context("accepted command was not delivered to the bounded queue");
        },
    };
    ensure!(
        command.durable_command_id() == Some(command_id),
        "queue delivered a different durable identity"
    );
    let queued = ledger
        .query(command_id)
        .await
        .context("query queued command")?
        .context("queued command missing from ledger")?;
    ensure!(
        queued.state() == CommandLedgerState::Queued && queued.accepted_at().is_some(),
        "accepted command did not retain its queue-admission marker"
    );

    let dispatch = consumer.consume(command).await;
    ensure!(
        dispatch.adapter_invocation_delta == 1,
        "production consumer invoked the adapter {} times for the first command",
        dispatch.adapter_invocation_delta
    );
    let completed = ledger
        .query(command_id)
        .await
        .context("query production consumer outcome")?
        .context("production consumer removed the command identity")?;
    ensure!(
        completed.state() == CommandLedgerState::Succeeded,
        "production consumer did not persist Succeeded: {:?}",
        completed.state()
    );
    let final_state = completed.state();

    let mut retry_attempts = 0_u64;
    let mut duplicate_acks = 0_u64;
    let mut busy_acks = 0_u64;
    let mut conflict_acks = 0_u64;
    let retry_started = Instant::now();
    for _ in 0..RETRY_ATTEMPTS {
        let ack = exchange_frame(&mut stream, frame).await?;
        retry_attempts += 1;
        match ack.status() {
            CommandAckStatus::Duplicate => duplicate_acks += 1,
            CommandAckStatus::Busy => busy_acks += 1,
            CommandAckStatus::Conflict => conflict_acks += 1,
            status => anyhow::bail!("unexpected retry acknowledgement: {status:?}"),
        }
        ensure!(
            ack.state() == CommandLedgerStateCode::Succeeded,
            "retry lost the original terminal state: {:?}",
            ack.state()
        );
    }
    let retry_elapsed = retry_started.elapsed();

    ensure!(
        matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "a duplicate retry was delivered to the command queue"
    );
    let stats = ledger.stats().await.context("load retry ledger stats")?;
    ensure!(
        stats.total == 1,
        "expected one ledger row, got {}",
        stats.total
    );
    ensure!(
        stats.succeeded == 1,
        "expected one succeeded ledger row, got {}",
        stats.succeeded
    );
    ensure!(
        retry_attempts == RETRY_ATTEMPTS,
        "expected {RETRY_ATTEMPTS} observed retries, got {retry_attempts}"
    );
    ensure!(
        duplicate_acks == retry_attempts,
        "expected {retry_attempts} duplicate ACKs, got {duplicate_acks}"
    );
    ensure!(
        busy_acks == 0,
        "terminal retries unexpectedly returned Busy"
    );
    ensure!(conflict_acks == 0, "exact retries unexpectedly conflicted");
    ensure!(
        consumer.adapter_invocations() == 1,
        "adapter was invoked {} times instead of once",
        consumer.adapter_invocations()
    );

    drop(stream);
    listener.stop().await?;
    drop(ledger);
    pool.close().await;

    Ok(RetryReport {
        command_id: format!("{:032x}", command_id.get()),
        initial_admission_us,
        retry_elapsed_us: elapsed_us(retry_elapsed),
        retry_operations_per_second: operations_per_second(retry_attempts, retry_elapsed),
        retry_attempts,
        accepted_acks,
        duplicate_acks,
        busy_acks,
        conflict_acks,
        ledger_rows: stats.total,
        queue_deliveries,
        adapter_invocations: consumer.adapter_invocations(),
        final_state: final_state.as_str(),
    })
}

async fn benchmark_process_reopen_recovery() -> Result<ProcessReopenRecoveryReport> {
    let directory = tempfile::tempdir().context("create process-reopen benchmark directory")?;
    let database_path = directory.path().join("command-ledger.sqlite");
    let seed_pool = open_file_pool(&database_path).await?;
    let seed_ledger = Arc::new(
        CommandLedger::initialize(seed_pool.clone())
            .await
            .context("initialize process-reopen seed ledger")?,
    );
    let frames = [
        command_frame(
            CommandId::new(0xa37e_2000_0000_0000_0000_0000_0000_0001),
            1,
            1.0,
        )?,
        command_frame(
            CommandId::new(0xa37e_2000_0000_0000_0000_0000_0000_0002),
            2,
            2.0,
        )?,
        command_frame(
            CommandId::new(0xa37e_2000_0000_0000_0000_0000_0000_0003),
            3,
            3.0,
        )?,
    ];

    for frame in frames {
        let admission = seed_ledger
            .admit_for_channel(
                frame.command_id(),
                frame.channel_id(),
                frame.semantic_digest(),
                TimestampMs::new(frame.expires_at_ms()),
            )
            .await
            .context("seed process-reopen command")?;
        ensure!(
            matches!(admission, CommandLedgerAdmission::New(_)),
            "process-reopen seed reused a command identity"
        );
    }
    transition_exact(
        seed_ledger.as_ref(),
        frames[1].command_id(),
        CommandLedgerState::Received,
        CommandLedgerState::Queued,
    )
    .await?;
    seed_ledger
        .mark_accepted(frames[1].command_id())
        .await
        .context("mark queued process-reopen seed accepted")?;
    transition_exact(
        seed_ledger.as_ref(),
        frames[2].command_id(),
        CommandLedgerState::Received,
        CommandLedgerState::Queued,
    )
    .await?;
    seed_ledger
        .mark_accepted(frames[2].command_id())
        .await
        .context("mark dispatching process-reopen seed accepted")?;
    transition_exact(
        seed_ledger.as_ref(),
        frames[2].command_id(),
        CommandLedgerState::Queued,
        CommandLedgerState::Dispatching,
    )
    .await?;

    let seeded = seed_ledger
        .stats()
        .await
        .context("observe process-reopen seed states")?;
    ensure!(
        seeded.total == 3 && seeded.received == 1 && seeded.queued == 1 && seeded.dispatching == 1,
        "unexpected process-reopen seed states: {seeded:?}"
    );

    // This closes committed SQLite state and constructs a fresh production
    // ledger instance. It measures process-reopen recovery, not power-loss or
    // kill-during-write durability.
    drop(seed_ledger);
    seed_pool.close().await;

    let reopen_started = Instant::now();
    let reopened_pool = open_file_pool(&database_path).await?;
    let reopened = Arc::new(
        CommandLedger::initialize(reopened_pool.clone())
            .await
            .context("reopen command ledger in a fresh process model")?,
    );
    let reopen_elapsed_us = elapsed_us(reopen_started.elapsed());
    let recovered = reopened
        .stats()
        .await
        .context("load recovered command stats")?;
    ensure!(
        recovered.total == 3,
        "recovery changed ledger cardinality: {}",
        recovered.total
    );
    ensure!(
        recovered.failed == 1,
        "Received did not recover to exactly one Failed row"
    );
    ensure!(
        recovered.possibly_applied == 2,
        "Queued/Dispatching did not recover to exactly two PossiblyApplied rows"
    );

    let mut duplicate_admissions = 0_u64;
    for frame in frames {
        match reopened
            .admit_for_channel(
                frame.command_id(),
                frame.channel_id(),
                frame.semantic_digest(),
                TimestampMs::new(frame.expires_at_ms()),
            )
            .await
            .context("retry recovered identity through ledger admission")?
        {
            CommandLedgerAdmission::Same(_) => duplicate_admissions += 1,
            other => anyhow::bail!("recovered identity was not recognized as duplicate: {other:?}"),
        }
    }

    let (listener, mut stream, mut receiver) =
        start_listener(&directory, "reopen", Arc::clone(&reopened)).await?;
    let mut consumer = ProductionCommandBenchDriver::new(
        &directory.path().join("reopen-consumer.shm"),
        Arc::clone(&reopened),
        CHANNEL_ID,
        1,
    )
    .context("compose production consumer after process reopen")?;
    let mut duplicate_acks = 0_u64;
    let mut busy_acks = 0_u64;
    let mut unavailable_acks = 0_u64;
    let mut network_retry_attempts = 0_u64;
    for frame in frames {
        let ack = exchange_frame(&mut stream, frame).await?;
        network_retry_attempts += 1;
        match ack.status() {
            CommandAckStatus::Duplicate => {
                duplicate_acks += 1;
                ensure!(
                    ack.state() == CommandLedgerStateCode::PossiblyApplied,
                    "accepted ambiguous recovery retry lost PossiblyApplied state"
                );
            },
            CommandAckStatus::Busy => {
                busy_acks += 1;
                ensure!(
                    ack.state() == CommandLedgerStateCode::PossiblyApplied,
                    "ambiguous recovery retry lost PossiblyApplied state"
                );
            },
            CommandAckStatus::Unavailable => {
                unavailable_acks += 1;
                ensure!(
                    ack.state() == CommandLedgerStateCode::Failed,
                    "pre-queue recovery retry lost Failed state"
                );
            },
            status => anyhow::bail!("unexpected recovered retry acknowledgement: {status:?}"),
        }
    }

    let mut observed_replay_queue_deliveries = 0_u64;
    loop {
        match receiver.try_recv() {
            Ok(command) => {
                observed_replay_queue_deliveries += 1;
                consumer.consume(command).await;
            },
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                anyhow::bail!("process-reopen command queue disconnected during benchmark")
            },
        }
    }
    let observed_adapter_invocations_after_reopen = consumer.adapter_invocations();
    ensure!(
        duplicate_admissions == 3,
        "not every recovered identity deduplicated"
    );
    ensure!(
        duplicate_acks == 2,
        "expected two accepted ambiguous Duplicate acknowledgements"
    );
    ensure!(busy_acks == 0, "accepted recovered commands returned Busy");
    ensure!(
        unavailable_acks == 1,
        "expected one pre-queue Unavailable acknowledgement"
    );
    ensure!(
        observed_replay_queue_deliveries == 0,
        "recovery replayed a command into the bounded queue"
    );
    ensure!(
        observed_adapter_invocations_after_reopen == 0,
        "recovery invoked the adapter without a new command"
    );

    drop(stream);
    listener.stop().await?;
    drop(reopened);
    reopened_pool.close().await;

    Ok(ProcessReopenRecoveryReport {
        recovery_boundary: "committed_state_process_reopen_not_power_loss",
        seeded_received: seeded.received,
        seeded_queued: seeded.queued,
        seeded_dispatching: seeded.dispatching,
        reopen_elapsed_us,
        recovered_failed: recovered.failed,
        recovered_possibly_applied: recovered.possibly_applied,
        duplicate_admissions,
        network_retry_attempts,
        duplicate_acks,
        busy_acks,
        unavailable_acks,
        observed_replay_queue_deliveries,
        observed_adapter_invocations_after_reopen,
    })
}

async fn start_listener(
    directory: &TempDir,
    name: &str,
    ledger: Arc<CommandLedger>,
) -> Result<(LiveListener, UnixStream, mpsc::Receiver<ChannelCommand>)> {
    let path = directory.path().join(format!("{name}.sock"));
    let path = path.to_string_lossy().into_owned();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let listener = Arc::new(ShmCommandListener::with_command_ledger_for_bench(
        &path,
        shutdown_rx,
        ledger,
    ));
    let (sender, receiver) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
    listener.register_channel(CHANNEL_ID, sender);
    let prepared = listener.prepare().context("prepare command socket")?;
    let running_listener = Arc::clone(&listener);
    let task = tokio::spawn(async move { running_listener.run_prepared(prepared).await });
    let mut stream = UnixStream::connect(&path)
        .await
        .context("connect command socket")?;
    let mut hello_bytes = [0_u8; CommandHello::SIZE];
    stream
        .read_exact(&mut hello_bytes)
        .await
        .context("read command hello")?;
    CommandHello::from_bytes(&hello_bytes).context("parse command hello")?;
    Ok((
        LiveListener {
            shutdown: Some(shutdown_tx),
            task: Some(task),
        },
        stream,
        receiver,
    ))
}

async fn exchange_frame(
    stream: &mut UnixStream,
    frame: DeviceCommandFrame,
) -> Result<DeviceCommandAck> {
    stream
        .write_all(&frame.to_bytes())
        .await
        .context("write command frame")?;
    let mut ack_bytes = [0_u8; DeviceCommandAck::SIZE];
    stream
        .read_exact(&mut ack_bytes)
        .await
        .context("read command acknowledgement")?;
    DeviceCommandAck::from_bytes(&ack_bytes).context("parse command acknowledgement")
}

fn command_frame(command_id: CommandId, point_id: u32, value: f64) -> Result<DeviceCommandFrame> {
    let issued_at_ms = now_ms();
    let expires_at_ms = issued_at_ms.saturating_add(duration_ms(COMMAND_WINDOW));
    let target = ChannelCommandAddress::new(
        ChannelId::new(CHANNEL_ID),
        PointKind::Command,
        PointId::new(point_id),
    )
    .map_err(|error| anyhow::anyhow!("construct command address: {error}"))?;
    let command = PhysicalDeviceCommand::new(
        command_id,
        target,
        value,
        TimestampMs::new(issued_at_ms),
        TimestampMs::new(expires_at_ms),
    )
    .map_err(|error| anyhow::anyhow!("construct physical command: {error}"))?;
    DeviceCommandFrame::new(command).context("encode command frame")
}

async fn transition_exact(
    ledger: &CommandLedger,
    command_id: CommandId,
    expected: CommandLedgerState,
    next: CommandLedgerState,
) -> Result<()> {
    let transition = ledger
        .transition(command_id, expected, next)
        .await
        .with_context(|| format!("transition command from {expected:?} to {next:?}"))?;
    ensure!(
        matches!(transition, CommandLedgerTransition::Updated(_)),
        "command transition was not applied: {transition:?}"
    );
    Ok(())
}

async fn open_file_pool(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("open SQLite ledger at {}", path.display()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn elapsed_us(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

fn operations_per_second(operations: u64, elapsed: Duration) -> f64 {
    operations as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn git_dirty() -> Option<bool> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}
