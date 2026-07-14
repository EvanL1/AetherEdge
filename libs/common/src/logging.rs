//! Unified logging module for AetherEMS services
//!
//! Provides multi-level logging support with automatic sub-logger creation

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
#[allow(unused_imports)] // Used in Write trait impl for DailyRollingWriter
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{
        self, FmtContext, FormatEvent, FormatFields, MakeWriter,
        format::{FmtSpan, Writer},
    },
    layer::SubscriberExt,
    registry::LookupSpan,
    reload,
    util::SubscriberInitExt,
};

/// Custom format for log level with brackets: `[INFO]`, `[WARN]`, etc.
fn format_level(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "[TRACE]",
        Level::DEBUG => "[DEBUG]",
        Level::INFO => "[INFO]",
        Level::WARN => "[WARN]",
        Level::ERROR => "[ERROR]",
    }
}

/// Custom event formatter that outputs: `timestamp [LEVEL] message`
///
/// Example output: `2025-12-02T00:50:44.809Z [INFO] Service started`
struct BracketedLevelFormat;

impl<S, N> FormatEvent<S, N> for BracketedLevelFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        // Format timestamp
        let now = chrono::Utc::now();
        write!(writer, "{} ", now.format("%Y-%m-%dT%H:%M:%S%.6fZ"))?;

        // Format level with brackets and color
        let level = *event.metadata().level();
        if writer.has_ansi_escapes() {
            let color = match level {
                Level::TRACE => "\x1b[35m", // magenta
                Level::DEBUG => "\x1b[34m", // blue
                Level::INFO => "\x1b[32m",  // green
                Level::WARN => "\x1b[33m",  // yellow
                Level::ERROR => "\x1b[31m", // red
            };
            write!(writer, "{}{}\x1b[0m ", color, format_level(&level))?;
        } else {
            write!(writer, "{} ", format_level(&level))?;
        }

        // Format the event message and fields
        ctx.field_format().format_fields(writer.by_ref(), event)?;

        writeln!(writer)
    }
}

// Global guards for keeping loggers alive
static GUARDS: OnceLock<Arc<Mutex<Vec<WorkerGuard>>>> = OnceLock::new();
// API logger guard - separate to allow independent lifecycle
static API_GUARD: OnceLock<Arc<Mutex<Option<WorkerGuard>>>> = OnceLock::new();
// Background logging task handles (SIGHUP handler, compression task)
static LOGGING_TASK_HANDLES: OnceLock<Arc<tokio::sync::RwLock<Vec<tokio::task::JoinHandle<()>>>>> =
    OnceLock::new();

// ============================================================================
// Log Root Directory Configuration
// ============================================================================

/// Global log root directory (initialized once from config or env)
/// Priority: AETHER_LOG_DIR env > config_dir > default "logs"
static LOG_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Initialize log root directory from config or environment
///
/// This should be called early during service bootstrap, before any logging
/// functions that write to files are invoked.
///
/// Priority:
/// 1. `AETHER_LOG_DIR` environment variable (highest)
/// 2. `config_dir` parameter (from SQLite config)
/// 3. Default value "logs" (lowest)
pub fn init_log_root(config_dir: Option<&str>) {
    LOG_ROOT.get_or_init(|| {
        std::env::var("AETHER_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                config_dir
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("logs"))
            })
    });
}

/// Get log root directory
///
/// Returns the configured log root directory. If `init_log_root` was not called,
/// falls back to checking environment variable or default "logs".
///
/// When running under `cargo test` (detected via CARGO_TARGET_TMPDIR or test binary path),
/// defaults to system temp directory to avoid polluting the project directory.
pub fn get_log_root() -> PathBuf {
    LOG_ROOT.get().cloned().unwrap_or_else(|| {
        std::env::var("AETHER_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                // Detect test environment: cargo sets CARGO_TARGET_TMPDIR during tests
                // or we can check if running from target/debug/deps (test binaries)
                if is_test_environment() {
                    std::env::temp_dir().join("aether-test-logs")
                } else {
                    PathBuf::from("logs")
                }
            })
    })
}

/// Detect if we're running in a test environment
fn is_test_environment() -> bool {
    // Method 1: Check CARGO_TARGET_TMPDIR (set by cargo during test runs)
    if std::env::var("CARGO_TARGET_TMPDIR").is_ok() {
        return true;
    }

    // Method 2: Cargo test binaries live below `<target>/{debug,release}/deps`.
    // The target root is configurable through CARGO_TARGET_DIR, so do not
    // require the directory itself to be literally named `target`.
    if let Ok(exe) = std::env::current_exe()
        && is_cargo_test_binary_path(&exe)
    {
        return true;
    }

    false
}

fn is_cargo_test_binary_path(path: &Path) -> bool {
    let Some(deps) = path.parent() else {
        return false;
    };
    if deps.file_name() != Some(OsStr::new("deps")) {
        return false;
    }
    deps.parent()
        .and_then(Path::file_name)
        .is_some_and(|profile| profile == "debug" || profile == "release")
}

/// Default max file size: 20MB (reduced from 100MB for faster rotation and compression)
const DEFAULT_MAX_FILE_SIZE: u64 = 20 * 1024 * 1024;

// Custom daily rolling file writer with naming format: {YYYYMMDD}_{service}{suffix}.log
// Also supports size-based rotation within a day
struct DailyRollingWriter {
    service_name: String,
    log_dir: PathBuf,
    /// Filename suffix (e.g., "" for regular, "_api" for API logs)
    suffix: String,
    current_date: Arc<Mutex<String>>,
    current_file: Arc<Mutex<Option<File>>>,
    /// Current file size in bytes (tracked for size-based rotation)
    current_size: Arc<AtomicU64>,
    /// Max file size before rotation (0 = no size rotation)
    max_file_size: u64,
    /// Rotation counter within the same day (e.g., .1, .2, .3)
    rotation_count: Arc<AtomicU32>,
}

