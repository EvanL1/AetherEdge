use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    Json,
    body::Body,
    extract::{Multipart, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tracing::{error, info};

use crate::auth::Claims;
use crate::routes_auth::require_admin;
use crate::state::AppState;

const CONFIG_PATH_ENV: &str = "AETHER_CONFIG_PATH";
const SYSTEMD_CONFIG_DIR: &str = "/etc/aether/config";
const CONTAINER_CONFIG_DIR: &str = "/app/data/config";
const LEGACY_CONFIG_DIR: &str = "/opt/AetherEdge/data/config";
const MAX_CONFIG_ARCHIVE_ENTRIES: usize = 4_096;
const MAX_CONFIG_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const UPGRADE_DIR: &str = "/opt/AetherEdge/upgrade";
const UPGRADE_STATUS_FILE: &str = "/opt/AetherEdge/upgrade/upgrade_status.json";
const UPGRADE_LOG_FILE: &str = "/opt/AetherEdge/upgrade/upgrade.log";

// In-memory upgrade state (also mirrored to UPGRADE_STATUS_FILE for persistence across restarts)
static UPGRADE_PID: Mutex<Option<u32>> = Mutex::new(None);
static UPGRADE_RUNNING: Mutex<bool> = Mutex::new(false);

// Upload progress tracking (reset on each new upload attempt)
static UPLOAD_RECEIVED_BYTES: AtomicU64 = AtomicU64::new(0);
static UPLOAD_TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);
// Set while an upload stream is in progress; cleared when it ends (success or error).
static UPLOAD_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
// Set by abort_upgrade to signal the streaming loop to stop early.
static UPLOAD_ABORT_REQUESTED: AtomicBool = AtomicBool::new(false);

fn select_config_directory(explicit: Option<PathBuf>, candidates: &[PathBuf]) -> Option<PathBuf> {
    explicit.or_else(|| candidates.iter().find(|path| path.is_dir()).cloned())
}

/// Resolve the active static-configuration tree without confusing it with the
/// runtime data directory. Composition roots should set `AETHER_CONFIG_PATH`;
/// the known-layout fallbacks keep existing Docker and systemd installs usable.
fn config_directory() -> PathBuf {
    let explicit = std::env::var_os(CONFIG_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let database_relative = std::env::var_os("AETHER_DB_PATH")
        .map(PathBuf::from)
        .and_then(|path| path.parent().map(|parent| parent.join("config")));

    let mut candidates = vec![
        PathBuf::from(SYSTEMD_CONFIG_DIR),
        PathBuf::from(CONTAINER_CONFIG_DIR),
    ];
    if let Some(path) = database_relative.clone() {
        candidates.push(path);
    }
    candidates.push(PathBuf::from(LEGACY_CONFIG_DIR));
    candidates.push(PathBuf::from("data/config"));
    candidates.push(PathBuf::from("config"));

    if let Some(selected) = select_config_directory(explicit, &candidates) {
        return selected;
    }

    // Preserve the intended layout even on a damaged/missing installation so
    // `/config/check` reports the correct missing path instead of a data root.
    if Path::new("/etc/aether/install.yaml").exists() {
        return PathBuf::from(SYSTEMD_CONFIG_DIR);
    }
    if Path::new("/app/data").exists() {
        return PathBuf::from(CONTAINER_CONFIG_DIR);
    }
    database_relative.unwrap_or_else(|| PathBuf::from(LEGACY_CONFIG_DIR))
}

fn require_config_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Claims, (StatusCode, Json<serde_json::Value>)> {
    require_admin(state, headers)
}

fn upgrade_state_error_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "success": false,
            "message": "Upgrade state is unavailable; please retry or restart api"
        })),
    )
        .into_response()
}

fn lock_upgrade_running() -> Option<MutexGuard<'static, bool>> {
    match UPGRADE_RUNNING.lock() {
        Ok(running) => Some(running),
        Err(e) => {
            error!("Upgrade running lock poisoned: {}", e);
            None
        },
    }
}

fn lock_upgrade_pid() -> Option<MutexGuard<'static, Option<u32>>> {
    match UPGRADE_PID.lock() {
        Ok(pid) => Some(pid),
        Err(e) => {
            error!("Upgrade PID lock poisoned: {}", e);
            None
        },
    }
}

fn write_upgrade_status(data: &serde_json::Value) {
    if let Ok(s) = serde_json::to_string_pretty(data) {
        let _ = std::fs::write(UPGRADE_STATUS_FILE, s);
    }
}

/// Called once at api startup.
///
/// If the status file shows `"running"` but `UPGRADE_RUNNING` is false (fresh
/// process), the previous upgrade was interrupted by a container restart — most
/// likely because the installer replaced the aetherems image and brought the
/// service back up.  Mark the status as "completed_or_restarted" so the status
/// endpoint never returns the contradictory `running=false` + `status="running"`.
pub fn reconcile_upgrade_status_on_startup() {
    if let Ok(content) = std::fs::read_to_string(UPGRADE_STATUS_FILE)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&content)
        && v.get("status").and_then(|s| s.as_str()) == Some("running")
    {
        write_upgrade_status(&json!({
            "status": "completed_or_restarted",
            "message": "Service was restarted during upgrade (likely by the installer). Check the upgrade log for the actual outcome.",
            "recovered_at": chrono::Utc::now().to_rfc3339(),
        }));
        info!("Recovered stale upgrade status: 'running' → 'completed_or_restarted'");
    }
}

