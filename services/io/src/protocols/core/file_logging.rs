//! File-based channel logging handler.
//!
//! This module provides `ChannelFileLogHandler` which writes channel logs
//! to per-channel, per-day log files.
//!
//! # Directory Structure
//!
//! ```text
//! /logs/io/channels/
//! ├── PCS#1/
//! │   ├── 2025-01-22.log
//! │   └── 2025-01-21.log
//! ├── BAMS#1/
//! │   └── 2025-01-22.log
//! └── GENSET#1/
//!     └── 2025-01-22.log
//! ```
//!
//! # Log Levels
//!
//! - **Info**: Raw packets (hex format) and errors only
//! - **Debug**: Raw packets + poll cycles, state changes, control writes

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{Local, NaiveDate};

use super::logging::{
    ChannelLogEvent, ChannelLogHandler, ErrorContext, PacketDirection, PacketMetadata,
};

// ============================================================================
// File Log Level
// ============================================================================

/// Log level for file logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileLogLevel {
    /// Info level: raw packets and errors only.
    #[default]
    Info,
    /// Debug level: raw packets + poll cycles, state changes, control writes.
    Debug,
}

impl FileLogLevel {
    /// Parse one canonical channel log level.
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s.unwrap_or("info") {
            "debug" => Some(Self::Debug),
            "info" | "error" => Some(Self::Info),
            _ => None,
        }
    }

    /// Convert to u8 for atomic storage.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Debug => 1,
        }
    }

    /// Convert from u8 (atomic load).
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Debug,
            _ => Self::Info,
        }
    }
}

// ============================================================================
// Channel File Log Handler
// ============================================================================

/// State for an open log file.
struct OpenFile {
    /// The date this file was created for.
    date: NaiveDate,
    /// Exact directory selected by the producing channel generation.
    channel_dir: PathBuf,
    /// Buffered writer for the file.
    writer: BufWriter<File>,
}

const DEFAULT_QUEUE_CAPACITY: usize = 1_024;
const MAX_RECORD_BYTES: usize = 64 * 1_024;
const MAX_RAW_PACKET_BYTES: usize = (MAX_RECORD_BYTES - 1_024) / 3;
const DEFAULT_FLUSH_BATCH_SIZE: usize = 64;
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
struct WorkerConfig {
    queue_capacity: usize,
    flush_batch_size: usize,
    flush_interval: Duration,
    shutdown_drain: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            flush_batch_size: DEFAULT_FLUSH_BATCH_SIZE,
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            shutdown_drain: DEFAULT_SHUTDOWN_DRAIN,
        }
    }
}

/// Snapshot of the bounded file-log admission and disk-worker state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileLogStats {
    pub accepted: u64,
    pub dropped: u64,
    pub oversized: u64,
    pub write_failures: u64,
    pub flush_failures: u64,
    pub shutdown_timeouts: u64,
    pub pending: u64,
    pub worker_running: bool,
    pub io_healthy: bool,
    pub io_stalled: bool,
}

#[derive(Default)]
struct FileLogCounters {
    accepted: AtomicU64,
    dropped: AtomicU64,
    oversized: AtomicU64,
    write_failures: AtomicU64,
    flush_failures: AtomicU64,
    shutdown_timeouts: AtomicU64,
    pending: AtomicU64,
    worker_running: std::sync::atomic::AtomicBool,
    io_healthy: std::sync::atomic::AtomicBool,
    io_operation_started_ms: AtomicU64,
    io_stall_after_ms: AtomicU64,
}

impl FileLogCounters {
    fn snapshot(&self) -> FileLogStats {
        let operation_started_ms = self.io_operation_started_ms.load(Ordering::Acquire);
        let io_stalled = operation_started_ms != 0
            && monotonic_ms().saturating_sub(operation_started_ms)
                > self.io_stall_after_ms.load(Ordering::Relaxed);
        FileLogStats {
            accepted: self.accepted.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            oversized: self.oversized.load(Ordering::Relaxed),
            write_failures: self.write_failures.load(Ordering::Relaxed),
            flush_failures: self.flush_failures.load(Ordering::Relaxed),
            shutdown_timeouts: self.shutdown_timeouts.load(Ordering::Relaxed),
            pending: self.pending.load(Ordering::Relaxed),
            worker_running: self.worker_running.load(Ordering::Acquire),
            io_healthy: self.io_healthy.load(Ordering::Acquire),
            io_stalled,
        }
    }
}

fn monotonic_ms() -> u64 {
    static PROCESS_EPOCH: OnceLock<Instant> = OnceLock::new();
    let elapsed = PROCESS_EPOCH
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis();
    u64::try_from(elapsed)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1)
}

struct IoOperationGuard<'a>(&'a AtomicU64);

impl<'a> IoOperationGuard<'a> {
    fn begin(started_ms: &'a AtomicU64) -> Self {
        started_ms.store(monotonic_ms(), Ordering::Release);
        Self(started_ms)
    }
}

impl Drop for IoOperationGuard<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}

struct LogRecord {
    channel_id: u32,
    channel_dir: PathBuf,
    date: NaiveDate,
    timestamp: String,
    line: String,
}

trait LogSink: Send + 'static {
    fn write(&mut self, record: &LogRecord) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;

    fn take_rotation_flush_failures(&mut self) -> u64 {
        0
    }
}

#[cfg(test)]
struct AlwaysFailingFlushSink;

#[cfg(test)]
impl LogSink for AlwaysFailingFlushSink {
    fn write(&mut self, _record: &LogRecord) -> io::Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic final flush failure"))
    }
}

#[derive(Default)]
struct FileLogSink {
    open_files: HashMap<u32, OpenFile>,
    rotation_flush_failures: u64,
}

impl FileLogSink {
    fn ensure_writer(&mut self, record: &LogRecord) -> io::Result<()> {
        let needs_new_file = self.open_files.get(&record.channel_id).is_none_or(|open| {
            open.date != record.date
                || open.channel_dir != record.channel_dir
                || !record.channel_dir.exists()
        });
        if !needs_new_file {
            return Ok(());
        }

        if let Some(previous) = self.open_files.get_mut(&record.channel_id) {
            if let Err(error) = previous.writer.flush() {
                self.rotation_flush_failures = self.rotation_flush_failures.saturating_add(1);
                return Err(error);
            }
            self.open_files.remove(&record.channel_id);
        }

        fs::create_dir_all(&record.channel_dir)?;
        let file_path = record
            .channel_dir
            .join(format!("{}.log", record.date.format("%Y-%m-%d")));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)?;
        self.open_files.insert(
            record.channel_id,
            OpenFile {
                date: record.date,
                channel_dir: record.channel_dir.clone(),
                writer: BufWriter::new(file),
            },
        );
        Ok(())
    }
}