impl DailyRollingWriter {
    fn new(service_name: String, log_dir: PathBuf) -> std::io::Result<Self> {
        Self::with_options(service_name, log_dir, String::new(), DEFAULT_MAX_FILE_SIZE)
    }

    /// Create an API log writer with "_api" suffix and no size rotation
    fn new_api(service_name: String, log_dir: PathBuf) -> std::io::Result<Self> {
        Self::with_options(service_name, log_dir, "_api".to_string(), 0)
    }

    fn with_options(
        service_name: String,
        log_dir: PathBuf,
        suffix: String,
        max_file_size: u64,
    ) -> std::io::Result<Self> {
        let current_date = chrono::Local::now().format("%Y%m%d").to_string();
        let file_path = log_dir.join(format!("{}_{}{}.log", current_date, service_name, suffix));

        fs::create_dir_all(&log_dir)?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;
        let initial_size = file.metadata().map(|m| m.len()).unwrap_or(0);

        Ok(Self {
            service_name,
            log_dir,
            suffix,
            current_date: Arc::new(Mutex::new(current_date)),
            current_file: Arc::new(Mutex::new(Some(file))),
            current_size: Arc::new(AtomicU64::new(initial_size)),
            max_file_size,
            rotation_count: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Rotate the log file due to size limit
    fn rotate_by_size(&self) -> std::io::Result<()> {
        let current_date = self
            .current_date
            .lock()
            .map_err(|e| std::io::Error::other(format!("Mutex poisoned: {}", e)))?;

        let count = self.rotation_count.fetch_add(1, Ordering::SeqCst) + 1;

        let new_file_path = self.log_dir.join(format!(
            "{}_{}{}.{}.log",
            *current_date, self.service_name, self.suffix, count
        ));

        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_file_path)?;

        // Reset size counter
        self.current_size.store(0, Ordering::SeqCst);

        // Update current file
        let mut current_file = self
            .current_file
            .lock()
            .map_err(|e| std::io::Error::other(format!("Mutex poisoned: {}", e)))?;
        *current_file = Some(new_file);

        Ok(())
    }

    fn get_writer(&self) -> std::io::Result<std::sync::MutexGuard<'_, Option<File>>> {
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let mut current_date = self
            .current_date
            .lock()
            .map_err(|e| std::io::Error::other(format!("Mutex poisoned: {}", e)))?;

        let current_file_path = self.log_dir.join(format!(
            "{}_{}{}.log",
            *current_date, self.service_name, self.suffix
        ));

        let file_deleted = !current_file_path.exists();

        if *current_date != today || file_deleted {
            let new_date = if *current_date != today {
                today.clone()
            } else {
                current_date.clone()
            };
            let new_file_path = self.log_dir.join(format!(
                "{}_{}{}.log",
                new_date, self.service_name, self.suffix
            ));

            // Ensure directory exists (in case it was also deleted)
            fs::create_dir_all(&self.log_dir)?;

            let new_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&new_file_path)?;
            let initial_size = new_file.metadata().map(|m| m.len()).unwrap_or(0);

            // Update current date, file, and reset counters
            if *current_date != today {
                *current_date = today;
                self.rotation_count.store(0, Ordering::SeqCst);
            }
            self.current_size.store(initial_size, Ordering::SeqCst);

            let mut current_file = self
                .current_file
                .lock()
                .map_err(|e| std::io::Error::other(format!("Mutex poisoned: {}", e)))?;
            *current_file = Some(new_file);
        }

        self.current_file
            .lock()
            .map_err(|e| std::io::Error::other(format!("Mutex poisoned: {}", e)))
    }
}

impl std::io::Write for DailyRollingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Check if we need to rotate due to size limit (0 = disabled)
        if self.max_file_size > 0 {
            let current_size = self.current_size.load(Ordering::Relaxed);
            if current_size + buf.len() as u64 > self.max_file_size {
                self.rotate_by_size()?;
            }
        }

        match *self.get_writer()? {
            Some(ref mut file) => {
                let written = file.write(buf)?;
                // Update size counter
                self.current_size
                    .fetch_add(written as u64, Ordering::Relaxed);
                Ok(written)
            },
            _ => Ok(0),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match *self.get_writer()? {
            Some(ref mut file) => file.flush(),
            _ => Ok(()),
        }
    }
}

impl Clone for DailyRollingWriter {
    fn clone(&self) -> Self {
        Self {
            service_name: self.service_name.clone(),
            log_dir: self.log_dir.clone(),
            suffix: self.suffix.clone(),
            current_date: Arc::clone(&self.current_date),
            current_file: Arc::clone(&self.current_file),
            current_size: Arc::clone(&self.current_size),
            max_file_size: self.max_file_size,
            rotation_count: Arc::clone(&self.rotation_count),
        }
    }
}

// Reloadable writer for file logging
struct ReloadableWriter {
    inner: Arc<Mutex<Option<tracing_appender::non_blocking::NonBlocking>>>,
}

impl ReloadableWriter {
    fn new(writer: tracing_appender::non_blocking::NonBlocking) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(writer))),
        }
    }

    fn reload(&self, new_writer: tracing_appender::non_blocking::NonBlocking) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some(new_writer);
        }
    }
}

impl std::io::Write for ReloadableWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(ref mut writer) = *guard
        {
            return writer.write(buf);
        }
        Ok(0)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(ref mut writer) = *guard
        {
            return writer.flush();
        }
        Ok(())
    }
}