async fn read_upgrade_status() -> serde_json::Value {
    tokio::fs::read_to_string(UPGRADE_STATUS_FILE)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"status": "idle"}))
}

// ── GET /api/v1/config/check ──────────────────────────────────────────────────

/// Check the health of the configuration directory.
///
/// Scans the `config/` directory structure (products, instances, routing, etc.)
/// and reports existence, completeness, and SQLite consistency for each
/// sub-module. Use as a pre-flight check before importing configuration or
/// running an upgrade, and as an indicator on the operations dashboard.
/// **Read-only.**
#[utoipa::path(get, path = "/api/v1/config/check", tag = "Config",
    security(("bearer_auth" = [])),
    responses((status = 200, description = "Configuration directory check result")))]
pub async fn check_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(response) = require_config_admin(&state, &headers) {
        return response.into_response();
    }

    let dir = config_directory();
    if !dir.exists() {
        return Json(json!({
            "success": false,
            "message": format!("Config directory not found: {}", dir.display()),
            "data": { "exists": false, "path": dir }
        }))
        .into_response();
    }

    let entries: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(e) => {
            error!("Failed to read config directory {}: {}", dir.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "message": format!("Failed to read config directory: {}", e)
                })),
            )
                .into_response();
        },
    };

    Json(json!({
        "success": true,
        "message": "Config directory check completed",
        "data": {
            "exists": true,
            "path": dir,
            "file_count": entries.len(),
            "files": entries,
        }
    }))
    .into_response()
}

// ── GET /api/v1/config/export ─────────────────────────────────────────────────

/// Export the current configuration as a ZIP archive.
///
/// Packages the entire `config/` directory tree (product definitions,
/// instances, routing, rules, etc.) into a ZIP stream returned as an
/// `attachment`. Use for site-to-site configuration migration, pre-upgrade
/// backups, and remote-support reproduction. The export includes only static
/// configuration files; live SHM state is intentionally excluded.
#[utoipa::path(get, path = "/api/v1/config/export", tag = "Config",
    security(("bearer_auth" = [])),
    responses((status = 200, description = "ZIP file stream")))]
pub async fn export_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match require_config_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response.into_response(),
    };
    info!(
        actor_user_id = admin.user_id,
        actor = %admin.username,
        action = "config.export",
        "Authorized configuration export"
    );

    let dir = config_directory();
    if !dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "message": "Config directory not found"})),
        )
            .into_response();
    }

    match create_zip_archive(&dir) {
        Ok(data) => {
            let filename = format!("config_{}.zip", chrono::Utc::now().format("%Y%m%d_%H%M%S"));
            match Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/zip")
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                )
                .body(Body::from(data))
            {
                Ok(response) => response,
                Err(e) => {
                    error!("Build export response error: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(
                            json!({"success": false, "message": "Failed to build export response"}),
                        ),
                    )
                        .into_response()
                },
            }
        },
        Err(e) => {
            error!("Export config error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Failed to export configuration"})),
            )
                .into_response()
        },
    }
}

fn create_zip_archive(dir: &Path) -> io::Result<Vec<u8>> {
    let buf = Vec::new();
    let cursor = io::Cursor::new(buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let base = dir;
    for entry in walkdir_safe(base)? {
        let rel = entry
            .strip_prefix(base)
            .map_err(|e| io::Error::other(format!("invalid archive path: {}", e)))?;
        let rel_str = rel.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "configuration paths must be valid UTF-8",
            )
        })?;

        let file_type = std::fs::symlink_metadata(&entry)?.file_type();
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "symbolic links are not allowed in configuration exports",
            ));
        }
        if file_type.is_dir() {
            zip.add_directory(format!("{}/", rel_str), options)?;
        } else if file_type.is_file() {
            zip.start_file(rel_str, options)?;
            let mut source = std::fs::File::open(&entry)?;
            io::copy(&mut source, &mut zip)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "special files are not allowed in configuration exports",
            ));
        }
    }

    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

fn walkdir_safe(dir: &Path) -> io::Result<Vec<PathBuf>> {
    fn visit(dir: &Path, paths: &mut Vec<PathBuf>, total_bytes: &mut u64) -> io::Result<()> {
        let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);

        for entry in entries {
            if paths.len() >= MAX_CONFIG_ARCHIVE_ENTRIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "configuration export contains too many entries",
                ));
            }

            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "symbolic links are not allowed in configuration exports",
                ));
            }

            paths.push(path.clone());
            if file_type.is_dir() {
                visit(&path, paths, total_bytes)?;
            } else if file_type.is_file() {
                *total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "configuration export size overflow",
                    )
                })?;
                if *total_bytes > MAX_CONFIG_ARCHIVE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "configuration export exceeds the 64 MB limit",
                    ));
                }
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "special files are not allowed in configuration exports",
                ));
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    let mut total_bytes = 0;
    visit(dir, &mut paths, &mut total_bytes)?;
    Ok(paths)
}

// ── POST /api/v1/config/import ────────────────────────────────────────────────

