use std::env;

/// Static configuration from environment variables.
/// All runtime settings (MQTT broker, topics, intervals, etc.) are stored
/// in the shared SQLite `uplink_config` table – see `db_config.rs`.
pub struct EnvConfig {
    pub api_host: String,
    pub api_port: u16,
    pub shm_path: String,
    pub shm_writer_stale_after_ms: u64,
    pub shm_identity_check_interval_ms: u64,
    /// Shared SQLite database path (same as alarm / api).
    pub db_path: String,
    /// Crash-recoverable uplink queue journal.
    pub outbox_path: String,
    /// Maximum number of pending uplink messages retained locally.
    pub outbox_capacity: usize,
    /// Credential used only for authenticated device-control calls to automation.
    pub control_token: Option<String>,
    /// Directory for TLS certificate files. Fixed at container build time;
    /// set via CERT_DIR env var (default: /app/config/cert).
    /// Mount a host path to this directory in docker-compose.
    pub cert_dir: String,
}

impl Default for EnvConfig {
    fn default() -> Self {
        let shm_path = aether_shm_bridge::default_shm_path();

        Self {
            api_host: env::var("API_HOST").unwrap_or_else(|_| common::DEFAULT_API_HOST.to_string()),
            api_port: env::var("API_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6006),
            shm_path: shm_path.to_string_lossy().into_owned(),
            shm_writer_stale_after_ms: env::var("SHM_WRITER_STALE_AFTER_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(30_000),
            shm_identity_check_interval_ms: env::var("SHM_IDENTITY_CHECK_INTERVAL_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(250),
            db_path: env::var("AETHER_DB_PATH")
                .unwrap_or_else(|_| "/app/data/aether.db".to_string()),
            outbox_path: env::var("AETHER_UPLINK_OUTBOX_PATH")
                .unwrap_or_else(|_| "/app/data/uplink.outbox".to_string()),
            outbox_capacity: env::var("AETHER_UPLINK_OUTBOX_CAPACITY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(10_000),
            control_token: env::var("AETHER_UPLINK_CONTROL_TOKEN")
                .ok()
                .filter(|value| value.len() >= 32 && value.trim() == value),
            cert_dir: env::var("CERT_DIR").unwrap_or_else(|_| "/app/config/cert".to_string()),
        }
    }
}
