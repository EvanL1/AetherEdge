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
            api_port: common::env_or("API_PORT", 6004),
            shm_path: shm_path.to_string_lossy().into_owned(),
            channel_health_shm_path: channel_health_shm_path.to_string_lossy().into_owned(),
            shm_writer_stale_after_ms: common::env_or("SHM_WRITER_STALE_AFTER_MS", 30_000),
            shm_identity_check_interval_ms: common::env_or("SHM_IDENTITY_CHECK_INTERVAL_MS", 250),
            shm_topology_refresh_interval_ms: common::env_or(
                "SHM_TOPOLOGY_REFRESH_INTERVAL_MS",
                1_000,
            ),
            db_path,
            history_db_path,
        }
    }
}

impl EnvConfig {
    pub fn api_bind_address(&self) -> anyhow::Result<std::net::SocketAddr> {
        common::loopback_bind_address(&self.api_host, self.api_port)
            .map_err(|error| anyhow::anyhow!("invalid internal history API bind address: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::EnvConfig;

    #[test]
    fn internal_api_bind_accepts_ipv4_and_ipv6_loopback() {
        let mut config = EnvConfig::default();
        for host in ["127.0.0.1", "127.88.1.3", "::1"] {
            config.api_host = host.to_owned();
            assert!(config.api_bind_address().is_ok(), "{host} must be accepted");
        }
    }

    #[test]
    fn internal_api_bind_rejects_unspecified_addresses() {
        let mut config = EnvConfig::default();
        for host in ["0.0.0.0", "::"] {
            config.api_host = host.to_owned();
            assert!(
                config.api_bind_address().is_err(),
                "{host} must not expose aether-history"
            );
        }
    }
}