/// Remote configuration import is deliberately disabled.
///
/// A safe implementation must stage and validate the complete archive, apply
/// the derived SQLite changes transactionally, atomically replace the static
/// configuration tree, and roll both back if activation fails. Until that
/// workflow exists, accepting ZIP uploads would expose partial-write and
/// path-traversal hazards. Operators must use the local `aether` CLI instead.
#[utoipa::path(post, path = "/api/v1/config/import", tag = "Config",
    security(("bearer_auth" = [])),
    responses((status = 501, description = "Remote configuration import is disabled")))]
pub async fn import_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(response) = require_config_admin(&state, &headers) {
        return response.into_response();
    }
    remote_config_mutation_disabled_response()
}

// ── POST /api/v1/config/restart-services ─────────────────────────────────────

/// Remote service restart is deliberately disabled with remote ZIP import.
///
/// The local `aether services` command selects Docker Compose or systemd using
/// the installed runtime context. The management API must not guess a backend
/// or report success after a partial restart.
#[utoipa::path(post, path = "/api/v1/config/restart-services", tag = "Config",
    security(("bearer_auth" = [])),
    responses((status = 501, description = "Remote service restart is disabled")))]
pub async fn restart_services(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(response) = require_config_admin(&state, &headers) {
        return response.into_response();
    }
    remote_config_mutation_disabled_response()
}

fn remote_config_mutation_disabled_response() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "success": false,
            "message": "Remote configuration mutation is disabled until staged validation and atomic rollback are implemented. Run `aether sync --dry-run`, then `aether sync`, on the edge host."
        })),
    )
        .into_response()
}

// ── POST /api/v1/config/upgrade ───────────────────────────────────────────────

/// Remote upgrades are disabled until release signatures, fixed-name staging,
/// architecture/version checks, and crash-safe rollback are implemented.
/// Operators must verify and run the release installer locally on the edge.
#[utoipa::path(post, path = "/api/v1/config/upgrade", tag = "Config",
    security(("bearer_auth" = [])),
    request_body(content_type = "multipart/form-data", description = "Upgrade package (.run installer)"),
    responses((status = 501, description = "Unsigned remote upgrade is disabled")))]
pub async fn start_upgrade(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    _multipart: Multipart,
) -> Response {
    let admin = match require_config_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response.into_response(),
    };
    info!(
        actor_user_id = admin.user_id,
        actor = %admin.username,
        action = "system.upgrade.denied",
        "Unsigned remote upgrade denied"
    );
    remote_upgrade_disabled_response()
}

fn remote_upgrade_disabled_response() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "success": false,
            "message": "Remote upgrade is disabled until signed artifact verification, safe staging, and rollback are implemented. Verify and run the release installer locally on the edge host."
        })),
    )
        .into_response()
}