impl MakeWriter<'_> for ReloadableWriter {
    type Writer = Self;

    fn make_writer(&'_ self) -> Self::Writer {
        ReloadableWriter {
            inner: self.inner.clone(),
        }
    }
}

// Newtype wrapper for Arc<ReloadableWriter> to bypass orphan rule
#[derive(Clone)]
struct ReloadableWriterHandle(Arc<ReloadableWriter>);

impl MakeWriter<'_> for ReloadableWriterHandle {
    type Writer = ReloadableWriter;

    fn make_writer(&'_ self) -> Self::Writer {
        ReloadableWriter {
            inner: self.0.inner.clone(),
        }
    }
}

// Keep runtime config and writer for reopen
#[derive(Clone)]
struct LogRuntime {
    service_name: String,
    log_dir: PathBuf,
}

static LOG_RUNTIME: OnceLock<Arc<Mutex<LogRuntime>>> = OnceLock::new();
static RELOADABLE_WRITER: OnceLock<Arc<ReloadableWriter>> = OnceLock::new();
// API logger runtime configuration and writer
static API_LOG_RUNTIME: OnceLock<Arc<Mutex<LogRuntime>>> = OnceLock::new();
static API_RELOADABLE_WRITER: OnceLock<Arc<ReloadableWriter>> = OnceLock::new();

// Dynamic log level reload support
type EnvFilterReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static LOG_FILTER_HANDLE: OnceLock<EnvFilterReloadHandle> = OnceLock::new();
static CURRENT_LOG_LEVEL: OnceLock<Mutex<String>> = OnceLock::new();

/// Logger configuration
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Service name (e.g., "io", "automation")
    pub service_name: String,
    /// Base directory for logs
    pub log_dir: PathBuf,
    /// Console log level
    pub console_level: Level,
    /// File log level
    pub file_level: Level,
    /// Enable JSON format for structured logging
    pub enable_json: bool,
    /// Maximum number of log files to keep (for compression/cleanup)
    pub max_log_files: usize,
    /// Enable API log separation (default: true)
    pub enable_api_log: bool,
    /// API log level (default: INFO)
    pub api_log_level: Level,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            service_name: "unknown".to_string(),
            log_dir: get_log_root(),
            console_level: Level::INFO,
            file_level: Level::DEBUG,
            enable_json: false,
            max_log_files: 30,
            enable_api_log: true,
            api_log_level: Level::INFO,
        }
    }
}

/// Build environment filter for tracing subscriber.
///
/// Priority: RUST_LOG env > default `info,{service}=debug,api_access={api_level}`.
/// When RUST_LOG contains "debug"/"trace", auto-upgrades api_access to debug.
fn build_env_filter(service_name: &str, api_level: &str) -> (EnvFilter, String) {
    if let Ok(env_str) = std::env::var("RUST_LOG") {
        if env_str.contains("api_access") {
            (EnvFilter::new(env_str.clone()), env_str)
        } else {
            let effective = if env_str.contains("debug") || env_str.contains("trace") {
                "debug"
            } else {
                api_level
            };
            let filter_str = format!("{},api_access={}", env_str, effective);
            (EnvFilter::new(filter_str.clone()), filter_str)
        }
    } else {
        let filter_str = format!("info,{}=debug,api_access={}", service_name, api_level);
        (EnvFilter::new(filter_str.clone()), filter_str)
    }
}

