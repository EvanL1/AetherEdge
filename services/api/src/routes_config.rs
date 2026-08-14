use std::io::{self, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::io::ReaderStream;
use tracing::{error, info};

use crate::auth::Claims;
use crate::routes_auth::require_admin;
use crate::state::AppState;

const CONFIG_PATH_ENV: &str = "AETHER_CONFIG_PATH";
const MAX_CONFIG_ARCHIVE_ENTRIES: usize = 4_096;
const MAX_CONFIG_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
static CONFIG_EXPORT_SLOT: Semaphore = Semaphore::const_new(1);

fn config_directory_from(value: Option<std::ffi::OsString>) -> io::Result<PathBuf> {
    let value = value.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{CONFIG_PATH_ENV} is required"),
        )
    })?;
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{CONFIG_PATH_ENV} must not be empty"),
        ));
    }
    Ok(PathBuf::from(value))
}

/// Resolve the one composition-selected static-configuration tree.
fn config_directory() -> io::Result<PathBuf> {
    config_directory_from(std::env::var_os(CONFIG_PATH_ENV))
}

fn config_directory_error(error: &io::Error) -> Response {
    error!("Configuration directory is not configured: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "success": false,
            "message": format!("Configuration directory is unavailable: {error}")
        })),
    )
        .into_response()
}

fn require_config_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Claims, (StatusCode, Json<serde_json::Value>)> {
    require_admin(state, headers)
}

// ── GET /api/config/check ─────────────────────────────────────────────────────

/// Check the health of the configuration directory.
///
/// Reports whether the selected `config/` directory exists and lists its
/// immediate entries. This lightweight probe does not parse files, validate
/// completeness, or compare them with SQLite. **Read-only; Admin only.**
#[utoipa::path(get, path = "/api/config/check", tag = "Config",
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

    let dir = match config_directory() {
        Ok(dir) => dir,
        Err(error) => return config_directory_error(&error),
    };
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

// ── GET /api/config/export ────────────────────────────────────────────────────

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
#[utoipa::path(get, path = "/api/config/export", tag = "Config",
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

    let dir = match config_directory() {
        Ok(dir) => dir,
        Err(error) => return config_directory_error(&error),
    };
    if !dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "message": "Config directory not found"})),
        )
            .into_response();
    }

    let export_permit = match CONFIG_EXPORT_SLOT.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "success": false,
                    "message": "Another configuration export is already running"
                })),
            )
                .into_response();
        },
    };
    let archive = tokio::task::spawn_blocking(move || {
        // A cancelled download request does not cancel `spawn_blocking`.
        // Retain the permit inside the worker so another export cannot begin
        // while the abandoned compression job is still consuming CPU and I/O.
        let _permit = export_permit;
        create_zip_archive(&dir)
    })
    .await;

    match archive {
        Ok(Ok(file)) => {
            let filename = format!("config_{}.zip", chrono::Utc::now().format("%Y%m%d_%H%M%S"));
            let archive_size = match file.metadata() {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    error!("Read export archive metadata error: {error}");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(
                            json!({"success": false, "message": "Failed to export configuration"}),
                        ),
                    )
                        .into_response();
                },
            };
            let stream = ReaderStream::new(tokio::fs::File::from_std(file));
            match Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/zip")
                .header(header::CONTENT_LENGTH, archive_size)
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                )
                .body(Body::from_stream(stream))
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
        Ok(Err(e)) => {
            error!("Export config error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Failed to export configuration"})),
            )
                .into_response()
        },
        Err(error) => {
            error!("Export config worker failed: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Failed to export configuration"})),
            )
                .into_response()
        },
    }
}

fn create_zip_archive(dir: &Path) -> io::Result<std::fs::File> {
    let mut zip = zip::ZipWriter::new(tempfile::tempfile()?);

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

    let mut file = zip.finish()?;
    file.seek(io::SeekFrom::Start(0))?;
    Ok(file)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

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
    fn configured_directory_is_used_even_before_it_exists() {
        let root = TestDirectory::new();
        let explicit = root.path().join("operator-selected");

        let selected = config_directory_from(Some(explicit.clone().into_os_string()))
            .expect("configured path");

        assert_eq!(selected, explicit);
    }

    #[test]
    fn missing_or_empty_config_directory_configuration_fails_closed() {
        let missing = config_directory_from(None).expect_err("missing variable must fail");
        assert_eq!(missing.kind(), io::ErrorKind::InvalidInput);
        assert!(missing.to_string().contains(CONFIG_PATH_ENV));

        let empty = config_directory_from(Some(std::ffi::OsString::new()))
            .expect_err("empty variable must fail");
        assert_eq!(empty.kind(), io::ErrorKind::InvalidInput);
        assert!(empty.to_string().contains("must not be empty"));
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
        let mut archive = zip::ZipArchive::new(data).expect("read archive");
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
}
