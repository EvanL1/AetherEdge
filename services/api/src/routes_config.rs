use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

// ── GET /api/v1/config/check ──────────────────────────────────────────────────

/// Check the health of the configuration directory.
///
/// Reports whether the selected `config/` directory exists and lists its
/// immediate entries. This lightweight probe does not parse files, validate
/// completeness, or compare them with SQLite. **Read-only; Admin only.**
#[utoipa::path(get, path = "/api/v1/config/check", tag = "Config",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Configuration directory check result", body = crate::models::GatewayDataResponse<serde_json::Value>),
        (status = 401, description = "Missing, invalid, or expired access JWT"),
        (status = 403, description = "Admin privileges required"),
        (status = 500, description = "Configuration directory could not be read")
    ))]
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

#[allow(dead_code)] // OpenAPI-only binary response schema.
#[derive(utoipa::ToSchema)]
#[schema(value_type = String, format = Binary)]
pub(crate) struct ConfigArchive(Vec<u8>);

/// Export the current configuration as a ZIP archive.
///
/// Packages the entire `config/` directory tree (product definitions,
/// instances, routing, rules, etc.) into a ZIP stream returned as an
/// `attachment`. Use for site-to-site configuration migration, pre-upgrade
/// backups, and remote-support reproduction. The export includes only static
/// configuration files; live SHM state is intentionally excluded. **Admin only.**
#[utoipa::path(get, path = "/api/v1/config/export", tag = "Config",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "ZIP file stream", body = ConfigArchive, content_type = "application/zip"),
        (status = 401, description = "Missing, invalid, or expired access JWT"),
        (status = 403, description = "Admin privileges required"),
        (status = 404, description = "Configuration directory not found"),
        (status = 500, description = "Configuration archive could not be created")
    ))]
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
    responses(
        (status = 401, description = "Missing or invalid access token"),
        (status = 403, description = "Admin privileges required"),
        (status = 501, description = "Remote configuration import is disabled")
    ))]
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
    responses(
        (status = 401, description = "Missing or invalid access token"),
        (status = 403, description = "Admin privileges required"),
        (status = 501, description = "Remote service restart is disabled")
    ))]
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
    responses(
        (status = 401, description = "Missing or invalid access token"),
        (status = 403, description = "Admin privileges required"),
        (status = 501, description = "Unsigned remote upgrade is disabled")
    ))]
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

// ── POST /api/v1/config/upgrade/abort ────────────────────────────────────────

/// Remote upgrade mutation is disabled together with the upload endpoint.
#[utoipa::path(post, path = "/api/v1/config/upgrade/abort", tag = "Config",
    security(("bearer_auth" = [])),
    responses(
        (status = 401, description = "Missing or invalid access token"),
        (status = 403, description = "Admin privileges required"),
        (status = 501, description = "Unsigned remote upgrade is disabled")
    ))]
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

// ── GET /api/v1/config/upgrade/status ────────────────────────────────────────

/// Report remote upgrade state.
///
/// Remote upgrade is disabled, so no upgrade can be started through this API
/// and nothing in this distribution writes upgrade progress. The response
/// keeps the historical envelope shape and reports the only reachable state.
#[utoipa::path(get, path = "/api/v1/config/upgrade/status", tag = "Config",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Compatibility upgrade status", body = crate::models::GatewayDataResponse<serde_json::Value>),
        (status = 401, description = "Missing, invalid, or expired access JWT"),
        (status = 403, description = "Admin privileges required")
    ))]
pub async fn upgrade_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(response) = require_config_admin(&state, &headers) {
        return response.into_response();
    }
    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "running": false,
            "pid": null,
            "log": "",
            "detail": {"status": "idle"},
            "upload": {"received_bytes": 0, "total_bytes": 0, "progress_pct": null}
        }
    }))
    .into_response()
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
