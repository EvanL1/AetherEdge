//! Rule execution logger
//!
//! Provides independent log files for each rule, capturing execution details
//! including variable values, matched conditions, and action results.

use std::{
    collections::HashMap,
    fmt::Write as FmtWrite,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
};

use crate::types::FlowCondition;
use chrono::{Local, NaiveDate, Utc};
use tracing::warn;

use crate::executor::{ActionResult, RuleExecutionResult};

const DEFAULT_QUEUE_CAPACITY: usize = 1_024;
const MAX_RECORD_BYTES: usize = 64 * 1_024;
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

/// Snapshot of bounded rule-log admission and disk-worker state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RuleLogStats {
    /// Records admitted to the bounded disk queue.
    pub accepted: u64,
    /// Records rejected because they were oversized or could not be queued.
    pub dropped: u64,
    /// Records rejected for exceeding the 64 KiB wire-to-disk limit.
    pub oversized: u64,
    /// Records that reached the worker but failed to write.
    pub write_failures: u64,
    /// Batches or rotations that failed to flush.
    pub flush_failures: u64,
    /// Admitted records that the disk worker has not processed yet.
    pub pending: u64,
    /// Disk-worker thread creation failures.
    pub worker_start_failures: u64,
    /// Explicit or reaper shutdowns that exceeded the drain deadline.
    pub shutdown_timeouts: u64,
    /// Whether the dedicated disk worker is still running.
    pub worker_running: bool,
}

#[derive(Default)]
struct RuleLogCounters {
    accepted: AtomicU64,
    dropped: AtomicU64,
    oversized: AtomicU64,
    write_failures: AtomicU64,
    flush_failures: AtomicU64,
    pending: AtomicU64,
    worker_start_failures: AtomicU64,
    shutdown_timeouts: AtomicU64,
    worker_running: AtomicBool,
}

impl RuleLogCounters {
    fn snapshot(&self) -> RuleLogStats {
        RuleLogStats {
            accepted: self.accepted.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            oversized: self.oversized.load(Ordering::Relaxed),
            write_failures: self.write_failures.load(Ordering::Relaxed),
            flush_failures: self.flush_failures.load(Ordering::Relaxed),
            pending: self.pending.load(Ordering::Relaxed),
            worker_start_failures: self.worker_start_failures.load(Ordering::Relaxed),
            shutdown_timeouts: self.shutdown_timeouts.load(Ordering::Relaxed),
            worker_running: self.worker_running.load(Ordering::Acquire),
        }
    }
}

struct RuleLogRecord {
    rule_id: String,
    log_dir: PathBuf,
    date: NaiveDate,
    line: String,
}

trait RuleLogSink: Send + 'static {
    fn write(&mut self, record: &RuleLogRecord) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;

    fn take_rotation_flush_failures(&mut self) -> u64 {
        0
    }
}

struct OpenRuleFile {
    date: NaiveDate,
    log_dir: PathBuf,
    writer: BufWriter<File>,
}

#[derive(Default)]
struct RuleFileLogSink {
    open_files: HashMap<String, OpenRuleFile>,
    rotation_flush_failures: u64,
}

impl RuleFileLogSink {
    fn ensure_writer(&mut self, record: &RuleLogRecord) -> io::Result<()> {
        let needs_new_file = self.open_files.get(&record.rule_id).is_none_or(|open| {
            open.date != record.date || open.log_dir != record.log_dir || !record.log_dir.exists()
        });
        if !needs_new_file {
            return Ok(());
        }

        if let Some(previous) = self.open_files.get_mut(&record.rule_id) {
            if let Err(error) = previous.writer.flush() {
                self.rotation_flush_failures = self.rotation_flush_failures.saturating_add(1);
                return Err(error);
            }
            self.open_files.remove(&record.rule_id);
        }

        fs::create_dir_all(&record.log_dir)?;
        let file_path = record.log_dir.join(format!(
            "{}_{}.log",
            record.date.format("%Y%m%d"),
            record.rule_id
        ));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)?;
        self.open_files.insert(
            record.rule_id.clone(),
            OpenRuleFile {
                date: record.date,
                log_dir: record.log_dir.clone(),
                writer: BufWriter::new(file),
            },
        );
        Ok(())
    }
}

