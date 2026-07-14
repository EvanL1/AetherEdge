use std::env;

/// Static configuration loaded from environment variables at startup.
/// Dynamic per-service settings (intervals, patterns, storage backend, etc.)
/// live in the `history_config` table in the shared SQLite database – see
/// `db_config.rs`.  Storage backend can be configured and toggled at runtime
/// via the `PUT /hisApi/storage` API endpoint.
#[derive(Debug, Clone)]
pub struct EnvConfig {
    pub api_host: String,
    pub api_port: u16,
    pub shm_path: String,
    pub channel_health_shm_path: String,
    pub shm_writer_stale_after_ms: u64,
    pub shm_identity_check_interval_ms: u64,
    pub shm_topology_refresh_interval_ms: u64,
    /// Shared SQLite database path (same as alarm / api).
    pub db_path: String,
    /// Embedded historical database used by the zero-dependency profile.
    pub history_db_path: String,
}

impl Default for EnvConfig {
    fn default() -> Self {
        let shm_path = aether_shm_bridge::default_shm_path();
        let channel_health_shm_path = env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| aether_shm_bridge::channel_health_path_from_shm(&shm_path));
        let db_path =
            env::var("AETHER_DB_PATH").unwrap_or_else(|_| "/app/data/aether.db".to_string());
        let history_db_path = env::var("AETHER_HISTORY_DB_PATH").unwrap_or_else(|_| {
            std::path::Path::new(&db_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("aether-history.db")
                .to_string_lossy()
                .into_owned()
        });

        Self {
            api_host: env::var("API_HOST").unwrap_or_else(|_| common::DEFAULT_API_HOST.to_string()),
            api_port: env::var("API_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6004),
            shm_path: shm_path.to_string_lossy().into_owned(),
            channel_health_shm_path: channel_health_shm_path.to_string_lossy().into_owned(),
            shm_writer_stale_after_ms: env::var("SHM_WRITER_STALE_AFTER_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(30_000),
            shm_identity_check_interval_ms: env::var("SHM_IDENTITY_CHECK_INTERVAL_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(250),
            shm_topology_refresh_interval_ms: env::var("SHM_TOPOLOGY_REFRESH_INTERVAL_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_000),
            db_path,
            history_db_path,
        }
    }
}