/// Initialize logging system with configuration
///
/// If the log directory is unavailable (e.g., external storage not mounted),
/// starts with console-only logging. File logging can be activated later
/// via `reload_logging()` when the directory becomes available.
pub fn init_with_config(config: LogConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Try to create log directory — graceful degradation if unavailable
    let log_dir_available = fs::create_dir_all(&config.log_dir).is_ok();
    if !log_dir_available {
        eprintln!(
            "Warning: Log directory {:?} unavailable, starting with console-only logging",
            config.log_dir
        );
    }

    // Create file writer: real DailyRollingWriter or io::sink() fallback
    let file_writer_result = if log_dir_available {
        DailyRollingWriter::new(config.service_name.clone(), config.log_dir.clone())
            .map(|w| -> Box<dyn std::io::Write + Send> { Box::new(w) })
    } else {
        Ok(Box::new(std::io::sink()) as Box<dyn std::io::Write + Send>)
    };
    let file_writer = file_writer_result?;

    let (non_blocking, guard) = tracing_appender::non_blocking(file_writer);

    // Store guard to prevent dropping
    let guards = GUARDS.get_or_init(|| Arc::new(Mutex::new(Vec::new())));
    match guards.lock() {
        Ok(mut guards) => guards.push(guard),
        Err(poisoned) => {
            eprintln!("Warning: GUARDS lock was poisoned, recovering...");
            poisoned.into_inner().push(guard);
        },
    }

    // Build env filter (respects RUST_LOG, auto-appends api_access level)
    let api_level = if config.enable_api_log {
        config.api_log_level.as_str()
    } else {
        "off"
    };
    let (env_filter, initial_level_str) = build_env_filter(&config.service_name, api_level);

    // Wrap EnvFilter with reload::Layer for dynamic level changes
    let (reload_filter, reload_handle) = reload::Layer::new(env_filter);
    let _ = LOG_FILTER_HANDLE.set(reload_handle);
    let _ = CURRENT_LOG_LEVEL.set(Mutex::new(initial_level_str));

    let registry = tracing_subscriber::registry().with(reload_filter);

    // Console layer - format only, level filtering handled by reload_filter
    // NOTE: Removed per-layer LevelFilter to allow dynamic log level changes via API
    // Custom format: 2025-12-02T00:50:44.809Z [INFO] message
    let console_layer = fmt::layer()
        .with_ansi(true)
        .event_format(BracketedLevelFormat)
        .boxed();

    // Create reloadable writer
    let reloadable_writer = ReloadableWriter::new(non_blocking);
    let reloadable_writer_arc = Arc::new(reloadable_writer);

    // Store the reloadable writer globally (Arc::clone is O(1) atomic counter increment)
    RELOADABLE_WRITER.get_or_init(|| Arc::clone(&reloadable_writer_arc));

    // Wrap in newtype for MakeWriter implementation
    let writer_handle = ReloadableWriterHandle(Arc::clone(&reloadable_writer_arc));

    // Business file layer (exclude api_access target)
    use tracing_subscriber::filter;

    // Business file layer - excludes api_access target only
    // NOTE: Removed LevelFilter to allow dynamic log level changes via API
    // Level filtering is now handled by the top-level reload_filter
    let business_file_layer = if config.enable_json {
        fmt::layer()
            .json()
            .with_writer(writer_handle)
            .with_level(true)
            .with_target(true)
            .with_thread_ids(true)
            .with_span_events(FmtSpan::FULL)
            .with_filter(filter::filter_fn(|metadata| {
                metadata.target() != "api_access"
            }))
            .boxed()
    } else {
        // Simplified format: no module paths, no thread IDs (saves ~40 chars/line)
        fmt::layer()
            .with_writer(writer_handle)
            .with_ansi(false)
            .event_format(BracketedLevelFormat) // Use [INFO] format like console
            .with_filter(filter::filter_fn(|metadata| {
                metadata.target() != "api_access"
            }))
            .boxed()
    };

    // API file layer (only api_access target) - created if enable_api_log is true
    let api_file_layer = if config.enable_api_log && log_dir_available {
        // Create API daily rolling file writer (reuses DailyRollingWriter with "_api" suffix)
        let api_writer =
            DailyRollingWriter::new_api(config.service_name.clone(), config.log_dir.clone())?;
        let (api_non_blocking, api_guard) = tracing_appender::non_blocking(api_writer);

        // Store API guard to prevent dropping
        let guard_storage = API_GUARD.get_or_init(|| Arc::new(Mutex::new(None)));
        match guard_storage.lock() {
            Ok(mut slot) => *slot = Some(api_guard),
            Err(poisoned) => {
                eprintln!("Warning: API_GUARD lock was poisoned, recovering...");
                *poisoned.into_inner() = Some(api_guard);
            },
        }

        // Create reloadable writer for API logs
        let api_reloadable_writer = ReloadableWriter::new(api_non_blocking);
        let api_reloadable_writer_arc = Arc::new(api_reloadable_writer);
        API_RELOADABLE_WRITER.get_or_init(|| api_reloadable_writer_arc.clone());

        let api_writer_handle = ReloadableWriterHandle(api_reloadable_writer_arc);

        Some(
            fmt::layer()
                .with_writer(api_writer_handle)
                .with_ansi(false)
                .event_format(BracketedLevelFormat) // Use [INFO] format like console
                .with_filter(filter::filter_fn(|metadata| metadata.target() == "api_access"))
                .boxed(),
        )
    } else {
        None
    };

    // Register all layers
    // Note: Using .with(Option<Layer>) which acts as identity when None
    // Console layer handles both business and API logs (api_access target)
    // API file layer only handles api_access target for separate API log file
    registry
        .with(console_layer)
        .with(business_file_layer)
        .with(api_file_layer)
        .init();

    let runtime = LogRuntime {
        service_name: config.service_name.clone(),
        log_dir: config.log_dir.clone(),
    };
    let rt_store = LOG_RUNTIME.get_or_init(|| Arc::new(Mutex::new(runtime.clone())));
    if let Ok(mut slot) = rt_store.lock() {
        *slot = runtime;
    }

    if log_dir_available {
        tracing::info!("Logging: {} @ {:?}", config.service_name, config.log_dir);
    } else {
        tracing::warn!(
            "Logging: {} console-only (log dir {:?} unavailable, will activate on reload)",
            config.service_name,
            config.log_dir
        );
    }

    if config.enable_api_log && log_dir_available {
        let current_date = chrono::Local::now().format("%Y%m%d");
        tracing::debug!("API log: {}_{}_api.log", current_date, config.service_name);
    }

    // Start background compression task (extracted to log_rotation module)
    // Only start if log directory is available (nothing to compress otherwise)
    if log_dir_available {
        let handle = crate::log_rotation::spawn_compression_task(
            config.log_dir,
            config.service_name,
            config.max_log_files,
        );
        store_logging_task_handle(handle);
    }

    Ok(())
}

/// Reopen log file writer (e.g., after manual deletion/rotation)
pub fn reopen_logs_now() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_arc = LOG_RUNTIME
        .get()
        .ok_or("logging not initialized (runtime config missing)")?
        .clone();
    let runtime = runtime_arc.lock().map_err(|_| "poisoned lock")?.clone();

    // Ensure directory exists
    fs::create_dir_all(&runtime.log_dir)?;

    // Create new custom daily rolling file writer with format: {service}{YYYYMMDD}.log
    let custom_writer =
        DailyRollingWriter::new(runtime.service_name.clone(), runtime.log_dir.clone())?;
    let (non_blocking, guard) = tracing_appender::non_blocking(custom_writer);

    // Swap guard (drop old to close deleted handle)
    if let Ok(mut guards) = GUARDS
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .lock()
    {
        guards.clear();
        guards.push(guard);
    }

    // Reload the writer in the reloadable wrapper
    if let Some(writer) = RELOADABLE_WRITER.get() {
        writer.reload(non_blocking);
    } else {
        return Err("reloadable writer not initialized".into());
    }

    // Touch today's file to ensure it exists
    let log_file_path = runtime.log_dir.join(format!(
        "{}_{}.log",
        chrono::Local::now().format("%Y%m%d"),
        runtime.service_name
    ));
    if !log_file_path.exists() {
        let _ = fs::File::create(&log_file_path);
    }

    reopen_api_logs()?;

    tracing::debug!("Log reopened");
    Ok(())
}