impl RuleLogSink for RuleFileLogSink {
    fn write(&mut self, record: &RuleLogRecord) -> io::Result<()> {
        self.ensure_writer(record)?;
        let result = match self.open_files.get_mut(&record.rule_id) {
            Some(open) => writeln!(open.writer, "{}", record.line),
            None => Err(io::Error::other("rule-log writer missing after open")),
        };
        if result.is_err() {
            self.open_files.remove(&record.rule_id);
        }
        result
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

struct ShutdownHandles {
    completed: Receiver<WorkerCompletion>,
    thread: JoinHandle<()>,
}

#[derive(Debug, Clone, Copy)]
struct WorkerCompletion {
    final_flush_failed: bool,
}

struct RuleLogWorker {
    sender: Mutex<Option<SyncSender<RuleLogRecord>>>,
    shutdown_handles: Mutex<Option<ShutdownHandles>>,
    counters: Arc<RuleLogCounters>,
    shutdown_drain: Duration,
}

impl RuleLogWorker {
    fn spawn<S: RuleLogSink>(sink: S, config: WorkerConfig) -> io::Result<Arc<Self>> {
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity.max(1));
        let (completed_tx, completed) = mpsc::channel();
        let counters = Arc::new(RuleLogCounters::default());
        counters.worker_running.store(true, Ordering::Release);
        let worker_counters = Arc::clone(&counters);
        let flush_batch_size = config.flush_batch_size.max(1);
        let flush_interval = config.flush_interval.max(Duration::from_millis(1));
        let thread_result = std::thread::Builder::new()
            .name("aether-rule-log".to_string())
            .spawn(move || {
                let _running = WorkerRunningGuard(Arc::clone(&worker_counters));
                let final_flush_failed = run_log_worker(
                    receiver,
                    sink,
                    Arc::clone(&worker_counters),
                    flush_batch_size,
                    flush_interval,
                );
                let _ = completed_tx.send(WorkerCompletion { final_flush_failed });
            });
        let thread = match thread_result {
            Ok(thread) => thread,
            Err(error) => {
                counters.worker_running.store(false, Ordering::Release);
                return Err(error);
            },
        };
        Ok(Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            shutdown_handles: Mutex::new(Some(ShutdownHandles { completed, thread })),
            counters,
            shutdown_drain: config.shutdown_drain,
        }))
    }

    fn spawn_default_or_unavailable() -> Arc<Self> {
        match Self::spawn(RuleFileLogSink::default(), WorkerConfig::default()) {
            Ok(worker) => worker,
            Err(error) => {
                warn!(%error, "Cannot start rule-log disk worker; logging disabled");
                Self::unavailable()
            },
        }
    }

    fn unavailable() -> Arc<Self> {
        let counters = Arc::new(RuleLogCounters::default());
        counters.worker_start_failures.store(1, Ordering::Relaxed);
        Arc::new(Self {
            sender: Mutex::new(None),
            shutdown_handles: Mutex::new(None),
            counters,
            shutdown_drain: DEFAULT_SHUTDOWN_DRAIN,
        })
    }

    fn enqueue(&self, record: RuleLogRecord) {
        let sender = lock_recover(&self.sender, "rule-log sender");
        let Some(sender) = sender.as_ref() else {
            self.record_drop("worker is shutting down or unavailable");
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
            warn!(
                oversized,
                bytes,
                max_record_bytes = MAX_RECORD_BYTES,
                "Oversized rule-log record dropped"
            );
        }
    }

    fn record_drop(&self, reason: &'static str) {
        let dropped = self.counters.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if dropped.is_power_of_two() {
            warn!(
                dropped,
                pending = self.counters.pending.load(Ordering::Relaxed),
                reason,
                "Rule log record dropped"
            );
        }
    }

    fn stats(&self) -> RuleLogStats {
        self.counters.snapshot()
    }

    fn shutdown_blocking(&self) -> io::Result<()> {
        lock_recover(&self.sender, "rule-log sender").take();
        let mut handles = lock_recover(&self.shutdown_handles, "rule-log shutdown handles");
        let Some(ShutdownHandles { completed, thread }) = handles.take() else {
            return if self.counters.worker_running.load(Ordering::Acquire) {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "rule-log worker shutdown is still in progress",
                ))
            } else {
                Ok(())
            };
        };

        let completion = match completed.recv_timeout(self.shutdown_drain) {
            Ok(completion) => completion,
            Err(mpsc::RecvTimeoutError::Disconnected) => WorkerCompletion {
                final_flush_failed: false,
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.counters
                    .shutdown_timeouts
                    .fetch_add(1, Ordering::Relaxed);
                drop(thread);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "rule-log worker did not drain {} pending records within {} ms",
                        self.counters.pending.load(Ordering::Relaxed),
                        self.shutdown_drain.as_millis()
                    ),
                ));
            },
        };
        thread
            .join()
            .map_err(|_| io::Error::other("rule-log worker panicked during shutdown"))?;
        if completion.final_flush_failed {
            Err(io::Error::other(
                "rule-log worker drained its queue but the final disk flush failed",
            ))
        } else {
            Ok(())
        }
    }

    fn shutdown_in_reaper(&self) {
        lock_recover(&self.sender, "rule-log sender").take();
        let Some(ShutdownHandles { completed, thread }) =
            lock_recover(&self.shutdown_handles, "rule-log shutdown handles").take()
        else {
            return;
        };
        let counters = Arc::clone(&self.counters);
        let shutdown_drain = self.shutdown_drain;
        if let Err(error) = std::thread::Builder::new()
            .name("aether-rule-log-reaper".to_string())
            .spawn(move || match completed.recv_timeout(shutdown_drain) {
                Ok(completion) => {
                    if thread.join().is_err() {
                        warn!("Rule-log worker panicked during shutdown");
                    } else if completion.final_flush_failed {
                        warn!("Rule-log worker final disk flush failed during shutdown");
                    }
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if thread.join().is_err() {
                        warn!("Rule-log worker panicked during shutdown");
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    counters.shutdown_timeouts.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        pending = counters.pending.load(Ordering::Relaxed),
                        drain_timeout_ms = shutdown_drain.as_millis(),
                        "Rule-log shutdown drain timed out; disk worker detached"
                    );
                    drop(thread);
                },
            })
        {
            warn!(%error, "Cannot start rule-log shutdown reaper; worker detached");
        }
    }
}