// Quarantined legacy implementation retained temporarily for reference while
// signed upgrade staging is designed. It is not registered in the router and
// cannot be called through the HTTP API.
#[allow(dead_code)]
async fn unsigned_start_upgrade(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    use tokio::io::AsyncWriteExt;

    let admin = match require_config_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response.into_response(),
    };

    // Log immediately so that even if the connection is later dropped mid-upload,
    // there is a record that this handler was invoked.
    info!(
        actor_user_id = admin.user_id,
        actor = %admin.username,
        action = "system.upgrade.start",
        "Authorized upgrade upload request received"
    );

    {
        let running = match lock_upgrade_running() {
            Some(running) => running,
            None => return upgrade_state_error_response(),
        };
        if *running {
            return (
                StatusCode::CONFLICT,
                Json(json!({"success": false, "message": "Upgrade already in progress"})),
            )
                .into_response();
        }
    }

    // Run all prerequisite checks before touching the request body so that
    // obvious failures are reported immediately without streaming any bytes.
    let docker_socket = Path::new("/var/run/docker.sock");
    if !docker_socket.exists() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "success": false,
                "message": "Docker Socket unavailable — ensure /var/run/docker.sock is mounted in docker-compose.yml",
                "hint": "volumes:\\n  - /var/run/docker.sock:/var/run/docker.sock"
            })),
        )
            .into_response();
    }

    let upgrade_dir = Path::new(UPGRADE_DIR);
    if let Err(e) = std::fs::create_dir_all(upgrade_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": format!("Failed to create upgrade directory: {}", e)})),
        )
            .into_response();
    }

    // Clean up any leftover upgrade packages from previous runs before writing a
    // new one.  Without this, multiple 500 MB .run files accumulate on disk,
    // rapidly filling the partition and causing the next upload to slow to a crawl
    // or fail with ENOSPC mid-transfer.
    match std::fs::read_dir(upgrade_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ext == "run" || ext == "sh" {
                    if let Err(e) = std::fs::remove_file(&path) {
                        error!("Failed to remove old upgrade package {:?}: {}", path, e);
                    } else {
                        info!("Removed old upgrade package: {:?}", path);
                    }
                }
            }
        },
        Err(e) => {
            error!("Failed to list upgrade directory for cleanup: {}", e);
        },
    }

    // Refuse early if there is less than 2 GB of free space on the upgrade
    // partition — a typical .run package is ~500 MB and the installer needs
    // additional headroom for Docker image layers and extraction.  2 GB gives
    // comfortable margin even when the previous upgrade left temporary files.
    {
        let df_out = std::process::Command::new("df")
            .args(["--output=avail", "-B1", UPGRADE_DIR])
            .output();
        if let Ok(out) = df_out {
            // df output: header line + data line, avail is in bytes (-B1)
            let stdout = String::from_utf8_lossy(&out.stdout);
            if let Some(avail_bytes) = stdout
                .lines()
                .nth(1)
                .and_then(|l| l.trim().parse::<u64>().ok())
            {
                const MIN_FREE: u64 = 2_000 * 1024 * 1024; // 2 GB
                info!(
                    "Disk space check (after cleanup): {:.1} MB free on {}",
                    avail_bytes as f64 / (1024.0 * 1024.0),
                    UPGRADE_DIR
                );
                if avail_bytes < MIN_FREE {
                    return (
                        StatusCode::INSUFFICIENT_STORAGE,
                        Json(json!({
                            "success": false,
                            "message": format!(
                                "Insufficient disk space: {:.1} MB free, at least 2 GB required",
                                avail_bytes as f64 / (1024.0 * 1024.0)
                            )
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    // Get the upload field.
    let field = match multipart.next_field().await {
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"success": false, "message": "No upgrade package received. Please select an upgrade file and try again."})),
            )
                .into_response();
        },
        Err(e) => {
            let msg = classify_upload_error(&e.to_string(), 1024);
            error!("Upgrade multipart parse error: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"success": false, "message": msg})),
            )
                .into_response();
        },
        Ok(Some(f)) => f,
    };

    let pkg_name = field.file_name().unwrap_or("upgrade.run").to_string();
    let pkg_path = upgrade_dir.join(&pkg_name);

    // Extract Content-Length from the request headers for progress tracking.
    // This is the full multipart body size (slightly larger than the file itself
    // due to multipart boundaries), but accurate enough for a progress indicator.
    let total_bytes = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    UPLOAD_RECEIVED_BYTES.store(0, Ordering::Relaxed);
    UPLOAD_TOTAL_BYTES.store(total_bytes, Ordering::Relaxed);
    UPLOAD_ABORT_REQUESTED.store(false, Ordering::Relaxed);
    UPLOAD_IN_PROGRESS.store(true, Ordering::Relaxed);

    write_upgrade_status(&json!({
        "status": "uploading",
        "filename": pkg_name,
        "total_bytes": total_bytes,
        "started_at": chrono::Utc::now().to_rfc3339(),
    }));

    info!(
        "Streaming upgrade package to disk: {} → {:?}",
        pkg_name, pkg_path
    );

    // Create the output file wrapped in a 4 MB BufWriter. Multipart chunks from
    // the network are typically 8–64 KB; without buffering each chunk triggers
    // a separate write syscall. On slow eMMC / SD storage that creates severe
    // I/O backpressure which stalls the TCP receive window and makes the upload
    // appear "slow". The 4 MB buffer batches ~64–512 chunks into one large
    // write, dramatically reducing syscall overhead.
    let mut out_file = match tokio::fs::File::create(&pkg_path).await {
        Ok(f) => tokio::io::BufWriter::with_capacity(4 * 1024 * 1024, f),
        Err(e) => {
            error!("Failed to create upgrade file: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": format!("Failed to create upgrade file: {}", e)})),
            )
                .into_response();
        },
    };

    // Stream the upload directly to disk chunk-by-chunk instead of buffering
    // the entire file in memory.  A 30-minute absolute deadline handles even
    // very slow links; the same deadline is reused across iterations so the
    // total allowed time is capped, not the per-chunk time.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
    let mut received_bytes: u64 = 0;
    let mut field = field;

    // Macro to consolidate the repeated cleanup on error paths inside the loop.
    macro_rules! upload_abort_cleanup {
        ($pkg_path:expr_2021) => {{
            UPLOAD_IN_PROGRESS.store(false, Ordering::Relaxed);
            UPLOAD_ABORT_REQUESTED.store(false, Ordering::Relaxed);
            UPLOAD_RECEIVED_BYTES.store(0, Ordering::Relaxed);
            UPLOAD_TOTAL_BYTES.store(0, Ordering::Relaxed);
            drop(out_file);
            let _ = std::fs::remove_file(&$pkg_path);
        }};
    }

    loop {
        // Check for a user-initiated abort before requesting the next chunk.
        if UPLOAD_ABORT_REQUESTED.load(Ordering::Relaxed) {
            upload_abort_cleanup!(pkg_path);
            write_upgrade_status(&json!({
                "status": "aborted",
                "aborted_at": chrono::Utc::now().to_rfc3339(),
                "message": "Upload aborted by user"
            }));
            info!(
                "Upgrade upload aborted by user after {} bytes",
                received_bytes
            );
            return (
                StatusCode::GONE,
                Json(json!({"success": false, "message": "Upload aborted by user"})),
            )
                .into_response();
        }

        match tokio::time::timeout_at(deadline, field.chunk()).await {
            Err(_elapsed) => {
                upload_abort_cleanup!(pkg_path);
                write_upgrade_status(&json!({"status": "idle"}));
                error!("Upgrade upload timed out after 30 minutes");
                return (
                    StatusCode::REQUEST_TIMEOUT,
                    Json(json!({
                        "success": false,
                        "message": "Upload timed out after 30 minutes. Please check your network connection and retry."
                    })),
                )
                    .into_response();
            },
            Ok(Err(e)) => {
                upload_abort_cleanup!(pkg_path);
                write_upgrade_status(&json!({"status": "idle"}));
                let msg = classify_upload_error(&e.to_string(), 1024);
                error!("Upgrade upload read error: {}", e);
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"success": false, "message": msg})),
                )
                    .into_response();
            },
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => {
                if let Err(e) = out_file.write_all(&chunk).await {
                    upload_abort_cleanup!(pkg_path);
                    write_upgrade_status(&json!({"status": "idle"}));
                    error!("Upgrade file write error: {}", e);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"success": false, "message": format!("Failed to write upgrade package: {}", e)})),
                    )
                        .into_response();
                }
                received_bytes += chunk.len() as u64;
                UPLOAD_RECEIVED_BYTES.store(received_bytes, Ordering::Relaxed);
            },
        }
    }

    UPLOAD_IN_PROGRESS.store(false, Ordering::Relaxed);

    if let Err(e) = out_file.flush().await {
        drop(out_file);
        let _ = std::fs::remove_file(&pkg_path);
        UPLOAD_RECEIVED_BYTES.store(0, Ordering::Relaxed);
        UPLOAD_TOTAL_BYTES.store(0, Ordering::Relaxed);
        error!("Upgrade file flush error: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": format!("Failed to flush upgrade package: {}", e)})),
        )
            .into_response();
    }
    drop(out_file);

    if received_bytes == 0 {
        let _ = std::fs::remove_file(&pkg_path);
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": "No upgrade package received. Please select an upgrade file and try again."})),
        )
            .into_response();
    }

    info!(
        "Upgrade package received: {} ({:.2} MB)",
        pkg_name,
        received_bytes as f64 / (1024.0 * 1024.0)
    );

    // Lock upload counters at 100% so the status endpoint shows the completed
    // upload size during the install phase (instead of confusing 0/0 values).
    UPLOAD_RECEIVED_BYTES.store(received_bytes, Ordering::Relaxed);
    UPLOAD_TOTAL_BYTES.store(received_bytes, Ordering::Relaxed);

    // Prepare upgrade: chmod +x so the .run file is executable
    let pkg_name_lower = pkg_name.to_lowercase();
    if pkg_name_lower.ends_with(".run") || pkg_name_lower.ends_with(".sh") {
        let _ = std::process::Command::new("chmod")
            .args(["+x"])
            .arg(&pkg_path)
            .output();
    }

    // Set UPGRADE_RUNNING=true and write status file BEFORE spawning the blocking task.
    // If we set it inside spawn_blocking, the tokio thread pool may not schedule the task
    // immediately — a status poll arriving in that window would incorrectly see running=false.
    match lock_upgrade_running() {
        Some(mut running) => *running = true,
        None => return upgrade_state_error_response(),
    }

    let size_mb = received_bytes as f64 / (1024.0 * 1024.0);
    write_upgrade_status(&json!({
        "status": "running",
        "filename": pkg_name,
        "size_mb": format!("{:.2}", size_mb),
        "started_at": chrono::Utc::now().to_rfc3339(),
    }));
    let log_header = format!(
        "=== AetherEMS Upgrade Log ===\nStarted at: {}\nPackage: {}\nFile size: {:.2} MB\n{}\n\n",
        chrono::Utc::now().to_rfc3339(),
        pkg_name,
        size_mb,
        "=".repeat(60)
    );
    let _ = std::fs::write(UPGRADE_LOG_FILE, log_header);

    tokio::task::spawn_blocking(move || {
        // UPGRADE_RUNNING is already true (set before this task was spawned)

        // Remove any leftover upgrader container before starting a new one.
        // This handles the case where a previous run was killed mid-flight or the container
        // name is still registered after the last session.
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", "aetherems-upgrader"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Build the upgrade command first, then attach log redirection before spawning.
        // This ensures ALL output (stdout + stderr) from the upgrade process is captured
        // into upgrade.log so the status API can return meaningful progress to the frontend.
        //
        // IMPORTANT: do NOT use -d (detached). We run synchronously so that child.wait()
        // tracks the real upgrade completion and UPGRADE_RUNNING stays true throughout.
        let mut cmd = if pkg_name_lower.ends_with(".run") || pkg_name_lower.ends_with(".sh") {
            let docker_available = std::process::Command::new("docker")
                .args(["info"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            if docker_available {
                let mut c = std::process::Command::new("docker");
                c.args([
                    "run",
                    "--rm",
                    "--name",
                    "aetherems-upgrader",
                    "--pid",
                    "host",
                    "--privileged",
                    "-v",
                    "/opt/AetherEdge:/opt/AetherEdge",
                    "alpine:latest",
                    "nsenter",
                    "--target",
                    "1",
                    "--mount",
                    "--uts",
                    "--ipc",
                    "--net",
                    "--pid",
                    "--",
                    "bash",
                ])
                .arg(&pkg_path)
                .args(["--", "--auto"]);
                c
            } else {
                info!("Docker unavailable; running upgrade directly in container");
                let mut c = std::process::Command::new("bash");
                c.arg(&pkg_path).args(["--", "--auto"]);
                c
            }
        } else if pkg_name_lower.ends_with(".tar.gz")
            || pkg_name_lower.ends_with(".tgz")
            || pkg_name_lower.ends_with(".tar.bz2")
            || pkg_name_lower.ends_with(".tar.xz")
        {
            let mut c = std::process::Command::new("tar");
            c.args(["-xaf"]).arg(&pkg_path).arg("-C").arg(upgrade_dir);
            c
        } else {
            let mut c = std::process::Command::new("bash");
            c.arg(&pkg_path);
            c
        };

        // Redirect stdout + stderr → upgrade.log (append mode)
        // Both handles must be separate File objects (can't share one fd for stdout+stderr).
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(UPGRADE_LOG_FILE)
        {
            Ok(log_out) => match log_out.try_clone() {
                Ok(log_err) => {
                    cmd.stdout(std::process::Stdio::from(log_out))
                        .stderr(std::process::Stdio::from(log_err));
                },
                Err(e) => error!("Failed to clone log fd: {}", e),
            },
            Err(e) => error!("Failed to open upgrade log for writing: {}", e),
        }

        let result = cmd.spawn();

        match result {
            Ok(mut child) => {
                match UPGRADE_PID.lock() {
                    Ok(mut pid) => *pid = Some(child.id()),
                    Err(e) => error!("Upgrade PID lock poisoned: {}", e),
                }
                let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                if exit_code == 0 {
                    info!("Upgrade completed successfully");
                    write_upgrade_status(&json!({
                        "status": "completed",
                        "finished_at": chrono::Utc::now().to_rfc3339(),
                        "exit_code": 0,
                        "message": "Upgrade successful"
                    }));
                } else {
                    error!("Upgrade failed with exit code {}", exit_code);
                    write_upgrade_status(&json!({
                        "status": "failed",
                        "finished_at": chrono::Utc::now().to_rfc3339(),
                        "exit_code": exit_code,
                        "message": format!("Upgrade failed (exit code {})", exit_code)
                    }));
                }
            },
            Err(e) => {
                error!("Upgrade command error: {}", e);
                write_upgrade_status(&json!({
                    "status": "failed",
                    "finished_at": chrono::Utc::now().to_rfc3339(),
                    "message": format!("Failed to start upgrade: {}", e)
                }));
            },
        }

        // Delete the .run / .sh package now that the installer has finished.
        // Leaving it on disk is the primary cause of slow subsequent uploads:
        // the next upload's pre-upload cleanup still has to contend with a
        // nearly-full disk during the actual transfer.  Deleting it here frees
        // ~500 MB immediately, well before the user starts the next upgrade.
        match std::fs::read_dir(UPGRADE_DIR) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if ext == "run" || ext == "sh" {
                        if let Err(e) = std::fs::remove_file(&path) {
                            error!(
                                "Failed to delete upgrade package after install {:?}: {}",
                                path, e
                            );
                        } else {
                            info!("Deleted upgrade package after install: {:?}", path);
                        }
                    }
                }
            },
            Err(e) => error!("Post-install cleanup read_dir failed: {}", e),
        }

        match UPGRADE_RUNNING.lock() {
            Ok(mut running) => *running = false,
            Err(e) => error!("Upgrade running lock poisoned: {}", e),
        }
        match UPGRADE_PID.lock() {
            Ok(mut pid) => *pid = None,
            Err(e) => error!("Upgrade PID lock poisoned: {}", e),
        }
    });

    Json(json!({
        "success": true,
        "message": "Upgrade started",
        "data": { "package": pkg_name }
    }))
    .into_response()
}