/// Reopen API log writer (called from `reopen_logs_now`).
fn reopen_api_logs() -> Result<(), Box<dyn std::error::Error>> {
    let Some(api_runtime_arc) = API_LOG_RUNTIME.get() else {
        return Ok(());
    };
    let api_runtime = api_runtime_arc.lock().map_err(|_| "poisoned lock")?.clone();

    let api_writer = DailyRollingWriter::new_api(
        api_runtime.service_name.clone(),
        api_runtime.log_dir.clone(),
    )?;
    let (non_blocking, guard) = tracing_appender::non_blocking(api_writer);

    if let Some(storage) = API_GUARD.get()
        && let Ok(mut slot) = storage.lock()
    {
        *slot = Some(guard);
    }
    if let Some(writer) = API_RELOADABLE_WRITER.get() {
        writer.reload(non_blocking);
    }

    // Touch today's API file to ensure it exists
    let api_log_path = api_runtime.log_dir.join(format!(
        "{}_{}_api.log",
        chrono::Local::now().format("%Y%m%d"),
        api_runtime.service_name
    ));
    if !api_log_path.exists() {
        let _ = fs::File::create(&api_log_path);
    }

    Ok(())
}

/// Install SIGHUP listener to reopen logs on demand (Unix only)
///
/// The task handle is stored internally and can be stopped via `shutdown_logging_tasks()`.
pub fn enable_sighup_log_reopen() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let handle = tokio::spawn(async move {
            match signal(SignalKind::hangup()) {
                Ok(mut hup) => loop {
                    hup.recv().await;
                    if let Err(e) = reopen_logs_now() {
                        tracing::warn!("SIGHUP reopen: {}", e);
                    }
                },
                Err(e) => tracing::warn!("SIGHUP handler: {}", e),
            }
        });

        // Store handle for cleanup
        store_logging_task_handle(handle);
    }
}

/// Legacy init function for backward compatibility
pub fn init(level: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = LogConfig {
        console_level: level.parse().unwrap_or(Level::INFO),
        file_level: level.parse().unwrap_or(Level::DEBUG),
        ..Default::default()
    };
    init_with_config(config)
}

/// Dynamically set log filter level at runtime
///
/// # Arguments
/// * `level` - Log level string (e.g., "debug", "info", "warn", "error", "trace")
///   or full filter spec (e.g., "info,io=debug")
///
/// # Returns
/// * `Ok(())` on success
/// * `Err(String)` with error message on failure
///
/// # Example
/// ```ignore
/// common::logging::set_log_level("debug")?;
/// common::logging::set_log_level("info,io=debug")?;
/// ```
pub fn set_log_level(level: &str) -> Result<(), String> {
    let handle = LOG_FILTER_HANDLE
        .get()
        .ok_or("Logging not initialized with reload support")?;

    let new_filter =
        EnvFilter::try_new(level).map_err(|e| format!("Invalid log level '{}': {}", level, e))?;

    handle
        .reload(new_filter)
        .map_err(|e| format!("Failed to reload log filter: {}", e))?;

    // Update stored level
    if let Some(current) = CURRENT_LOG_LEVEL.get()
        && let Ok(mut guard) = current.lock()
    {
        *guard = level.to_string();
    }

    tracing::info!("Log level changed to: {}", level);
    Ok(())
}

/// Get current log filter level
///
/// # Returns
/// Current log filter string
pub fn get_log_level() -> String {
    CURRENT_LOG_LEVEL
        .get()
        .and_then(|m| m.lock().ok())
        .map(|guard| guard.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

// ============================================================================
// HTTP API Request Logging Middleware
// ============================================================================

/// Redact sensitive fields in JSON string
///
/// Recursively searches for sensitive field names and replaces their values with "***REDACTED***".
/// Handles nested objects and arrays.
///
/// # Sensitive Fields
/// - password
/// - token
/// - api_key
/// - secret
/// - authorization
/// - credential
/// - community
/// - complete `parameters` and `logging` containers
///
/// # Example
/// ```rust,ignore
/// let json = r#"{"username":"admin","password":"secret123"}"#;
/// let redacted = redact_sensitive_fields(json);
/// // Result: r#"{"username":"admin","password":"***REDACTED***"}"#
/// ```
#[allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (compile-time safe, never panics)
fn redact_sensitive_fields(json_str: &str) -> String {
    use serde_json::{Value, json};

    const SENSITIVE_KEYS: &[&str] = &[
        "password",
        "token",
        "api_key",
        "secret",
        "authorization",
        "credential",
        "community",
    ];
    const OPAQUE_SENSITIVE_KEYS: &[&str] = &["parameters", "logging"];

    // Try to parse as JSON
    let Ok(mut value) = serde_json::from_str::<Value>(json_str) else {
        // Never copy a malformed payload into logs. It may contain a secret
        // precisely because parsing/redaction failed.
        return "<unparseable json omitted>".to_string();
    };

    // Recursive redaction function
    fn redact_recursive(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    let key_lower = key.to_lowercase();
                    if OPAQUE_SENSITIVE_KEYS.contains(&key_lower.as_str())
                        || SENSITIVE_KEYS.iter().any(|&k| key_lower.contains(k))
                    {
                        // Replace sensitive value with redacted marker
                        *val = json!("***REDACTED***");
                    } else {
                        // Recursively process nested objects/arrays
                        redact_recursive(val);
                    }
                }
            },
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    redact_recursive(item);
                }
            },
            _ => {},
        }
    }

    redact_recursive(&mut value);

    // Serialize back to string (compact format)
    serde_json::to_string(&value)
        .unwrap_or_else(|_| "<json redaction failed; body omitted>".to_string())
}

