//! Service configuration loaded from environment variables

use std::env;

#[derive(Debug, Clone)]
pub struct AlarmConfig {
    pub api_host: String,
    pub api_port: u16,
    pub shm_path: String,
    pub channel_health_shm_path: String,
    pub point_watch_socket: String,
    pub point_watch_debounce_ms: u64,
    pub shm_writer_stale_after_ms: u64,
    pub shm_identity_check_interval_ms: u64,
    pub shm_topology_refresh_interval_ms: u64,
    pub db_path: String,
    /// Monitoring check interval in seconds
    pub data_fetch_interval: u64,
    pub api_url: String,
    pub uplink_url: String,
}

impl Default for AlarmConfig {
    fn default() -> Self {
        let shm_path = aether_shm_bridge::default_shm_path();
        let channel_health_shm_path = env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| aether_shm_bridge::channel_health_path_from_shm(&shm_path));
        Self {
            api_host: env::var("API_HOST").unwrap_or_else(|_| common::DEFAULT_API_HOST.to_string()),
            api_port: common::env_or("SERVICE_PORT", 6007),
            shm_path: shm_path.to_string_lossy().into_owned(),
            channel_health_shm_path: channel_health_shm_path.to_string_lossy().into_owned(),
            point_watch_socket: env::var("AETHER_ALARM_POINT_WATCH_SOCKET").unwrap_or_else(|_| {
                aether_shm_bridge::point_watch_socket_from_shm(&shm_path, "alarm")
                    .to_string_lossy()
                    .into_owned()
            }),
            point_watch_debounce_ms: common::env_or("POINT_WATCH_DEBOUNCE_MS", 25),
            shm_writer_stale_after_ms: common::env_or("SHM_WRITER_STALE_AFTER_MS", 30_000),
            shm_identity_check_interval_ms: common::env_or("SHM_IDENTITY_CHECK_INTERVAL_MS", 250),
            shm_topology_refresh_interval_ms: common::env_or(
                "SHM_TOPOLOGY_REFRESH_INTERVAL_MS",
                1_000,
            ),
            db_path: env::var("AETHER_DB_PATH")
                .unwrap_or_else(|_| "/app/data/aether.db".to_string()),
            data_fetch_interval: common::env_or("DATA_FETCH_INTERVAL", 5),
            api_url: env::var("AETHER_API_URL")
                .unwrap_or_else(|_| "http://localhost:6005".to_string()),
            uplink_url: env::var("AETHER_UPLINK_URL")
                .unwrap_or_else(|_| "http://localhost:6006".to_string()),
        }
    }
}