// ── POST /api/v1/config/upgrade/abort ────────────────────────────────────────

/// Remote upgrade mutation is disabled together with the upload endpoint.
#[utoipa::path(post, path = "/api/v1/config/upgrade/abort", tag = "Config",
    security(("bearer_auth" = [])),
    responses((status = 501, description = "Unsigned remote upgrade is disabled")))]
pub async fn abort_upgrade(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match require_config_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response.into_response(),
    };
    info!(
        actor_user_id = admin.user_id,
        actor = %admin.username,
        action = "system.upgrade.abort.denied",
        "Remote upgrade abort denied because remote upgrade is disabled"
    );
    remote_upgrade_disabled_response()
}

#[allow(dead_code)]
async fn unsigned_abort_upgrade(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let admin = match require_config_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response.into_response(),
    };
    info!(
        actor_user_id = admin.user_id,
        actor = %admin.username,
        action = "system.upgrade.abort",
        "Authorized upgrade abort request"
    );

    let uploading = UPLOAD_IN_PROGRESS.load(Ordering::Relaxed);
    let pid = match lock_upgrade_pid() {
        Some(mut pid) => pid.take(),
        None => return upgrade_state_error_response(),
    };
    let installing = match lock_upgrade_running() {
        Some(running) => *running,
        None => return upgrade_state_error_response(),
    };

    if !uploading && !installing && pid.is_none() {
        return Json(json!({"success": false, "message": "No upgrade in progress"}))
            .into_response();
    }

    // ── Phase 1: abort an in-progress upload ──────────────────────────────────
    // Setting this flag causes the streaming loop in start_upgrade to break on
    // its next iteration, clean up the partial file, and write "aborted" status.
    if uploading {
        UPLOAD_ABORT_REQUESTED.store(true, Ordering::Relaxed);
        info!("Abort requested: signalled upload streaming loop to stop");
        // Give the streaming loop a moment to honour the flag and clean up,
        // then return immediately — the status endpoint will reflect "aborted".
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    // ── Phase 2: abort an in-progress install ─────────────────────────────────
    if installing || pid.is_some() {
        let _ = tokio::process::Command::new("docker")
            .args(["stop", "aetherems-upgrader"])
            .output()
            .await;

        if let Some(pid) = pid {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
        }

        match lock_upgrade_running() {
            Some(mut running) => *running = false,
            None => return upgrade_state_error_response(),
        }
        match lock_upgrade_pid() {
            Some(mut pid) => *pid = None,
            None => return upgrade_state_error_response(),
        }

        write_upgrade_status(&json!({
            "status": "aborted",
            "aborted_at": chrono::Utc::now().to_rfc3339(),
            "message": "Upgrade aborted by user"
        }));
    }

    Json(json!({"success": true, "message": "Upgrade aborted"})).into_response()
}