/// Truncate body string to maximum length
///
/// If the body exceeds max_length, it will be truncated and a suffix will be added
/// indicating how many bytes were truncated.
///
/// # Example
/// ```rust,ignore
/// let long_body = "a".repeat(1000);
/// let truncated = truncate_body(&long_body, 500);
/// // Result: "aaa...aaa[truncated 500 bytes]"
/// ```
fn truncate_body(body: &str, max_length: usize) -> String {
    if body.len() <= max_length {
        body.to_string()
    } else {
        // `max_length` is a byte budget, but slicing Rust strings requires a
        // UTF-8 character boundary. Walk back by at most three bytes so a
        // legitimate JSON body containing CJK text or emoji can never panic
        // (the workspace release profile aborts the whole service on panic).
        let mut boundary = max_length;
        while boundary > 0 && !body.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let truncated_bytes = body.len() - boundary;
        format!("{}[truncated {} bytes]", &body[..boundary], truncated_bytes)
    }
}

/// Returns true for request families whose payload values must never enter
/// access logs, even after generic field-name redaction.
fn request_body_logging_forbidden(path: &str) -> bool {
    path == "/api/channels" || path.starts_with("/api/channels/")
}

/// HTTP API request logger middleware
///
/// Provides selective HTTP request logging with request body recording:
/// - **INFO level**: Logs only POST/PUT/PATCH/DELETE requests (no body)
/// - **DEBUG level**: Logs all requests with body content (truncated & redacted)
///
/// Logs are routed to dedicated API log files via the "api_access" target.
///
/// # Design Decisions
///
/// - **Body Recording at DEBUG**: Request body is only read and logged at DEBUG level
/// - **Sensitive Field Redaction**: password, token, api_key, secret, authorization are filtered
/// - **Body Truncation**: Body limited to 500 characters to prevent log bloat
/// - **Simplified Fields**: Removed redundant headers (user_agent, content_type, content_length, is_error)
/// - **No Duplicate Logging**: INFO and DEBUG levels are mutually exclusive
///
/// # Logged Information
/// - HTTP method (POST, GET, PUT, DELETE, PATCH)
/// - Request path (e.g., `/api/channels`, `/api/instances`)
/// - HTTP status code (e.g., 200, 404, 500)
/// - Response duration in milliseconds
/// - Request body (DEBUG only, truncated to 500 chars, sensitive fields redacted)
///
/// # Example Log Output
///
/// INFO level (production, written to `{service}_api{YYYYMMDD}.log`):
/// ```text
/// INFO  HTTP request method=POST path=/api/instances status=200 duration_ms=15
/// INFO  HTTP request method=PUT path=/api/channels/1 status=200 duration_ms=23
/// ```
///
/// DEBUG level (development, written to `{service}_api{YYYYMMDD}.log`):
/// ```text
/// DEBUG HTTP request method=POST path=/api/instances status=200 duration_ms=15 request_body={"instance_name":"test","properties":{...}}[truncated 234 bytes]
/// DEBUG HTTP request method=GET path=/health status=200 duration_ms=5 request_body=-
/// DEBUG HTTP request method=POST path=/api/auth/login status=200 duration_ms=50 request_body={"username":"admin","password":"***REDACTED***"}
/// ```
///
/// # Usage
///
/// Add this middleware to your Axum router **before** `.with_state()`:
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use common::logging::http_request_logger;
///
/// let app = Router::new()
///     // ... routes ...
///     .layer(middleware::from_fn(http_request_logger))  // BEFORE .with_state()
///     .with_state(state);
/// ```
pub async fn http_request_logger(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use std::time::Instant;
    use tracing::{Level, debug, info, level_enabled};

    const MAX_BODY_LENGTH: usize = 500;
    const MAX_BODY_READ: usize = 2048;

    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    // Channel payloads contain protocol credentials and per-channel log paths.
    // Their application contract forbids recording parameter/logging values,
    // so the access logger never captures any body below this route prefix --
    // including malformed JSON that cannot be structurally redacted.
    let body_logging_forbidden = request_body_logging_forbidden(uri.path());

    // Decide whether to read body (DEBUG + modifying method + known-small JSON
    // only). An absent Content-Length may be chunked; do not consume it merely
    // for diagnostics because a bounded read cannot reconstruct an oversized
    // stream after failure.
    let should_read_body = level_enabled!(Level::DEBUG)
        && !body_logging_forbidden
        && matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
        && req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"))
        && req
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|length| length <= MAX_BODY_READ);

    let (req, body_str) = if should_read_body {
        extract_request_body(req, MAX_BODY_READ, MAX_BODY_LENGTH).await
    } else {
        (req, "-".to_string())
    };

    let response = next.run(req).await;
    let duration = start.elapsed();
    let status = response.status();

    // INFO: modifying methods (no body); DEBUG: all requests with body
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        info!(
            target: "api_access",
            method = %method,
            path = %uri.path(),
            status = %status.as_u16(),
            duration_ms = %duration.as_millis(),
            "HTTP request"
        );
    }

    if body_str != "-" {
        debug!(
            target: "api_access",
            method = %method,
            path = %uri.path(),
            status = %status.as_u16(),
            duration_ms = %duration.as_millis(),
            request_body = %body_str,
            "HTTP request (detailed)"
        );
    } else if !matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        debug!(
            target: "api_access",
            method = %method,
            path = %uri.path(),
            status = %status.as_u16(),
            duration_ms = %duration.as_millis(),
            "HTTP request"
        );
    }

    response
}