impl Drop for RuleLogWorker {
    fn drop(&mut self) {
        self.shutdown_in_reaper();
    }
}

struct WorkerRunningGuard(Arc<RuleLogCounters>);

impl Drop for WorkerRunningGuard {
    fn drop(&mut self) {
        let pending = self.0.pending.swap(0, Ordering::AcqRel);
        if pending > 0 {
            self.0.dropped.fetch_add(pending, Ordering::Relaxed);
            warn!(pending, "Rule-log worker exited with records undelivered");
        }
        self.0.worker_running.store(false, Ordering::Release);
    }
}

fn lock_recover<'a, T>(mutex: &'a Mutex<T>, name: &str) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        warn!(name, "Recovering poisoned rule-log mutex");
        poisoned.into_inner()
    })
}

fn flush_sink<S: RuleLogSink>(sink: &mut S, counters: &RuleLogCounters) -> bool {
    if let Err(error) = sink.flush() {
        let failures = counters.flush_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures.is_power_of_two() {
            warn!(failures, %error, "Rule-log batch flush failed");
        }
        false
    } else {
        true
    }
}

fn run_log_worker<S: RuleLogSink>(
    receiver: Receiver<RuleLogRecord>,
    mut sink: S,
    counters: Arc<RuleLogCounters>,
    flush_batch_size: usize,
    flush_interval: Duration,
) -> bool {
    let mut dirty = 0_usize;
    let mut next_flush = Instant::now() + flush_interval;
    loop {
        let timeout = next_flush.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(record) => {
                if let Err(error) = sink.write(&record) {
                    let failures = counters.write_failures.fetch_add(1, Ordering::Relaxed) + 1;
                    if failures.is_power_of_two() {
                        warn!(
                            failures,
                            rule_id = record.rule_id,
                            %error,
                            "Rule-log write failed"
                        );
                    }
                } else {
                    dirty = dirty.saturating_add(1);
                }
                let rotation_flush_failures = sink.take_rotation_flush_failures();
                if rotation_flush_failures > 0 {
                    counters
                        .flush_failures
                        .fetch_add(rotation_flush_failures, Ordering::Relaxed);
                }
                counters.pending.fetch_sub(1, Ordering::Relaxed);
                if dirty >= flush_batch_size {
                    if flush_sink(&mut sink, &counters) {
                        dirty = 0;
                    }
                    next_flush = Instant::now() + flush_interval;
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty > 0 && flush_sink(&mut sink, &counters) {
                    dirty = 0;
                }
                next_flush = Instant::now() + flush_interval;
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    dirty > 0 && !flush_sink(&mut sink, &counters)
}

struct BoundedLine {
    value: String,
    limit: usize,
    attempted_bytes: usize,
    overflowed: bool,
}

impl BoundedLine {
    fn new(limit: usize) -> Self {
        Self {
            value: String::with_capacity(limit.min(1_024)),
            limit,
            attempted_bytes: 0,
            overflowed: false,
        }
    }

    fn into_result(self) -> Result<String, usize> {
        if self.overflowed {
            Err(self.attempted_bytes.max(self.limit.saturating_add(1)))
        } else {
            Ok(self.value)
        }
    }
}

impl FmtWrite for BoundedLine {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.attempted_bytes = self.attempted_bytes.saturating_add(value.len());
        let remaining = self.limit.saturating_sub(self.value.len());
        if value.len() <= remaining {
            self.value.push_str(value);
            return Ok(());
        }

        let mut end = remaining.min(value.len());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        self.value.push_str(&value[..end]);
        self.overflowed = true;
        Ok(())
    }
}

/// Logger for individual rule execution.
pub struct RuleLogger {
    rule_id: String,
    log_dir: PathBuf,
    worker: Arc<RuleLogWorker>,
}

impl RuleLogger {
    /// Create a new RuleLogger for a specific rule
    ///
    /// Log files will be created in: `{log_root}/rules/{rule_id}/`
    /// with naming format: `{YYYYMMDD}_{rule_name}.log`
    pub fn new(log_root: &Path, rule_id: i64, _rule_name: &str) -> Self {
        Self::with_worker(
            log_root,
            rule_id,
            RuleLogWorker::spawn_default_or_unavailable(),
        )
    }

    fn with_worker(log_root: &Path, rule_id: i64, worker: Arc<RuleLogWorker>) -> Self {
        let rule_id_str = rule_id.to_string();
        Self {
            log_dir: log_root.join("rules").join(&rule_id_str),
            rule_id: rule_id_str,
            worker,
        }
    }

    /// Log rule execution with matched condition only
    ///
    /// Format: `timestamp [RULE] rule_id vars | matched_condition | action_result`
    pub fn log_execution(&self, result: &RuleExecutionResult, vars: &HashMap<String, f64>) {
        let now = Utc::now();
        let mut line = BoundedLine::new(MAX_RECORD_BYTES.saturating_sub(1));
        let _ = write!(
            line,
            "{} [RULE] {} ",
            now.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            self.rule_id
        );
        write_variables_bounded(&mut line, vars);
        if !line.overflowed {
            let _ = write!(
                line,
                " | {} | ",
                result.matched_condition.as_deref().unwrap_or("-")
            );
        }
        if !line.overflowed {
            write_actions_bounded(&mut line, &result.actions_executed, result.error.as_deref());
        }

        let line = match line.into_result() {
            Ok(line) => line,
            Err(bytes) => {
                self.worker.reject_oversized(bytes);
                return;
            },
        };
        self.worker.enqueue(RuleLogRecord {
            rule_id: self.rule_id.clone(),
            log_dir: self.log_dir.clone(),
            date: Local::now().date_naive(),
            line,
        });
    }
}

fn write_variables_bounded(line: &mut BoundedLine, vars: &HashMap<String, f64>) {
    if vars.is_empty() {
        let _ = line.write_str("-");
        return;
    }
    for (index, (name, value)) in vars.iter().enumerate() {
        if index > 0 {
            let _ = line.write_char(' ');
        }
        let _ = write!(line, "{}={:.1}", name, value);
        if line.overflowed {
            return;
        }
    }
}

fn write_actions_bounded(line: &mut BoundedLine, actions: &[ActionResult], error: Option<&str>) {
    if let Some(error) = error {
        let _ = line.write_str(error);
        return;
    }
    if actions.is_empty() {
        let _ = line.write_str("no action");
        return;
    }
    for (index, action) in actions.iter().enumerate() {
        if index > 0 {
            let _ = line.write_str(", ");
        }
        let status = if action.success { "OK" } else { "FAIL" };
        let _ = write!(
            line,
            "{}:{}:{}={} {}",
            action.target_id, action.point_type, action.point_id, action.value, status
        );
        if line.overflowed {
            return;
        }
    }
}

/// Format action results for logging
/// Optimization: use fmt::Write to avoid intermediate Vec allocation
#[cfg(test)]
fn format_actions(actions: &[ActionResult], error: Option<&str>) -> String {
    if let Some(err) = error {
        return err.to_string();
    }

    if actions.is_empty() {
        return "no action".to_string();
    }

    // Pre-allocate: ~40 chars per action (instance:type:point=value status)
    let mut result = String::with_capacity(actions.len() * 40);
    for (i, a) in actions.iter().enumerate() {
        if i > 0 {
            result.push_str(", ");
        }
        let status = if a.success { "OK" } else { "FAIL" };
        // Format: "instance_id:point_type:point_id=value OK"
        let _ = write!(
            result,
            "{}:{}:{}={} {}",
            a.target_id, a.point_type, a.point_id, a.value, status
        );
    }
    result
}

/// Format conditions as expression string (e.g., "X1>=49" or "X1>10 && X2<50")
pub fn format_conditions(conditions: &[FlowCondition]) -> String {
    if conditions.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    let mut pending_relation: Option<&str> = None;

    for cond in conditions {
        if cond.cond_type == "relation" {
            pending_relation = cond.value.as_ref().and_then(|v| v.as_str());
            continue;
        }

        // Format: "X1>=49"
        if let Some(var) = &cond.variables {
            let op = cond.operator.as_deref().unwrap_or("==");
            let val = cond
                .value
                .as_ref()
                .map(|v| {
                    // Remove quotes from string values
                    let s = v.to_string();
                    s.trim_matches('"').to_string()
                })
                .unwrap_or_default();

            let expr = format!("{}{}{}", var, op, val);

            // Add relation if pending
            if let Some(rel) = pending_relation.take() {
                let rel_str = match rel {
                    "||" | "or" | "OR" => " || ",
                    _ => " && ",
                };
                parts.push(rel_str.to_string());
            }
            parts.push(expr);
        }
    }

    parts.concat()
}

/// Manager for multiple rule loggers
pub struct RuleLoggerManager {
    log_root: PathBuf,
    loggers: Mutex<HashMap<String, Arc<RuleLogger>>>,
    worker: Arc<RuleLogWorker>,
}

impl RuleLoggerManager {
    /// Create a new logger manager
    pub fn new(log_root: PathBuf) -> Self {
        Self {
            log_root,
            loggers: Mutex::new(HashMap::new()),
            worker: RuleLogWorker::spawn_default_or_unavailable(),
        }
    }

    /// Get or create a logger for a specific rule
    pub fn get_logger(&self, rule_id: i64, rule_name: &str) -> Arc<RuleLogger> {
        let rule_id_str = rule_id.to_string();
        let mut loggers = lock_recover(&self.loggers, "rule logger registry");

        if let Some(logger) = loggers.get(&rule_id_str) {
            return Arc::clone(logger);
        }

        let _ = rule_name;
        let logger = Arc::new(RuleLogger::with_worker(
            &self.log_root,
            rule_id,
            Arc::clone(&self.worker),
        ));
        loggers.insert(rule_id_str, Arc::clone(&logger));
        logger
    }

    /// Returns cumulative admission and disk-worker counters.
    #[must_use]
    pub fn stats(&self) -> RuleLogStats {
        self.worker.stats()
    }

    /// Stops admission and drains already-admitted records up to the worker deadline.
    ///
    /// This is a blocking operation. Async callers must invoke it through a
    /// blocking executor rather than on a Tokio worker thread. The method is
    /// idempotent after a completed shutdown.
    pub fn shutdown_blocking(&self) -> io::Result<()> {
        self.worker.shutdown_blocking()
    }
}

impl Drop for RuleLoggerManager {
    fn drop(&mut self) {
        self.worker.shutdown_in_reaper();
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use tempfile::TempDir;

    struct BlockingSink {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        writes: Arc<AtomicU64>,
    }

    impl RuleLogSink for BlockingSink {
        fn write(&mut self, _record: &RuleLogRecord) -> io::Result<()> {
            let _ = self.entered.send(());
            self.release
                .recv()
                .map_err(|_| io::Error::other("test writer release channel closed"))?;
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct RecordingSink {
        lines: Arc<Mutex<Vec<String>>>,
        flushes: Arc<AtomicU64>,
    }

    impl RuleLogSink for RecordingSink {
        fn write(&mut self, record: &RuleLogRecord) -> io::Result<()> {
            lock_recover(&self.lines, "test recorded lines").push(record.line.clone());
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FailingWriteSink;

    impl RuleLogSink for FailingWriteSink {
        fn write(&mut self, _record: &RuleLogRecord) -> io::Result<()> {
            Err(io::Error::other("synthetic rule-log write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct RetryingFlushSink {
        flushes: Arc<AtomicUsize>,
        failures_before_success: usize,
    }

    impl RuleLogSink for RetryingFlushSink {
        fn write(&mut self, _record: &RuleLogRecord) -> io::Result<()> {
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            let attempt = self.flushes.fetch_add(1, Ordering::SeqCst);
            if attempt < self.failures_before_success {
                Err(io::Error::other("synthetic rule-log flush failure"))
            } else {
                Ok(())
            }
        }
    }

    fn worker_config(queue_capacity: usize) -> WorkerConfig {
        WorkerConfig {
            queue_capacity,
            flush_batch_size: 64,
            flush_interval: Duration::from_secs(10),
            shutdown_drain: Duration::from_secs(1),
        }
    }

    fn execution_result(error: Option<String>) -> RuleExecutionResult {
        RuleExecutionResult {
            rule_id: 7,
            success: error.is_none(),
            actions_executed: Vec::new(),
            error,
            execution_path: Vec::new(),
            matched_condition: Some("X1>=49".to_string()),
            variable_values: Arc::new(HashMap::new()),
            point_values: Arc::new(HashMap::new()),
            node_details: HashMap::new(),
        }
    }

    fn test_record(index: usize) -> RuleLogRecord {
        RuleLogRecord {
            rule_id: "7".to_string(),
            log_dir: PathBuf::from("/test/rules/7"),
            date: Local::now().date_naive(),
            line: format!("record {index}"),
        }
    }

    fn wait_until(mut predicate: impl FnMut() -> bool, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn slow_disk_does_not_block_admission_and_full_queue_is_observable() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writes = Arc::new(AtomicU64::new(0));
        let worker = RuleLogWorker::spawn(
            BlockingSink {
                entered: entered_tx,
                release: release_rx,
                writes: Arc::clone(&writes),
            },
            worker_config(1),
        )
        .expect("test rule-log worker");
        let logger = RuleLogger::with_worker(Path::new("/test"), 7, Arc::clone(&worker));
        let result = execution_result(None);
        let variables = HashMap::new();

        logger.log_execution(&result, &variables);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("disk worker entered first write");
        let started = Instant::now();
        logger.log_execution(&result, &variables);
        logger.log_execution(&result, &variables);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "rule execution logging waited on the blocked disk worker"
        );

        let stats = worker.stats();
        assert_eq!(stats.accepted, 2);
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.pending, 2);
        assert!(stats.worker_running);

        release_tx.send(()).expect("release first write");
        release_tx.send(()).expect("release queued write");
        worker.shutdown_blocking().expect("worker drains");
        assert_eq!(writes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn oversized_error_is_stopped_by_bounded_formatter_before_admission() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let worker = RuleLogWorker::spawn(
            RecordingSink {
                lines: Arc::clone(&lines),
                flushes: Arc::new(AtomicU64::new(0)),
            },
            worker_config(1),
        )
        .expect("test rule-log worker");
        let logger = RuleLogger::with_worker(Path::new("/test"), 7, Arc::clone(&worker));

        logger.log_execution(
            &execution_result(Some("x".repeat(MAX_RECORD_BYTES))),
            &HashMap::new(),
        );

        let stats = worker.stats();
        assert_eq!(stats.accepted, 0);
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.oversized, 1);
        worker.shutdown_blocking().expect("empty worker stops");
        assert!(lock_recover(&lines, "test lines").is_empty());
    }

    #[test]
    fn write_failure_is_counted_and_does_not_leave_pending_work() {
        let worker =
            RuleLogWorker::spawn(FailingWriteSink, worker_config(1)).expect("test rule-log worker");
        worker.enqueue(test_record(1));
        wait_until(
            || worker.stats().write_failures == 1,
            "rule-log write failure",
        );

        let stats = worker.stats();
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.write_failures, 1);
        assert_eq!(stats.pending, 0);
        worker
            .shutdown_blocking()
            .expect("failed write was consumed");
    }

    #[test]
    fn failed_periodic_flush_is_retried_until_it_succeeds() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let worker = RuleLogWorker::spawn(
            RetryingFlushSink {
                flushes: Arc::clone(&flushes),
                failures_before_success: 1,
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 1,
                flush_interval: Duration::from_millis(10),
                shutdown_drain: Duration::from_secs(1),
            },
        )
        .expect("test rule-log worker");
        worker.enqueue(test_record(1));
        wait_until(
            || flushes.load(Ordering::SeqCst) >= 2,
            "rule-log flush retry",
        );

        assert_eq!(worker.stats().flush_failures, 1);
        worker.shutdown_blocking().expect("retry succeeded");
    }

    #[test]
    fn final_flush_failure_is_returned_by_explicit_shutdown() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let worker = RuleLogWorker::spawn(
            RetryingFlushSink {
                flushes: Arc::clone(&flushes),
                failures_before_success: usize::MAX,
            },
            worker_config(1),
        )
        .expect("test rule-log worker");
        worker.enqueue(test_record(1));

        let error = worker
            .shutdown_blocking()
            .expect_err("final flush failure must reach shutdown caller");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(worker.stats().flush_failures, 1);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert!(!worker.stats().worker_running);
    }

    #[test]
    fn explicit_shutdown_drains_records_and_remains_idempotent() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let flushes = Arc::new(AtomicU64::new(0));
        let worker = RuleLogWorker::spawn(
            RecordingSink {
                lines: Arc::clone(&lines),
                flushes: Arc::clone(&flushes),
            },
            worker_config(4),
        )
        .expect("test rule-log worker");
        for index in 0..3 {
            worker.enqueue(test_record(index));
        }

        worker
            .shutdown_blocking()
            .expect("worker drains on shutdown");
        worker
            .shutdown_blocking()
            .expect("completed shutdown is idempotent");

        let stats = worker.stats();
        assert_eq!(stats.accepted, 3);
        assert_eq!(stats.dropped, 0);
        assert_eq!(stats.pending, 0);
        assert!(!stats.worker_running);
        assert_eq!(lock_recover(&lines, "test lines").len(), 3);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn explicit_shutdown_timeout_keeps_pending_state_observable() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = RuleLogWorker::spawn(
            BlockingSink {
                entered: entered_tx,
                release: release_rx,
                writes: Arc::new(AtomicU64::new(0)),
            },
            WorkerConfig {
                queue_capacity: 1,
                flush_batch_size: 1,
                flush_interval: Duration::from_secs(1),
                shutdown_drain: Duration::from_millis(10),
            },
        )
        .expect("test rule-log worker");
        worker.enqueue(test_record(1));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("disk worker entered blocking write");

        let error = worker
            .shutdown_blocking()
            .expect_err("bounded shutdown reports slow disk");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let stats = worker.stats();
        assert_eq!(stats.shutdown_timeouts, 1);
        assert_eq!(stats.pending, 1);
        assert!(stats.worker_running);

        release_tx.send(()).expect("release disk worker");
        wait_until(
            || !worker.stats().worker_running,
            "detached rule-log worker exit",
        );
        assert_eq!(worker.stats().pending, 0);
    }

    #[test]
    fn manager_worker_writes_the_existing_daily_file_contract() {
        let temp = TempDir::new().expect("temporary rule-log root");
        let manager = RuleLoggerManager::new(temp.path().to_path_buf());
        let logger = manager.get_logger(7, "ignored-compatible-name");
        logger.log_execution(&execution_result(None), &HashMap::new());
        manager
            .shutdown_blocking()
            .expect("manager drains log worker");

        let path = temp.path().join("rules").join("7").join(format!(
            "{}_7.log",
            Local::now().date_naive().format("%Y%m%d")
        ));
        let content = fs::read_to_string(path).expect("daily rule log exists");
        assert!(content.contains("[RULE] 7 - | X1>=49 | no action"));
        assert_eq!(manager.stats().pending, 0);
        assert!(!manager.stats().worker_running);
    }

    #[test]
    fn test_format_conditions_simple() {
        let conditions = vec![FlowCondition {
            cond_type: "variable".to_string(),
            variables: Some("X1".to_string()),
            operator: Some(">=".to_string()),
            value: Some(serde_json::json!(49)),
        }];

        assert_eq!(format_conditions(&conditions), "X1>=49");
    }

    #[test]
    fn test_format_conditions_compound_and() {
        let conditions = vec![
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some(">".to_string()),
                value: Some(serde_json::json!(10)),
            },
            FlowCondition {
                cond_type: "relation".to_string(),
                variables: None,
                operator: None,
                value: Some(serde_json::json!("&&")),
            },
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X2".to_string()),
                operator: Some("<".to_string()),
                value: Some(serde_json::json!(50)),
            },
        ];

        assert_eq!(format_conditions(&conditions), "X1>10 && X2<50");
    }

    #[test]
    fn test_format_conditions_compound_or() {
        let conditions = vec![
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some("<=".to_string()),
                value: Some(serde_json::json!(5)),
            },
            FlowCondition {
                cond_type: "relation".to_string(),
                variables: None,
                operator: None,
                value: Some(serde_json::json!("||")),
            },
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some(">=".to_string()),
                value: Some(serde_json::json!(95)),
            },
        ];

        assert_eq!(format_conditions(&conditions), "X1<=5 || X1>=95");
    }

    #[test]
    fn test_format_conditions_empty() {
        let conditions: Vec<FlowCondition> = vec![];
        assert_eq!(format_conditions(&conditions), "");
    }

    #[test]
    fn test_format_actions_success() {
        let actions = vec![ActionResult {
            target_type: "instance",
            target_id: 5,
            point_type: "A",
            point_id: 2,
            value: 1.0,
            success: true,
            delivery_possible: false,
        }];

        assert_eq!(format_actions(&actions, None), "5:A:2=1 OK");
    }

    #[test]
    fn test_format_actions_failure() {
        let actions = vec![ActionResult {
            target_type: "instance",
            target_id: 5,
            point_type: "A",
            point_id: 2,
            value: 1.0,
            success: false,
            delivery_possible: false,
        }];

        assert_eq!(format_actions(&actions, None), "5:A:2=1 FAIL");
    }

    #[test]
    fn test_format_actions_with_error() {
        let actions = vec![];
        assert_eq!(format_actions(&actions, Some("read failed")), "read failed");
    }

    #[test]
    fn test_format_actions_empty() {
        let actions: Vec<ActionResult> = vec![];
        assert_eq!(format_actions(&actions, None), "no action");
    }
}