// ── GET /api/v1/config/upgrade/status ────────────────────────────────────────

/// Poll upgrade progress.
///
/// Returns the current upgrade phase (idle / uploading / verifying /
/// installing / restarting / done / failed), progress percentage, the latest
/// log output, and the last failure reason if any. Used by the frontend upgrade
/// page to drive the progress bar. `idle` status means no upgrade is running
/// and a new package may be submitted via `POST /upgrade`.
#[utoipa::path(get, path = "/api/v1/config/upgrade/status", tag = "Config",
    security(("bearer_auth" = [])),
    responses((status = 200, description = "Upgrade status")))]
pub async fn upgrade_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(response) = require_config_admin(&state, &headers) {
        return response.into_response();
    }

    let file_status = read_upgrade_status().await;
    let mem_running = match lock_upgrade_running() {
        Some(running) => *running,
        None => return upgrade_state_error_response(),
    };
    let mem_pid = match lock_upgrade_pid() {
        Some(pid) => *pid,
        None => return upgrade_state_error_response(),
    };

    // Cross-check: if memory says not running but status file says "running",
    // verify whether the upgrader container is actually still alive on the host.
    // Use tokio::process::Command (non-blocking) with a 3-second timeout so that
    // a slow or unavailable docker daemon never stalls the status response.
    let container_running = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new("docker")
            .args(["inspect", "-f", "{{.State.Running}}", "aetherems-upgrader"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .and_then(|o| String::from_utf8(o.stdout).ok())
    .map(|s| s.trim() == "true")
    .unwrap_or(false);

    let running = mem_running || container_running;

    // If the status file still says "running" but we can confirm nothing is
    // actually running (not in memory, no upgrader container), the previous
    // run was cut short by a service restart.  Fix the file now so subsequent
    // queries and the current response are consistent.
    let file_status = if !running
        && file_status.get("status").and_then(|s| s.as_str()) == Some("running")
    {
        let recovered = json!({
            "status": "completed_or_restarted",
            "message": "Service was restarted during upgrade (likely by the installer). Check the upgrade log for the actual outcome.",
            "recovered_at": chrono::Utc::now().to_rfc3339(),
        });
        write_upgrade_status(&recovered);
        info!(
            "Status inconsistency detected: status file was 'running' but no upgrade in progress; corrected to 'completed_or_restarted'"
        );
        recovered
    } else {
        file_status
    };

    let log_content = tokio::fs::read_to_string(UPGRADE_LOG_FILE)
        .await
        .map(|raw| clean_log(&raw))
        .unwrap_or_default();

    // Upload progress (only meaningful while detail.status == "uploading")
    let received = UPLOAD_RECEIVED_BYTES.load(Ordering::Relaxed);
    let total = UPLOAD_TOTAL_BYTES.load(Ordering::Relaxed);
    let upload_progress_pct: Option<f64> = if total > 0 {
        Some((received as f64 / total as f64 * 100.0).min(100.0))
    } else {
        None
    };

    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "running": running,
            "pid": mem_pid,
            "log": log_content,
            "detail": file_status,
            "upload": {
                "received_bytes": received,
                "total_bytes": total,
                "progress_pct": upload_progress_pct,
            }
        }
    }))
    .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Strip ANSI escape sequences and collapse backspace-based progress-bar overwriting.