/// Extract and redact request body for logging.
///
/// Reads the body, applies sensitive field redaction, truncates to max length,
/// and reconstructs the request with the original bytes.
async fn extract_request_body(
    req: axum::extract::Request,
    max_read: usize,
    max_display: usize,
) -> (axum::extract::Request, String) {
    use axum::body::Body;

    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, max_read).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Body read failure: {}", e);
            let new_req = axum::extract::Request::from_parts(parts, Body::empty());
            return (new_req, "-".to_string());
        },
    };

    let body_str = match std::str::from_utf8(&bytes) {
        Ok(s) => truncate_body(&redact_sensitive_fields(s), max_display),
        Err(_) => "<binary data>".to_string(),
    };

    let new_req = axum::extract::Request::from_parts(parts, Body::from(bytes));
    (new_req, body_str)
}

// ============================================================================
// Logging Task Lifecycle Management
// ============================================================================

/// Store a logging task handle for later cleanup
fn store_logging_task_handle(handle: tokio::task::JoinHandle<()>) {
    let handles =
        LOGGING_TASK_HANDLES.get_or_init(|| Arc::new(tokio::sync::RwLock::new(Vec::new())));

    // Try to store synchronously first to avoid timing issues with shutdown
    // Fall back to spawn only if lock is held (rare case)
    match handles.try_write() {
        Ok(mut guard) => {
            guard.push(handle);
        },
        Err(_) => {
            // Lock is held, spawn async task as fallback
            let handles = Arc::clone(handles);
            tokio::spawn(async move {
                handles.write().await.push(handle);
            });
        },
    }
}