impl LogSink for FileLogSink {
    fn write(&mut self, record: &LogRecord) -> io::Result<()> {
        self.ensure_writer(record)?;
        match self.open_files.get_mut(&record.channel_id) {
            Some(open) => writeln!(open.writer, "{} {}", record.timestamp, record.line),
            None => Err(io::Error::other("file-log writer missing after open")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut first_error = None;
        for open in self.open_files.values_mut() {
            if let Err(error) = open.writer.flush()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn take_rotation_flush_failures(&mut self) -> u64 {
        std::mem::take(&mut self.rotation_flush_failures)
    }
}

pub(crate) struct LogWorker {
    sender: Mutex<Option<SyncSender<LogRecord>>>,
    completed: Mutex<Option<Receiver<io::Result<()>>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    shutdown_lock: Mutex<()>,
    terminal_result: Mutex<Option<WorkerTerminalResult>>,
    counters: Arc<FileLogCounters>,
    shutdown_drain: Duration,
}

#[derive(Clone)]
enum WorkerTerminalResult {
    Succeeded,
    Failed {
        kind: io::ErrorKind,
        message: String,
    },
}

impl WorkerTerminalResult {
    fn from_result(result: &io::Result<()>) -> Self {
        match result {
            Ok(()) => Self::Succeeded,
            Err(error) => Self::Failed {
                kind: error.kind(),
                message: error.to_string(),
            },
        }
    }

    fn into_result(self) -> io::Result<()> {
        match self {
            Self::Succeeded => Ok(()),
            Self::Failed { kind, message } => Err(io::Error::new(kind, message)),
        }
    }
}

impl LogWorker {
    fn spawn<S: LogSink>(sink: S, config: WorkerConfig) -> io::Result<Arc<Self>> {
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity.max(1));
        let (completed_tx, completed) = mpsc::channel();
        let counters = Arc::new(FileLogCounters::default());
        counters.worker_running.store(true, Ordering::Release);
        counters.io_healthy.store(true, Ordering::Release);
        counters.io_stall_after_ms.store(
            u64::try_from(config.shutdown_drain.as_millis())
                .unwrap_or(u64::MAX)
                .max(1),
            Ordering::Relaxed,
        );
        let worker_counters = Arc::clone(&counters);
        let flush_batch_size = config.flush_batch_size.max(1);
        let flush_interval = config.flush_interval.max(Duration::from_millis(1));
        let thread = std::thread::Builder::new()
            .name("aether-io-file-log".to_string())
            .spawn(move || {
                let _running = WorkerRunningGuard(Arc::clone(&worker_counters));
                let result = run_log_worker(
                    receiver,
                    sink,
                    Arc::clone(&worker_counters),
                    flush_batch_size,
                    flush_interval,
                );
                let _ = completed_tx.send(result);
            })?;
        Ok(Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            completed: Mutex::new(Some(completed)),
            thread: Mutex::new(Some(thread)),
            shutdown_lock: Mutex::new(()),
            terminal_result: Mutex::new(None),
            counters,
            shutdown_drain: config.shutdown_drain,
        }))
    }

    pub(crate) fn spawn_default() -> io::Result<Arc<Self>> {
        Self::spawn(FileLogSink::default(), WorkerConfig::default())
    }

    #[cfg(test)]
    pub(crate) fn spawn_failing_final_flush_for_test() -> io::Result<Arc<Self>> {
        Self::spawn(
            AlwaysFailingFlushSink,
            WorkerConfig {
                flush_batch_size: usize::MAX,
                flush_interval: Duration::from_secs(60),
                ..WorkerConfig::default()
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn enqueue_record_for_test(&self) {
        self.enqueue(LogRecord {
            channel_id: 1,
            channel_dir: PathBuf::from("/test"),
            date: Local::now().date_naive(),
            timestamp: "test".to_string(),
            line: "test".to_string(),
        });
    }

    fn enqueue(&self, record: LogRecord) {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(sender) = sender.as_ref() else {
            self.record_drop("worker is shutting down");
            return;
        };
        self.counters.pending.fetch_add(1, Ordering::Relaxed);
        match sender.try_send(record) {
            Ok(()) => {
                self.counters.accepted.fetch_add(1, Ordering::Relaxed);
            },
            Err(TrySendError::Full(_)) => {
                self.counters.pending.fetch_sub(1, Ordering::Relaxed);
                self.record_drop("bounded queue is full");
            },
            Err(TrySendError::Disconnected(_)) => {
                self.counters.pending.fetch_sub(1, Ordering::Relaxed);
                self.record_drop("disk worker exited");
            },
        }
    }

    fn reject_oversized(&self, bytes: usize) {
        let oversized = self.counters.oversized.fetch_add(1, Ordering::Relaxed) + 1;
        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        if oversized.is_power_of_two() {
            tracing::warn!(
                oversized,
                bytes,
                max_record_bytes = MAX_RECORD_BYTES,
                "Oversized channel file-log record dropped"
            );
        }
    }

    fn record_drop(&self, reason: &'static str) {
        let dropped = self.counters.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if dropped.is_power_of_two() {
            tracing::warn!(
                dropped,
                pending = self.counters.pending.load(Ordering::Relaxed),
                reason,
                "Channel file log event dropped"
            );
        }
    }

    pub(crate) fn stats(&self) -> FileLogStats {
        self.counters.snapshot()
    }

    pub(crate) fn shutdown_blocking(&self) -> io::Result<()> {
        let _shutdown = self
            .shutdown_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(result) = self
            .terminal_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return result.into_result();
        }
        let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return Err(io::Error::other(
                "file-log worker handle is unavailable before terminal completion",
            ));
        };
        let Some(completed) = self
            .completed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            self.thread
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .replace(thread);
            return Err(io::Error::other(
                "file-log completion receiver is unavailable",
            ));
        };
        let worker_result = match completed.recv_timeout(self.shutdown_drain) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
                "file-log worker exited without reporting terminal completion",
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.completed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .replace(completed);
                self.thread
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .replace(thread);
                self.counters
                    .shutdown_timeouts
                    .fetch_add(1, Ordering::Relaxed);
                self.counters.io_healthy.store(false, Ordering::Release);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "file-log worker did not drain {} pending records within {} ms",
                        self.counters.pending.load(Ordering::Relaxed),
                        self.shutdown_drain.as_millis()
                    ),
                ));
            },
        };
        let result = thread
            .join()
            .map_err(|_| io::Error::other("file-log worker panicked during shutdown"))
            .and(worker_result);
        self.terminal_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(WorkerTerminalResult::from_result(&result));
        result
    }

    fn shutdown_in_reaper(&self) {
        let _shutdown = self
            .shutdown_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if self
            .terminal_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
        {
            return;
        }
        let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        let Some(completed) = self
            .completed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            drop(thread);
            return;
        };
        let counters = Arc::clone(&self.counters);
        let shutdown_drain = self.shutdown_drain;
        if let Err(error) = std::thread::Builder::new()
            .name("aether-io-file-log-reaper".to_string())
            .spawn(move || match completed.recv_timeout(shutdown_drain) {
                Ok(result) => {
                    if thread.join().is_err() {
                        tracing::warn!("Channel file-log worker panicked during shutdown");
                    } else if let Err(error) = result {
                        tracing::warn!(%error, "Channel file-log worker final flush failed during shutdown");
                    }
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if thread.join().is_err() {
                        tracing::warn!("Channel file-log worker panicked during shutdown");
                    } else {
                        tracing::warn!(
                            "Channel file-log worker exited without reporting terminal completion"
                        );
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    counters
                        .shutdown_timeouts
                        .fetch_add(1, Ordering::Relaxed);
                    counters.io_healthy.store(false, Ordering::Release);
                    tracing::warn!(
                        pending = counters.pending.load(Ordering::Relaxed),
                        drain_timeout_ms = shutdown_drain.as_millis(),
                        "Channel file-log shutdown drain timed out; disk worker detached"
                    );
                    drop(thread);
                },
            })
        {
            tracing::warn!(%error, "Cannot start channel file-log shutdown reaper; worker detached");
        }
    }
}