///
/// Makeself progress bars work by writing e.g. "  0% \b\b\b\b\b  1% \b\b\b\b\b  2%"
/// which looks correct in a terminal (each % overwrites the previous) but produces
/// garbage when captured to a file.  This function simulates the terminal rendering
/// so each "line" only keeps the final visible content.
fn clean_log(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            // ANSI escape: ESC '[' <params> <final-byte>
            0x1b if i + 1 < bytes.len() && bytes[i + 1] == b'[' => {
                i += 2;
                while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                i += 1; // skip the final letter
            },
            // Backspace: erase the last character written on this line
            0x08 => {
                let trim_to = out
                    .char_indices()
                    .rev()
                    .find(|(_, c)| *c != '\n')
                    .map(|(idx, _)| idx);
                if let Some(t) = trim_to {
                    out.truncate(t);
                }
                i += 1;
            },
            // Carriage return without newline: reset to start of line
            0x0d if i + 1 < bytes.len() && bytes[i + 1] != 0x0a => {
                if let Some(nl) = out.rfind('\n') {
                    out.truncate(nl + 1);
                } else {
                    out.clear();
                }
                i += 1;
            },
            // Normal byte: append as-is
            _ => {
                // Preserve multi-byte UTF-8 sequences intact
                let ch_len = if bytes[i] < 0x80 {
                    1
                } else if bytes[i] < 0xE0 {
                    2
                } else if bytes[i] < 0xF0 {
                    3
                } else {
                    4
                };
                let end = (i + ch_len).min(bytes.len());
                if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
                    out.push_str(s);
                    i = end;
                } else {
                    i += 1;
                }
            },
        }
    }
    out
}