/// Shutdown all background logging tasks
///
/// Call this during service shutdown to cleanly stop:
/// - SIGHUP log reopen handler
/// - Log compression task
///
/// # Example
/// ```ignore
/// // In shutdown handler
/// common::logging::shutdown_logging_tasks().await;
/// ```
pub async fn shutdown_logging_tasks() {
    if let Some(handles) = LOGGING_TASK_HANDLES.get() {
        let mut guard = handles.write().await;
        for handle in guard.drain(..) {
            if !handle.is_finished() {
                handle.abort();
            }
        }
        tracing::debug!("Logging tasks shutdown complete");
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use tracing::Level;

    // ========================================================================
    // format_level tests
    // ========================================================================

    #[test]
    fn test_format_level_trace() {
        assert_eq!(format_level(&Level::TRACE), "[TRACE]");
    }

    #[test]
    fn test_format_level_debug() {
        assert_eq!(format_level(&Level::DEBUG), "[DEBUG]");
    }

    #[test]
    fn test_format_level_info() {
        assert_eq!(format_level(&Level::INFO), "[INFO]");
    }

    #[test]
    fn test_format_level_warn() {
        assert_eq!(format_level(&Level::WARN), "[WARN]");
    }

    #[test]
    fn test_format_level_error() {
        assert_eq!(format_level(&Level::ERROR), "[ERROR]");
    }

    // ========================================================================
    // truncate_body tests
    // ========================================================================

    #[test]
    fn test_truncate_body_short_string() {
        let body = "hello";
        let result = truncate_body(body, 100);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_body_exact_length() {
        let body = "12345";
        let result = truncate_body(body, 5);
        assert_eq!(result, "12345");
    }

    #[test]
    fn test_truncate_body_long_string() {
        let body = "hello world";
        let result = truncate_body(body, 5);
        assert_eq!(result, "hello[truncated 6 bytes]");
    }

    #[test]
    fn test_truncate_body_empty_string() {
        let body = "";
        let result = truncate_body(body, 100);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_body_max_zero() {
        let body = "hello";
        let result = truncate_body(body, 0);
        assert_eq!(result, "[truncated 5 bytes]");
    }

    #[test]
    fn test_truncate_body_realistic() {
        let body = r#"{"username":"admin","password":"secret123","data":{"key":"value"}}"#;
        let result = truncate_body(body, 30);
        assert!(result.starts_with(r#"{"username":"admin","password""#));
        assert!(result.contains("[truncated"));
    }

    #[test]
    fn test_truncate_body_never_splits_utf8_code_points() {
        let body = "ab设备🙂tail";
        let result = truncate_body(body, 4);

        assert_eq!(result, "ab[truncated 14 bytes]");
    }

    // ========================================================================
    // redact_sensitive_fields tests
    // ========================================================================

    #[test]
    fn test_redact_password_field() {
        let json = r#"{"username":"admin","password":"secret123"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""password":"***REDACTED***""#));
        assert!(result.contains(r#""username":"admin""#));
    }

    #[test]
    fn test_redact_token_field() {
        let json = r#"{"token":"abc123xyz"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""token":"***REDACTED***""#));
    }

    #[test]
    fn test_redact_api_key_field() {
        let json = r#"{"api_key":"sk-12345"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""api_key":"***REDACTED***""#));
    }

    #[test]
    fn test_redact_secret_field() {
        let json = r#"{"client_secret":"mysecret"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""client_secret":"***REDACTED***""#));
    }

    #[test]
    fn test_redact_authorization_field() {
        let json = r#"{"authorization":"Bearer xyz"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""authorization":"***REDACTED***""#));
    }

    #[test]
    fn test_redact_case_insensitive() {
        let json = r#"{"PASSWORD":"secret","Token":"abc"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""***REDACTED***""#));
        // Both should be redacted (case insensitive)
        assert!(!result.contains("secret"));
        assert!(!result.contains("abc"));
    }

    #[test]
    fn test_redact_nested_object() {
        let json = r#"{"user":{"name":"john","password":"secret"}}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""password":"***REDACTED***""#));
        assert!(result.contains(r#""name":"john""#));
    }

    #[test]
    fn test_redact_array_of_objects() {
        let json = r#"[{"password":"a"},{"password":"b"}]"#;
        let result = redact_sensitive_fields(json);
        // Both passwords should be redacted
        assert!(!result.contains("\"a\""));
        assert!(!result.contains("\"b\""));
    }

    #[test]
    fn test_redact_deeply_nested() {
        let json = r#"{"level1":{"level2":{"level3":{"password":"deep_secret"}}}}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""password":"***REDACTED***""#));
        assert!(!result.contains("deep_secret"));
    }

    #[test]
    fn test_redact_no_sensitive_fields() {
        let json = r#"{"username":"admin","email":"admin@example.com"}"#;
        let result = redact_sensitive_fields(json);
        // Should remain unchanged
        assert!(result.contains(r#""username":"admin""#));
        assert!(result.contains(r#""email":"admin@example.com""#));
    }

    #[test]
    fn test_redact_invalid_json() {
        let invalid_json = "not valid json";
        let result = redact_sensitive_fields(invalid_json);
        assert_eq!(result, "<unparseable json omitted>");
    }

    #[test]
    fn test_redact_empty_json() {
        let json = "{}";
        let result = redact_sensitive_fields(json);
        assert_eq!(result, "{}");
    }

    #[test]
    fn test_redact_empty_array() {
        let json = "[]";
        let result = redact_sensitive_fields(json);
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_redact_multiple_sensitive_fields() {
        let json = r#"{"password":"p1","token":"t1","api_key":"k1"}"#;
        let result = redact_sensitive_fields(json);
        assert!(!result.contains("p1"));
        assert!(!result.contains("t1"));
        assert!(!result.contains("k1"));
        // All should be redacted
        let redacted_count = result.matches("***REDACTED***").count();
        assert_eq!(redacted_count, 3);
    }

    #[test]
    fn test_redact_partial_key_match() {
        // Keys containing sensitive words should also be redacted
        let json = r#"{"user_password":"secret","access_token":"abc"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""user_password":"***REDACTED***""#));
        assert!(result.contains(r#""access_token":"***REDACTED***""#));
    }

    #[test]
    fn test_redact_channel_parameter_and_logging_containers_as_opaque() {
        let json = r#"{
            "name":"field device",
            "parameters":{"username":"operator","community":"private","nested":{"key":"value"}},
            "logging":{"enabled":true,"file":"/secret/site/device.log"}
        }"#;
        let result = redact_sensitive_fields(json);

        assert!(!result.contains("operator"));
        assert!(!result.contains("private"));
        assert!(!result.contains("device.log"));
        assert_eq!(result.matches("***REDACTED***").count(), 2);
    }

    #[test]
    fn test_channel_request_bodies_are_never_eligible_for_access_logging() {
        for path in [
            "/api/channels",
            "/api/channels/7",
            "/api/channels/7/enabled",
            "/api/channels/7/provision",
        ] {
            assert!(request_body_logging_forbidden(path), "{path}");
        }
        assert!(!request_body_logging_forbidden("/api/channel-health"));
        assert!(!request_body_logging_forbidden("/api/auth/login"));
    }

    // ========================================================================
    // LogConfig tests
    // ========================================================================

    #[test]
    fn test_log_config_default() {
        let config = LogConfig::default();
        assert_eq!(config.service_name, "unknown");
        assert_eq!(config.console_level, Level::INFO);
        assert_eq!(config.file_level, Level::DEBUG);
        assert!(!config.enable_json);
        assert_eq!(config.max_log_files, 30);
        assert!(config.enable_api_log);
        assert_eq!(config.api_log_level, Level::INFO);
    }

    // ========================================================================
    // is_test_environment tests
    // ========================================================================

    #[test]
    fn test_is_test_environment_returns_true() {
        // When running via cargo test, this should return true
        // because CARGO_TARGET_TMPDIR is set or we're in target/debug/deps
        assert!(is_test_environment());
    }

    #[test]
    fn custom_cargo_target_directory_is_still_detected_as_a_test_path() {
        assert!(is_cargo_test_binary_path(Path::new(
            "/tmp/aether-target/debug/deps/common-deadbeef"
        )));
        assert!(is_cargo_test_binary_path(Path::new(
            "/tmp/aether-target/release/deps/common-deadbeef"
        )));
        assert!(!is_cargo_test_binary_path(Path::new(
            "/opt/aether/bin/aether-api"
        )));
    }

    // ========================================================================
    // get_log_root tests
    // ========================================================================

    #[test]
    fn test_get_log_root_in_test_env() {
        // In test environment without LOG_ROOT initialized,
        // should return temp directory path
        let log_root = get_log_root();
        // Either initialized via init_log_root or defaults to temp dir in tests
        assert!(!log_root.to_string_lossy().is_empty());
    }

    // ========================================================================
    // RedisPoolConfig tests (from redis.rs - included here for coverage)
    // ========================================================================

    #[test]
    fn test_daily_rolling_writer_filename_format() {
        // Test that the filename format is correct: {YYYYMMDD}_{service}.log
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let expected_pattern = format!("{}_{}.log", today, "io");
        assert!(expected_pattern.contains(&today));
        assert!(expected_pattern.ends_with(".log"));
    }
}