struct WorkerRunningGuard(Arc<FileLogCounters>);

impl Drop for WorkerRunningGuard {
    fn drop(&mut self) {
        let pending = self.0.pending.swap(0, Ordering::AcqRel);
        if pending > 0 {
            self.0.dropped.fetch_add(pending, Ordering::Relaxed);
            tracing::warn!(
                pending,
                "Channel file-log worker exited with admitted records undelivered"
            );
        }
        self.0.worker_running.store(false, Ordering::Release);
    }
}

impl Drop for LogWorker {
    fn drop(&mut self) {
        self.shutdown_in_reaper();
    }
}

fn flush_sink<S: LogSink>(sink: &mut S, counters: &FileLogCounters) -> io::Result<()> {
    let _operation = IoOperationGuard::begin(&counters.io_operation_started_ms);
    match sink.flush() {
        Ok(()) => {
            counters.io_healthy.store(true, Ordering::Release);
            Ok(())
        },
        Err(error) => {
            counters.flush_failures.fetch_add(1, Ordering::Relaxed);
            counters.io_healthy.store(false, Ordering::Release);
            tracing::warn!(%error, "Channel file-log batch flush failed");
            Err(error)
        },
    }
}

fn run_log_worker<S: LogSink>(
    receiver: Receiver<LogRecord>,
    mut sink: S,
    counters: Arc<FileLogCounters>,
    flush_batch_size: usize,
    flush_interval: Duration,
) -> io::Result<()> {
    let mut dirty = 0_usize;
    let mut unresolved_error = None;
    let mut next_flush = Instant::now() + flush_interval;
    loop {
        let timeout = next_flush.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(record) => {
                let write_result = {
                    let _operation = IoOperationGuard::begin(&counters.io_operation_started_ms);
                    sink.write(&record)
                };
                if let Err(error) = write_result {
                    counters.write_failures.fetch_add(1, Ordering::Relaxed);
                    counters.io_healthy.store(false, Ordering::Release);
                    tracing::warn!(
                        channel_id = record.channel_id,
                        %error,
                        "Channel file-log write failed"
                    );
                    unresolved_error = Some(io::Error::new(error.kind(), error.to_string()));
                } else {
                    dirty = dirty.saturating_add(1);
                }
                let rotation_flush_failures = sink.take_rotation_flush_failures();
                if rotation_flush_failures > 0 {
                    counters
                        .flush_failures
                        .fetch_add(rotation_flush_failures, Ordering::Relaxed);
                    counters.io_healthy.store(false, Ordering::Release);
                }
                counters.pending.fetch_sub(1, Ordering::Relaxed);
                if dirty >= flush_batch_size {
                    match flush_sink(&mut sink, &counters) {
                        Ok(()) => {
                            dirty = 0;
                            unresolved_error = None;
                        },
                        Err(error) => {
                            unresolved_error =
                                Some(io::Error::new(error.kind(), error.to_string()));
                        },
                    }
                    next_flush = Instant::now() + flush_interval;
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty > 0 {
                    match flush_sink(&mut sink, &counters) {
                        Ok(()) => {
                            dirty = 0;
                            unresolved_error = None;
                        },
                        Err(error) => {
                            unresolved_error =
                                Some(io::Error::new(error.kind(), error.to_string()));
                        },
                    }
                }
                next_flush = Instant::now() + flush_interval;
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if dirty > 0 {
        match flush_sink(&mut sink, &counters) {
            Ok(()) => unresolved_error = None,
            Err(error) => {
                unresolved_error = Some(io::Error::new(error.kind(), error.to_string()));
            },
        }
    }
    unresolved_error.map_or(Ok(()), Err)
}

/// File-based channel log handler.
///
/// Writes channel logs to per-channel, per-day log files.
/// Thread-safe through internal `Mutex` on file handles.
///
/// The log level can be changed dynamically at runtime via `set_level()`.
pub struct ChannelFileLogHandler {
    /// Base directory for log files.
    base_dir: PathBuf,
    /// Mapping from channel_id to channel name.
    channel_names: HashMap<u32, String>,
    /// Bounded non-blocking admission into the dedicated disk thread.
    worker: Arc<LogWorker>,
    /// Log level filter (stored as AtomicU8 for hot-reload support).
    level: AtomicU8,
}

impl ChannelFileLogHandler {
    /// Create a new file log handler with the given base directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let handler = ChannelFileLogHandler::new("/logs/io/channels", worker)
    ///     .with_level(FileLogLevel::Debug)
    ///     .with_channel(1, "PCS#1");
    /// ```
    pub(crate) fn new(base_dir: impl Into<PathBuf>, worker: Arc<LogWorker>) -> Self {
        Self {
            base_dir: base_dir.into(),
            channel_names: HashMap::new(),
            worker,
            level: AtomicU8::new(FileLogLevel::default().as_u8()),
        }
    }

    #[cfg(test)]
    fn with_sink_for_test<S: LogSink>(sink: S, config: WorkerConfig) -> Self {
        Self {
            base_dir: PathBuf::from("/test/file-log"),
            channel_names: HashMap::new(),
            worker: LogWorker::spawn(sink, config).expect("test file-log worker starts"),
            level: AtomicU8::new(FileLogLevel::default().as_u8()),
        }
    }

    /// Returns cumulative admission and disk-worker counters.
    #[must_use]
    pub fn stats(&self) -> FileLogStats {
        self.worker.stats()
    }

    /// Set the log level (builder pattern).
    #[must_use]
    pub fn with_level(self, level: FileLogLevel) -> Self {
        self.level.store(level.as_u8(), Ordering::Relaxed);
        self
    }

    /// Get the current log level.
    #[must_use]
    pub fn level(&self) -> FileLogLevel {
        FileLogLevel::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Set the log level dynamically at runtime.
    ///
    /// This method is thread-safe and can be called while the handler is
    /// actively processing log events.
    pub fn set_level(&self, level: FileLogLevel) {
        self.level.store(level.as_u8(), Ordering::Relaxed);
    }

    /// Register a channel with its name.
    #[must_use]
    pub fn with_channel(mut self, channel_id: u32, channel_name: impl Into<String>) -> Self {
        self.channel_names.insert(channel_id, channel_name.into());
        self
    }

    /// Sanitize channel name for use as directory name.
    /// Replaces invalid filesystem characters with underscore.
    fn sanitize_name(name: &str) -> String {
        name.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '#' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Get or create the directory for a channel.
    fn get_channel_dir(&self, channel_id: u32) -> PathBuf {
        let channel_name = self
            .channel_names
            .get(&channel_id)
            .map(|s| Self::sanitize_name(s))
            .unwrap_or_else(|| format!("channel_{}", channel_id));

        self.base_dir.join(channel_name)
    }

    /// Format a raw packet event as log line.
    ///
    /// Output format:
    /// - TCP: `>>> modbus [TID=D596] [slave=2 fc=0x03 @100-163] [87B] D5 96 ...`
    /// - RTU: `>>> modbus [slave=2 fc=0x03 @100-163] [12B] 02 03 ...`
    fn format_raw_packet(
        &self,
        direction: &PacketDirection,
        data: &[u8],
        metadata: &PacketMetadata,
    ) -> String {
        let mut line = String::with_capacity(128);

        // Direction arrow
        let arrow = match direction {
            PacketDirection::Send => ">>>",
            PacketDirection::Receive => "<<<",
        };

        // Protocol name and metadata
        let proto_info = match metadata {
            PacketMetadata::Modbus {
                slave_id,
                function_code,
                transaction_id,
                start_address,
                quantity,
                ..
            } => {
                let mut info = String::with_capacity(64);

                // TID (TCP only)
                if let Some(tid) = transaction_id {
                    let _ = write!(info, "[TID={:04X}] ", tid);
                }

                // Base info: slave and function code
                let _ = write!(info, "[slave={} fc=0x{:02X}", slave_id, function_code);

                // Address range (if available)
                if let (Some(start), Some(qty)) = (start_address, quantity)
                    && *qty > 0
                {
                    let end = start.saturating_add(qty.saturating_sub(1));
                    let _ = write!(info, " @{}-{}", start, end);
                }

                info.push(']');
                format!("modbus {}", info)
            },
            PacketMetadata::Iec104 {
                asdu_type,
                cause_of_tx,
                common_addr,
            } => {
                format!(
                    "iec104 [type={} cot={} ca={}]",
                    asdu_type, cause_of_tx, common_addr
                )
            },
            PacketMetadata::J1939 {
                pgn,
                source,
                destination,
            } => {
                format!("j1939 [pgn={} src={} dst={}]", pgn, source, destination)
            },
            PacketMetadata::OpcUa {
                message_type,
                request_id,
            } => {
                format!("opcua [msg={} req={}]", message_type, request_id)
            },
            PacketMetadata::Gpio => "gpio".to_string(),
            PacketMetadata::Other { protocol } => protocol.clone(),
        };

        // Format: >>> modbus [TID=D596] [slave=1 fc=0x03 @100-163] [12B] 00 01 ...
        let _ = write!(line, "{} {} [{}B] ", arrow, proto_info, data.len());

        // Append hex data
        for (i, byte) in data.iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            let _ = write!(line, "{:02X}", byte);
        }

        line
    }

    fn reject_oversized_raw_packet(&self, data_len: usize) -> bool {
        if data_len <= MAX_RAW_PACKET_BYTES {
            return false;
        }
        self.worker.reject_oversized(data_len.saturating_mul(3));
        true
    }

    /// Format an error event as log line.
    fn format_error(&self, error: &str, context: &ErrorContext) -> String {
        format!("[ERROR] [{}] {}", context, error)
    }

    /// Format a poll cycle event as log line (debug mode only).
    fn format_poll_cycle(
        &self,
        points_count: usize,
        success_count: usize,
        failed_count: usize,
        duration_ms: u64,
    ) -> String {
        format!(
            "[POLL] points={} ok={} fail={} ({}ms)",
            points_count, success_count, failed_count, duration_ms
        )
    }

    /// Format a state change event as log line (debug mode only).
    fn format_state_change(
        &self,
        old_state: &crate::protocols::core::ConnectionState,
        new_state: &crate::protocols::core::ConnectionState,
    ) -> String {
        format!("[STATE] {} -> {}", old_state, new_state)
    }

    /// Format a write result (control or adjustment) as log line.
    fn format_write_result(
        tag: &str,
        commands_count: usize,
        result: &Result<crate::protocols::core::WriteResult, String>,
        duration_ms: u64,
    ) -> String {
        match result {
            Ok(wr) => format!(
                "[{}] cmds={} ok ({}) ({}ms)",
                tag, commands_count, wr.success_count, duration_ms
            ),
            Err(e) => format!(
                "[{}] cmds={} FAILED: {} ({}ms)",
                tag, commands_count, e, duration_ms
            ),
        }
    }

    /// Admit one formatted line without waiting for filesystem I/O.
    fn enqueue_log(&self, channel_id: u32, line: String) {
        if line.len() > MAX_RECORD_BYTES {
            self.worker.reject_oversized(line.len());
            return;
        }
        let now = Local::now();
        self.worker.enqueue(LogRecord {
            channel_id,
            channel_dir: self.get_channel_dir(channel_id),
            date: now.date_naive(),
            timestamp: now.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            line,
        });
    }

    /// Check if the event should be logged based on the level.
    ///
    /// Log level mapping:
    /// - **Info**: Errors, connections, disconnections, control writes, **point values** (key events)
    /// - **Debug**: All of Info + raw packets, poll cycles, state changes, reconnects
    fn should_log(&self, event: &ChannelLogEvent) -> bool {
        let level = self.level(); // Atomic read for hot-reload support
        match event {
            // Always log these at Info level (key events)
            ChannelLogEvent::Error { .. } => true,
            ChannelLogEvent::Connected { .. } => true,
            ChannelLogEvent::Disconnected { .. } => true,
            ChannelLogEvent::ControlWrite { .. } => true, // Control commands are important
            ChannelLogEvent::AdjustmentWrite { .. } => true, // Adjustment commands are important
            ChannelLogEvent::PointValues { .. } => true,  // Point values at Info level

            // Debug level only (verbose logging)
            ChannelLogEvent::RawPacket { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::PollCycleCompleted { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::StateChanged { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::ReconnectAttempt { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::ReconnectSuccess { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::ReadOperation { .. } => level == FileLogLevel::Debug,
        }
    }

    /// Format a log line with optional group ID prefix.
    ///
    /// If group_id is Some, prepends `[G001] ` to the line.
    fn format_with_group_id(&self, group_id: Option<u32>, line: &str) -> String {
        if let Some(gid) = group_id {
            format!("[G{:03}] {}", gid % 1000, line)
        } else {
            line.to_string()
        }
    }

    /// Format point values grouped by type.
    ///
    /// Returns multiple log lines, one per point type present in the values.
    /// Format: `[T] 1001:23.5, 1002:45.2` or `[S] 2001:1, 2002:0!` (! = bad quality)
    fn format_point_values_by_type(
        &self,
        values: &[super::logging::PointValueSummary],
        group_id: Option<u32>,
    ) -> Result<Vec<String>, usize> {
        use aether_core::PointType;
        let mut lines: [Option<String>; 4] = std::array::from_fn(|_| None);
        for value in values {
            let (index, tag) = match value.point_type {
                PointType::Telemetry => (0, "T"),
                PointType::Signal => (1, "S"),
                PointType::Control => (2, "C"),
                PointType::Adjustment => (3, "A"),
            };
            let has_values = lines[index].is_some();
            let line = lines[index]
                .get_or_insert_with(|| self.format_with_group_id(group_id, &format!("[{tag}] ")));
            let id = value.id.to_string();
            let separator_len = usize::from(has_values) * 2;
            let quality_len =
                usize::from(!matches!(value.quality, aether_domain::PointQuality::Good));
            let next_len = line
                .len()
                .saturating_add(separator_len)
                .saturating_add(id.len())
                .saturating_add(1)
                .saturating_add(value.value.len())
                .saturating_add(quality_len);
            if next_len > MAX_RECORD_BYTES {
                return Err(next_len);
            }
            if separator_len > 0 {
                line.push_str(", ");
            }
            line.push_str(&id);
            line.push(':');
            line.push_str(&value.value);
            if quality_len > 0 {
                line.push('!');
            }
        }
        Ok(lines.into_iter().flatten().collect())
    }
}

#[async_trait]
impl ChannelLogHandler for ChannelFileLogHandler {
    async fn on_log(&self, channel_id: u32, event: &ChannelLogEvent) {
        // Level filter
        if !self.should_log(event) {
            return;
        }

        let line = match event {
            ChannelLogEvent::RawPacket {
                direction,
                data,
                metadata,
                group_id,
                ..
            } => {
                if self.reject_oversized_raw_packet(data.len()) {
                    return;
                }
                let packet_line = self.format_raw_packet(direction, data, metadata);
                self.format_with_group_id(*group_id, &packet_line)
            },

            ChannelLogEvent::Error { error, context, .. } => self.format_error(error, context),

            ChannelLogEvent::Connected {
                endpoint,
                duration_ms,
                ..
            } => {
                format!("[CONNECTED] {} ({}ms)", endpoint, duration_ms)
            },

            ChannelLogEvent::Disconnected { reason, .. } => {
                let reason_str = reason.as_deref().unwrap_or("intentional");
                format!("[DISCONNECTED] reason={}", reason_str)
            },

            ChannelLogEvent::PollCycleCompleted {
                points_count,
                success_count,
                failed_count,
                duration_ms,
                ..
            } => self.format_poll_cycle(*points_count, *success_count, *failed_count, *duration_ms),

            ChannelLogEvent::StateChanged {
                old_state,
                new_state,
                ..
            } => self.format_state_change(old_state, new_state),

            ChannelLogEvent::ControlWrite {
                commands,
                result,
                duration_ms,
                ..
            } => Self::format_write_result("CONTROL", commands.len(), result, *duration_ms),

            ChannelLogEvent::AdjustmentWrite {
                commands,
                result,
                duration_ms,
                ..
            } => Self::format_write_result("ADJUST", commands.len(), result, *duration_ms),

            ChannelLogEvent::ReconnectAttempt {
                attempt,
                max_attempts,
                next_retry_ms,
                ..
            } => {
                let max_str = max_attempts
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "∞".to_string());
                let retry_str = next_retry_ms
                    .map(|ms| format!(" retry in {}ms", ms))
                    .unwrap_or_default();
                format!("[RECONNECT] attempt {}/{}{}", attempt, max_str, retry_str)
            },

            ChannelLogEvent::ReconnectSuccess {
                total_attempts,
                total_duration_ms,
                ..
            } => {
                format!(
                    "[RECONNECT] SUCCESS after {} attempts ({}ms)",
                    total_attempts, total_duration_ms
                )
            },

            ChannelLogEvent::ReadOperation { .. } => {
                // Skip detailed read operations in file log
                return;
            },

            ChannelLogEvent::PointValues {
                values, group_id, ..
            } => {
                // Point values: output multiple lines, one per type
                let lines = match self.format_point_values_by_type(values, *group_id) {
                    Ok(lines) => lines,
                    Err(bytes) => {
                        self.worker.reject_oversized(bytes);
                        return;
                    },
                };
                for line in lines {
                    self.enqueue_log(channel_id, line);
                }
                return;
            },
        };

        self.enqueue_log(channel_id, line);
    }

    fn set_log_level(&self, level: &str) {
        if let Some(new_level) = FileLogLevel::parse(Some(level)) {
            self.set_level(new_level);
        }
    }
}

/// Benchmark-only injection seam for proving that slow disk work remains on
/// the dedicated blocking worker instead of delaying the async channel loop.
#[cfg(feature = "bench-support")]
pub mod bench_support {
    use super::*;
    use std::sync::Condvar;

    /// Raw measurements from one deterministic blocked-disk run.
    #[derive(Debug)]
    pub struct SlowLogBenchmarkSample {
        pub minimum_attempts: u64,
        pub attempts: u64,
        pub queue_capacity: u64,
        pub accepted: u64,
        pub dropped: u64,
        pub pending_at_saturation: u64,
        pub max_pending_observed: u64,
        pub writes_after_drain: u64,
        pub admission_latency_observations: u64,
        pub admission_latency_ns: Vec<u64>,
        pub admission_window_ns: u64,
        pub ticker_interval_ns: u64,
        pub ticker_window_ns: u64,
        pub ticker_lateness_ns: Vec<u64>,
    }

    const MAX_ADMISSION_LATENCY_SAMPLES: usize = 65_536;

    struct SlowLogSink {
        gate: Arc<(Mutex<bool>, Condvar)>,
        entered: mpsc::Sender<()>,
        writes: Arc<AtomicU64>,
        first_write: bool,
    }

    impl LogSink for SlowLogSink {
        fn write(&mut self, _record: &LogRecord) -> io::Result<()> {
            if self.first_write {
                self.first_write = false;
                let _ = self.entered.send(());
                let (released, wake) = self.gate.as_ref();
                let mut released = released
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while !*released {
                    released = wake
                        .wait(released)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ReleaseSlowLogSink(Arc<(Mutex<bool>, Condvar)>);

    impl ReleaseSlowLogSink {
        fn release(&self) {
            let (released, wake) = self.0.as_ref();
            *released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            wake.notify_all();
        }
    }

    impl Drop for ReleaseSlowLogSink {
        fn drop(&mut self) {
            self.release();
        }
    }

    fn duration_ns(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    async fn ticker_lateness(interval: Duration, samples: u64) -> Vec<u64> {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Tokio intervals complete the first tick immediately. It is startup,
        // not a one-millisecond scheduling observation.
        ticker.tick().await;
        let mut lateness = Vec::with_capacity(samples as usize);
        for _ in 0..samples {
            let scheduled = ticker.tick().await;
            lateness.push(duration_ns(
                tokio::time::Instant::now().saturating_duration_since(scheduled),
            ));
        }
        lateness
    }

    fn record_latency_sample(
        samples: &mut Vec<u64>,
        observations: u64,
        sample: u64,
        reservoir_state: &mut u64,
    ) {
        if samples.len() < MAX_ADMISSION_LATENCY_SAMPLES {
            samples.push(sample);
            return;
        }

        // Deterministic xorshift reservoir sampling keeps the JSON bounded
        // while retaining observations from the full admission window.
        *reservoir_state ^= *reservoir_state << 13;
        *reservoir_state ^= *reservoir_state >> 7;
        *reservoir_state ^= *reservoir_state << 17;
        let candidate = *reservoir_state % observations;
        if candidate < MAX_ADMISSION_LATENCY_SAMPLES as u64 {
            samples[candidate as usize] = sample;
        }
    }

    /// Admit logs asynchronously while the injected sink holds its first disk
    /// write. Queue accounting and the bound are deterministic; admission and
    /// ticker timing values are measurements and intentionally have no limit.
    pub async fn run_slow_log_benchmark(
        minimum_attempts: u64,
        queue_capacity: usize,
        ticker_interval: Duration,
        ticker_samples: u64,
    ) -> Result<SlowLogBenchmarkSample, String> {
        if queue_capacity == 0 || minimum_attempts <= queue_capacity as u64 + 1 {
            return Err(
                "slow-log benchmark attempts must exceed queue capacity plus in-flight write"
                    .to_string(),
            );
        }
        if ticker_interval.is_zero() || ticker_samples == 0 {
            return Err("slow-log ticker configuration must be non-zero".to_string());
        }
        let ticker_multiplier = u32::try_from(ticker_samples)
            .map_err(|_| "slow-log ticker sample count exceeds duration multiplier".to_string())?;
        let ticker_window = ticker_interval
            .checked_mul(ticker_multiplier)
            .ok_or_else(|| "slow-log ticker window overflowed Duration".to_string())?;

        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let writes = Arc::new(AtomicU64::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = LogWorker::spawn(
            SlowLogSink {
                gate: Arc::clone(&gate),
                entered: entered_tx,
                writes: Arc::clone(&writes),
                first_write: true,
            },
            WorkerConfig {
                queue_capacity,
                flush_batch_size: usize::MAX,
                flush_interval: Duration::from_secs(60),
                shutdown_drain: Duration::from_secs(5),
            },
        )
        .map_err(|error| format!("spawn slow-log worker: {error}"))?;
        let release_sink = ReleaseSlowLogSink(Arc::clone(&gate));
        let handler = ChannelFileLogHandler::new("/bench/no-file-io", Arc::clone(&worker));
        let event = ChannelLogEvent::Error {
            timestamp: std::time::SystemTime::now(),
            error: "synthetic blocked disk".to_string(),
            context: ErrorContext::Polling,
        };

        let mut admission_latency_ns = Vec::with_capacity(
            usize::try_from(minimum_attempts)
                .unwrap_or(MAX_ADMISSION_LATENCY_SAMPLES)
                .min(MAX_ADMISSION_LATENCY_SAMPLES),
        );
        let mut attempts = 1_u64;
        let mut reservoir_state = 0x4d59_5df4_d0f3_3173_u64;
        let started = Instant::now();
        handler.on_log(7, &event).await;
        record_latency_sample(
            &mut admission_latency_ns,
            attempts,
            duration_ns(started.elapsed()),
            &mut reservoir_state,
        );
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("slow-log sink did not enter first write: {error}"))?;

        let admission_window_started = Instant::now();
        let coverage_deadline = tokio::time::Instant::now() + ticker_window;
        // This is only a liveness guard. It is not a performance threshold and
        // never changes a successful timing measurement.
        let hard_deadline = coverage_deadline + Duration::from_secs(5);
        let ticker = tokio::spawn(ticker_lateness(ticker_interval, ticker_samples));
        let mut max_pending_observed = handler.stats().pending;
        while attempts < minimum_attempts
            || tokio::time::Instant::now() < coverage_deadline
            || !ticker.is_finished()
        {
            if tokio::time::Instant::now() >= hard_deadline {
                ticker.abort();
                return Err(format!(
                    "one-millisecond ticker did not complete within its {:?} window plus 5s guard after {attempts} admissions",
                    ticker_window
                ));
            }
            let started = Instant::now();
            handler.on_log(7, &event).await;
            attempts = attempts.saturating_add(1);
            record_latency_sample(
                &mut admission_latency_ns,
                attempts,
                duration_ns(started.elapsed()),
                &mut reservoir_state,
            );
            max_pending_observed = max_pending_observed.max(handler.stats().pending);
            if attempts.is_multiple_of(32) {
                tokio::task::yield_now().await;
            }
        }
        let ticker_lateness_ns = ticker
            .await
            .map_err(|error| format!("one-millisecond ticker task failed: {error}"))?;
        let admission_window_ns = duration_ns(admission_window_started.elapsed());
        let saturated = handler.stats();

        release_sink.release();
        worker
            .shutdown_blocking()
            .map_err(|error| format!("drain slow-log worker: {error}"))?;
        let writes_after_drain = writes.load(Ordering::Relaxed);

        let expected_accepted = queue_capacity as u64 + 1;
        if saturated.accepted != expected_accepted {
            return Err(format!(
                "bounded slow-log admission accepted {}, expected {expected_accepted}",
                saturated.accepted
            ));
        }
        if saturated.accepted.saturating_add(saturated.dropped) != attempts {
            return Err(format!(
                "slow-log accounting mismatch: accepted={} dropped={} attempts={attempts}",
                saturated.accepted, saturated.dropped
            ));
        }
        if saturated.pending > expected_accepted || max_pending_observed > expected_accepted {
            return Err(format!(
                "slow-log pending exceeded bounded queue plus in-flight write: snapshot={} peak={max_pending_observed} bound={expected_accepted}",
                saturated.pending
            ));
        }
        if writes_after_drain != saturated.accepted {
            return Err(format!(
                "slow-log drain wrote {writes_after_drain}, expected {}",
                saturated.accepted
            ));
        }

        Ok(SlowLogBenchmarkSample {
            minimum_attempts,
            attempts,
            queue_capacity: queue_capacity as u64,
            accepted: saturated.accepted,
            dropped: saturated.dropped,
            pending_at_saturation: saturated.pending,
            max_pending_observed,
            writes_after_drain,
            admission_latency_observations: attempts,
            admission_latency_ns,
            admission_window_ns,
            ticker_interval_ns: duration_ns(ticker_interval),
            ticker_window_ns: duration_ns(ticker_window),
            ticker_lateness_ns,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use std::time::SystemTime;
    use tempfile::TempDir;

    struct BlockingCountingSink {
        release: mpsc::Receiver<()>,
        entered: mpsc::Sender<()>,
        writes: Arc<AtomicU64>,
        flushes: Arc<AtomicU64>,
    }

    impl LogSink for BlockingCountingSink {
        fn write(&mut self, _record: &LogRecord) -> io::Result<()> {
            let _ = self.entered.send(());
            self.release
                .recv()
                .map_err(|_| io::Error::other("test writer release closed"))?;
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct PanickingSink;

    impl LogSink for PanickingSink {
        fn write(&mut self, _record: &LogRecord) -> io::Result<()> {
            panic!("synthetic disk worker panic");
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ControlledFlushSink {
        flushes: Arc<AtomicU64>,
        failures_before_success: u64,
    }

    impl LogSink for ControlledFlushSink {
        fn write(&mut self, _record: &LogRecord) -> io::Result<()> {
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            let attempt = self.flushes.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.failures_before_success {
                Err(io::Error::other("synthetic file-log flush failure"))
            } else {
                Ok(())
            }
        }
    }

    fn error_event() -> ChannelLogEvent {
        ChannelLogEvent::Error {
            timestamp: SystemTime::now(),
            error: "synthetic disk latency".to_string(),
            context: ErrorContext::Polling,
        }
    }

    async fn wait_for_counter(counter: &AtomicU64, expected: u64) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while counter.load(Ordering::SeqCst) < expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("background log worker reached expected count");
    }

    #[tokio::test]
    async fn on_log_does_not_wait_for_slow_disk_writer() {
        let writes = Arc::new(AtomicU64::new(0));
        let (release_tx, release_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let handler = ChannelFileLogHandler::with_sink_for_test(
            BlockingCountingSink {
                release: release_rx,
                entered: entered_tx,
                writes: Arc::clone(&writes),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig {
                queue_capacity: 8,
                flush_batch_size: 8,
                flush_interval: Duration::from_secs(1),
                shutdown_drain: Duration::from_millis(20),
            },
        );

        handler.on_log(7, &error_event()).await;
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("disk worker entered its blocking write");
        tokio::time::timeout(Duration::from_secs(1), handler.on_log(7, &error_event()))
            .await
            .expect("admission completed while disk worker remained blocked");

        assert_eq!(handler.stats().accepted, 2);
        assert_eq!(handler.stats().pending, 2);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            handler.stats().io_stalled,
            "health must detect a disk operation that outlives the shutdown/stall budget"
        );
        release_tx.send(()).expect("release first write");
        release_tx.send(()).expect("release second write");
        wait_for_counter(&writes, 2).await;
        assert!(!handler.stats().io_stalled);
    }

    #[tokio::test]
    async fn worker_flushes_a_batch_instead_of_every_log_line() {
        let writes = Arc::new(AtomicU64::new(0));
        let flushes = Arc::new(AtomicU64::new(0));
        let handler = ChannelFileLogHandler::with_sink_for_test(
            BlockingCountingSink {
                release: {
                    let (tx, rx) = mpsc::channel();
                    for _ in 0..3 {
                        tx.send(()).expect("prime test writer");
                    }
                    rx
                },
                entered: mpsc::channel().0,
                writes: Arc::clone(&writes),
                flushes: Arc::clone(&flushes),
            },
            WorkerConfig {
                queue_capacity: 8,
                flush_batch_size: 3,
                flush_interval: Duration::from_secs(10),
                shutdown_drain: Duration::from_millis(100),
            },
        );

        for _ in 0..3 {
            handler.on_log(7, &error_event()).await;
        }
        wait_for_counter(&flushes, 1).await;

        assert_eq!(writes.load(Ordering::SeqCst), 3);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert_eq!(handler.stats().pending, 0);
    }

    #[tokio::test]
    async fn full_queue_drops_without_growing_and_reports_the_loss() {
        let (release_tx, release_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let handler = ChannelFileLogHandler::with_sink_for_test(
            BlockingCountingSink {
                release: release_rx,
                entered: entered_tx,
                writes: Arc::new(AtomicU64::new(0)),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 8,
                flush_interval: Duration::from_secs(1),
                shutdown_drain: Duration::from_millis(20),
            },
        );

        handler.on_log(7, &error_event()).await;
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker holds the first record");
        handler.on_log(7, &error_event()).await;
        handler.on_log(7, &error_event()).await;

        let stats = handler.stats();
        assert_eq!(stats.accepted, 2);
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.pending, 2);
        release_tx.send(()).expect("release first write");
        release_tx.send(()).expect("release queued write");
    }

    #[tokio::test]
    async fn oversized_record_is_rejected_before_queue_admission() {
        let writes = Arc::new(AtomicU64::new(0));
        let handler = ChannelFileLogHandler::with_sink_for_test(
            BlockingCountingSink {
                release: mpsc::channel().1,
                entered: mpsc::channel().0,
                writes: Arc::clone(&writes),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 1,
                flush_interval: Duration::from_secs(1),
                shutdown_drain: Duration::from_millis(20),
            },
        );

        handler.enqueue_log(7, "x".repeat(MAX_RECORD_BYTES + 1));

        let stats = handler.stats();
        assert_eq!(stats.accepted, 0);
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.oversized, 1);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn last_handler_drop_does_not_wait_for_blocked_disk_drain() {
        let (release_tx, release_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let handler = ChannelFileLogHandler::with_sink_for_test(
            BlockingCountingSink {
                release: release_rx,
                entered: entered_tx,
                writes: Arc::new(AtomicU64::new(0)),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 1,
                flush_interval: Duration::from_secs(1),
                shutdown_drain: Duration::from_secs(10),
            },
        );
        handler.on_log(7, &error_event()).await;
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("disk worker is blocked");

        let (dropped_tx, dropped_rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(handler);
            let _ = dropped_tx.send(());
        });
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handler drop returns before disk drain completes");

        release_tx.send(()).expect("release disk worker");
    }

    #[tokio::test]
    async fn worker_panic_reconciles_pending_as_dropped_and_stops_reuse() {
        let worker = LogWorker::spawn(PanickingSink, WorkerConfig::default()).expect("test worker");
        let handler = ChannelFileLogHandler::new("/test", Arc::clone(&worker));
        handler.on_log(7, &error_event()).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while worker.stats().worker_running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("panicked worker becomes observable");

        let stats = worker.stats();
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.dropped, 1);
        assert!(worker.shutdown_blocking().is_err());
    }

    #[tokio::test]
    async fn explicit_shutdown_timeout_preserves_state_for_a_second_real_join() {
        let (release_tx, release_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let writes = Arc::new(AtomicU64::new(0));
        let worker = LogWorker::spawn(
            BlockingCountingSink {
                release: release_rx,
                entered: entered_tx,
                writes: Arc::clone(&writes),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 1,
                flush_interval: Duration::from_secs(1),
                shutdown_drain: Duration::from_millis(10),
            },
        )
        .expect("test worker");
        let handler = ChannelFileLogHandler::new("/test", Arc::clone(&worker));
        handler.on_log(7, &error_event()).await;
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("disk worker blocked");

        let error = worker
            .shutdown_blocking()
            .expect_err("bounded drain reports its timeout");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(worker.stats().pending, 1);
        assert_eq!(worker.stats().shutdown_timeouts, 1);
        assert!(worker.stats().worker_running);
        assert!(!worker.stats().io_healthy);
        assert!(
            worker
                .thread
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some(),
            "timeout must retain the JoinHandle"
        );

        release_tx.send(()).expect("release disk worker");
        wait_for_counter(&writes, 1).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while worker.stats().worker_running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker eventually finishes after release");

        worker
            .shutdown_blocking()
            .expect("second shutdown observes completion and joins the worker");
        assert_eq!(worker.stats().pending, 0);
        assert!(worker.stats().io_healthy);
        assert!(
            worker
                .thread
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none(),
            "successful retry must consume the joined handle"
        );
    }

    #[tokio::test]
    async fn final_flush_failure_is_reported_by_shutdown() {
        let flushes = Arc::new(AtomicU64::new(0));
        let worker = LogWorker::spawn(
            ControlledFlushSink {
                flushes: Arc::clone(&flushes),
                failures_before_success: u64::MAX,
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 8,
                flush_interval: Duration::from_secs(10),
                shutdown_drain: Duration::from_secs(1),
            },
        )
        .expect("test worker");
        let handler = ChannelFileLogHandler::new("/test", Arc::clone(&worker));
        handler.on_log(7, &error_event()).await;

        let error = worker
            .shutdown_blocking()
            .expect_err("final flush failure must escape worker shutdown");

        assert!(
            error
                .to_string()
                .contains("synthetic file-log flush failure")
        );
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert_eq!(worker.stats().flush_failures, 1);
        assert!(!worker.stats().io_healthy);
        assert!(!worker.stats().worker_running);
        assert!(
            worker.shutdown_blocking().is_err(),
            "idempotent shutdown must retain the terminal failure"
        );
    }

    #[tokio::test]
    async fn successful_flush_recovers_current_io_health_after_a_failure() {
        let flushes = Arc::new(AtomicU64::new(0));
        let worker = LogWorker::spawn(
            ControlledFlushSink {
                flushes: Arc::clone(&flushes),
                failures_before_success: 1,
            },
            WorkerConfig {
                queue_capacity: 2,
                flush_batch_size: 1,
                flush_interval: Duration::from_secs(10),
                shutdown_drain: Duration::from_secs(1),
            },
        )
        .expect("test worker");
        let handler = ChannelFileLogHandler::new("/test", Arc::clone(&worker));

        handler.on_log(7, &error_event()).await;
        wait_for_counter(&flushes, 1).await;
        assert_eq!(worker.stats().flush_failures, 1);
        assert!(!worker.stats().io_healthy);

        handler.on_log(7, &error_event()).await;
        wait_for_counter(&flushes, 2).await;
        assert!(worker.stats().io_healthy);
        assert_eq!(worker.stats().flush_failures, 1);

        worker
            .shutdown_blocking()
            .expect("recovered worker shuts down cleanly");
    }

    #[test]
    fn sink_reopens_when_channel_directory_changes() {
        let directory = TempDir::new().expect("temporary log root");
        let old_dir = directory.path().join("old-name");
        let new_dir = directory.path().join("new-name");
        let date = Local::now().date_naive();
        let mut sink = FileLogSink::default();
        let record = |channel_dir: PathBuf, line: &str| LogRecord {
            channel_id: 7,
            channel_dir,
            date,
            timestamp: "2026-08-13 12:00:00.000".to_string(),
            line: line.to_string(),
        };

        sink.write(&record(old_dir.clone(), "before rename"))
            .expect("write old channel path");
        sink.write(&record(new_dir.clone(), "after rename"))
            .expect("flush old path and open new channel path");
        sink.flush().expect("flush new path");

        let filename = format!("{}.log", date.format("%Y-%m-%d"));
        let old = fs::read_to_string(old_dir.join(&filename)).expect("old log file");
        let new = fs::read_to_string(new_dir.join(filename)).expect("new log file");
        assert!(old.contains("before rename"));
        assert!(!old.contains("after rename"));
        assert!(new.contains("after rename"));
    }

    #[test]
    fn oversized_raw_packet_is_rejected_before_hex_formatting() {
        let writes = Arc::new(AtomicU64::new(0));
        let worker = LogWorker::spawn(
            BlockingCountingSink {
                release: mpsc::channel().1,
                entered: mpsc::channel().0,
                writes: Arc::clone(&writes),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig::default(),
        )
        .expect("test worker");
        let handler = ChannelFileLogHandler::new("/test", Arc::clone(&worker))
            .with_level(FileLogLevel::Debug);
        let event = ChannelLogEvent::RawPacket {
            timestamp: SystemTime::now(),
            direction: PacketDirection::Send,
            data: vec![0; MAX_RAW_PACKET_BYTES + 1],
            metadata: PacketMetadata::Other {
                protocol: "test".to_string(),
            },
            group_id: None,
        };

        futures::executor::block_on(handler.on_log(7, &event));

        assert_eq!(worker.stats().oversized, 1);
        assert_eq!(worker.stats().accepted, 0);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn oversized_point_values_stop_bounded_formatting_before_admission() {
        let writes = Arc::new(AtomicU64::new(0));
        let worker = LogWorker::spawn(
            BlockingCountingSink {
                release: mpsc::channel().1,
                entered: mpsc::channel().0,
                writes: Arc::clone(&writes),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig::default(),
        )
        .expect("test worker");
        let handler = ChannelFileLogHandler::new("/test", Arc::clone(&worker));
        let event = ChannelLogEvent::PointValues {
            timestamp: SystemTime::now(),
            values: vec![super::super::logging::PointValueSummary {
                id: 1,
                point_type: aether_core::PointType::Telemetry,
                value: "x".repeat(MAX_RECORD_BYTES),
                quality: aether_domain::PointQuality::Good,
            }],
            total_points: 1,
            group_id: Some(1),
        };

        futures::executor::block_on(handler.on_log(7, &event));

        assert_eq!(worker.stats().oversized, 1);
        assert_eq!(worker.stats().accepted, 0);
        assert_eq!(worker.stats().pending, 0);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(ChannelFileLogHandler::sanitize_name("PCS#1"), "PCS#1");
        assert_eq!(ChannelFileLogHandler::sanitize_name("BAMS/1"), "BAMS_1");
        assert_eq!(
            ChannelFileLogHandler::sanitize_name("test:name"),
            "test_name"
        );
        assert_eq!(ChannelFileLogHandler::sanitize_name("a b c"), "a_b_c");
    }

    #[test]
    fn test_file_log_level_from_str() {
        assert_eq!(FileLogLevel::parse(None), Some(FileLogLevel::Info));
        assert_eq!(FileLogLevel::parse(Some("info")), Some(FileLogLevel::Info));
        assert_eq!(
            FileLogLevel::parse(Some("debug")),
            Some(FileLogLevel::Debug)
        );
        assert_eq!(FileLogLevel::parse(Some("error")), Some(FileLogLevel::Info));
        for retired in ["INFO", "DEBUG", "warn", "verbose"] {
            assert_eq!(FileLogLevel::parse(Some(retired)), None);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_file_log_handler() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let handler = ChannelFileLogHandler::new(
            temp_dir.path(),
            LogWorker::spawn_default().expect("start test file-log worker"),
        )
        .with_level(FileLogLevel::Debug)
        .with_channel(1, "TestChannel#1");

        // Log a raw packet
        let event = ChannelLogEvent::RawPacket {
            timestamp: SystemTime::now(),
            direction: PacketDirection::Send,
            data: vec![
                0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x64, 0x00, 0x0A,
            ],
            metadata: PacketMetadata::modbus_tcp(1, 0x03),
            group_id: None,
        };

        handler.on_log(1, &event).await;
        while handler.stats().pending != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(DEFAULT_FLUSH_INTERVAL + Duration::from_millis(20)).await;

        // Verify file was created
        let channel_dir = temp_dir.path().join("TestChannel#1");
        assert!(channel_dir.exists());

        let today = Local::now().date_naive();
        let log_file = channel_dir.join(format!("{}.log", today.format("%Y-%m-%d")));
        assert!(log_file.exists());

        // Verify content
        let content = fs::read_to_string(&log_file).expect("Failed to read log file");
        assert!(content.contains(">>> modbus [slave=1 fc=0x03]"));
        assert!(content.contains("00 01 00 00 00 06 01 03 00 64 00 0A"));
    }

    #[test]
    fn test_dynamic_level_change() {
        let handler = ChannelFileLogHandler::new(
            "/tmp",
            LogWorker::spawn_default().expect("start test file-log worker"),
        )
        .with_level(FileLogLevel::Info);

        // Initial level should be Info
        assert_eq!(handler.level(), FileLogLevel::Info);

        // Change to Debug dynamically
        handler.set_level(FileLogLevel::Debug);
        assert_eq!(handler.level(), FileLogLevel::Debug);

        // Change back to Info
        handler.set_level(FileLogLevel::Info);
        assert_eq!(handler.level(), FileLogLevel::Info);

        // Test via trait method (simulates API call)
        use crate::protocols::core::logging::ChannelLogHandler;
        handler.set_log_level("debug");
        assert_eq!(handler.level(), FileLogLevel::Debug);

        handler.set_log_level("info");
        assert_eq!(handler.level(), FileLogLevel::Info);

        // Invalid level should default to Info
        handler.set_log_level("invalid");
        assert_eq!(handler.level(), FileLogLevel::Info);
    }

    #[test]
    fn test_format_raw_packet() {
        let handler = ChannelFileLogHandler::new(
            "/tmp",
            LogWorker::spawn_default().expect("start test file-log worker"),
        );

        let line = handler.format_raw_packet(
            &PacketDirection::Send,
            &[0x00, 0x01, 0x00, 0x00, 0x00, 0x06],
            &PacketMetadata::modbus_tcp(1, 0x03),
        );

        assert_eq!(line, ">>> modbus [slave=1 fc=0x03] [6B] 00 01 00 00 00 06");
    }
}