/// Map a multipart/body read error to a user-facing message.
/// `limit_mb` is the configured limit for this endpoint (for display only).
fn classify_upload_error(err: &str, limit_mb: usize) -> String {
    let lower = err.to_lowercase();
    if lower.contains("length limit")
        || lower.contains("too large")
        || lower.contains("size exceeded")
        || lower.contains("payload too large")
        || lower.contains("body limit")
    {
        format!(
            "File too large. Maximum upload size is {} MB. Please compress the file and try again.",
            limit_mb
        )
    } else {
        format!("Failed to parse uploaded file: {}", err)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::extract::{FromRequest, State};
    use axum::http::{Request, StatusCode, header};
    use axum::response::IntoResponse;

    use super::*;
    use crate::test_support::{app_state, authorization_headers};

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("aether-api-config-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create isolated test directory");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn explicit_config_directory_wins_even_before_it_exists() {
        let root = TestDirectory::new();
        let explicit = root.path().join("operator-selected");
        let existing = root.path().join("existing");
        fs::create_dir_all(&existing).expect("create fallback config directory");

        let selected = select_config_directory(Some(explicit.clone()), &[existing]);

        assert_eq!(selected, Some(explicit));
    }

    #[test]
    fn first_existing_config_directory_is_selected() {
        let root = TestDirectory::new();
        let missing = root.path().join("missing");
        let existing = root.path().join("existing");
        fs::create_dir_all(&existing).expect("create fallback config directory");

        let selected = select_config_directory(
            None,
            &[missing, existing.clone(), root.path().join("later")],
        );

        assert_eq!(selected, Some(existing));
    }

    #[test]
    fn config_export_never_includes_sibling_runtime_data() {
        let root = TestDirectory::new();
        let config = root.path().join("config");
        fs::create_dir_all(config.join("io")).expect("create config tree");
        fs::write(config.join("global.yaml"), "service: aether\n").expect("write global config");
        fs::write(config.join("io/io.yaml"), "channels: []\n").expect("write io config");
        fs::write(root.path().join("aether.db"), b"not configuration")
            .expect("write sibling database");
        fs::write(root.path().join("private.pem"), b"secret").expect("write sibling secret");

        let data = create_zip_archive(&config).expect("archive config tree");
        let mut archive = zip::ZipArchive::new(io::Cursor::new(data)).expect("read archive");
        let mut names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .expect("read archive entry")
                    .name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(names, vec!["global.yaml", "io/", "io/io.yaml"]);
    }

    #[cfg(unix)]
    #[test]
    fn config_export_rejects_symbolic_links_instead_of_following_them() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        let config = root.path().join("config");
        fs::create_dir_all(&config).expect("create config directory");
        let outside = root.path().join("outside-secret");
        fs::write(&outside, b"secret").expect("write outside secret");
        symlink(&outside, config.join("linked-secret")).expect("create config symlink");

        let error = create_zip_archive(&config).expect_err("symlink must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn unsafe_remote_config_mutation_is_explicitly_disabled() {
        let state = app_state().await;
        let admin = authorization_headers("Admin");
        for response in [
            import_config(State(Arc::clone(&state)), admin.clone()).await,
            restart_services(State(Arc::clone(&state)), admin.clone()).await,
        ] {
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

            let body = to_bytes(response.into_body(), 16 * 1024)
                .await
                .expect("read disabled response");
            let payload: serde_json::Value =
                serde_json::from_slice(&body).expect("parse disabled response");

            assert_eq!(payload["success"], false);
            assert!(
                payload["message"]
                    .as_str()
                    .expect("message is a string")
                    .contains("aether sync")
            );
        }
    }

    #[tokio::test]
    async fn every_config_management_endpoint_rejects_viewers() {
        let state = app_state().await;
        let viewer = authorization_headers("Viewer");

        let responses = [
            check_config(State(Arc::clone(&state)), viewer.clone())
                .await
                .into_response(),
            export_config(State(Arc::clone(&state)), viewer.clone())
                .await
                .into_response(),
            import_config(State(Arc::clone(&state)), viewer.clone()).await,
            restart_services(State(Arc::clone(&state)), viewer.clone()).await,
            abort_upgrade(State(Arc::clone(&state)), viewer.clone())
                .await
                .into_response(),
            upgrade_status(State(Arc::clone(&state)), viewer.clone())
                .await
                .into_response(),
        ];
        for response in responses {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let boundary = "aether-rbac-test-boundary";
        let request = Request::builder()
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(format!("--{boundary}--\r\n")))
            .expect("build multipart request");
        let multipart = Multipart::from_request(request, &())
            .await
            .expect("parse multipart test request");
        let response = start_upgrade(State(state), viewer, multipart)
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unsigned_remote_upgrade_is_disabled_even_for_admins() {
        let state = app_state().await;
        let boundary = "aether-disabled-upgrade-boundary";
        let request = Request::builder()
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(format!("--{boundary}--\r\n")))
            .expect("build multipart request");
        let multipart = Multipart::from_request(request, &())
            .await
            .expect("parse multipart test request");

        let response = start_upgrade(
            State(Arc::clone(&state)),
            authorization_headers("Admin"),
            multipart,
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let abort_response = abort_upgrade(State(state), authorization_headers("Admin")).await;
        assert_eq!(abort_response.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
