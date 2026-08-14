//! Service configuration loaded from environment variables

use std::env;

#[derive(Clone)]
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
    /// Credential accepted only by API's internal alarm-event ingress.
    pub api_broadcast_token: String,
    /// Durable alarm notification dispatcher poll interval.
    pub notification_outbox_poll_interval_ms: u64,
    /// Initial retry delay for failed notification destinations.
    pub notification_retry_initial_ms: u64,
    /// Maximum retry delay for failed notification destinations.
    pub notification_retry_max_ms: u64,
    /// Maximum best-effort outbox drain time during shutdown.
    pub notification_shutdown_drain_ms: u64,
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
            api_broadcast_token: env::var("AETHER_ALARM_BROADCAST_TOKEN").unwrap_or_default(),
            notification_outbox_poll_interval_ms: common::env_or(
                "ALARM_NOTIFICATION_OUTBOX_POLL_INTERVAL_MS",
                250,
            ),
            notification_retry_initial_ms: common::env_or(
                "ALARM_NOTIFICATION_RETRY_INITIAL_MS",
                500,
            ),
            notification_retry_max_ms: common::env_or("ALARM_NOTIFICATION_RETRY_MAX_MS", 30_000),
            notification_shutdown_drain_ms: common::env_or(
                "ALARM_NOTIFICATION_SHUTDOWN_DRAIN_MS",
                2_000,
            ),
        }
    }
}

impl AlarmConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_broadcast_token = env::var("AETHER_ALARM_BROADCAST_TOKEN")
            .map_err(|_| anyhow::anyhow!("AETHER_ALARM_BROADCAST_TOKEN is required"))?;
        validate_api_broadcast_token(&api_broadcast_token)?;
        let jwt_secret = env::var("JWT_SECRET_KEY")
            .map_err(|_| anyhow::anyhow!("JWT_SECRET_KEY is required"))?;
        validate_distinct_credentials(&jwt_secret, &api_broadcast_token)?;
        let mut config = Self {
            api_broadcast_token,
            ..Self::default()
        };
        validate_internal_service_url("AETHER_API_URL", &config.api_url)?;
        validate_internal_service_url("AETHER_UPLINK_URL", &config.uplink_url)?;
        config.api_url = config.api_url.trim_end_matches('/').to_owned();
        config.uplink_url = config.uplink_url.trim_end_matches('/').to_owned();
        Ok(config)
    }

    pub fn api_bind_address(&self) -> anyhow::Result<std::net::SocketAddr> {
        common::loopback_bind_address(&self.api_host, self.api_port)
            .map_err(|error| anyhow::anyhow!("invalid internal alarm API bind address: {error}"))
    }
}

fn validate_api_broadcast_token(token: &str) -> anyhow::Result<()> {
    if token.len() < 32
        || token.trim() != token
        || token
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        anyhow::bail!(
            "AETHER_ALARM_BROADCAST_TOKEN must contain at least 32 bytes without surrounding whitespace or control characters"
        );
    }
    if matches!(token, "change-me-in-production" | "your-service-token-here") {
        anyhow::bail!("AETHER_ALARM_BROADCAST_TOKEN must not use a documented placeholder");
    }
    Ok(())
}

fn validate_distinct_credentials(jwt_secret: &str, alarm_token: &str) -> anyhow::Result<()> {
    use sha2::Digest as _;
    use subtle::ConstantTimeEq as _;

    let same = sha2::Sha256::digest(jwt_secret.as_bytes())
        .ct_eq(&sha2::Sha256::digest(alarm_token.as_bytes()))
        .unwrap_u8()
        == 1;
    if same {
        anyhow::bail!("AETHER_ALARM_BROADCAST_TOKEN must be distinct from JWT_SECRET_KEY");
    }
    Ok(())
}

fn validate_internal_service_url(name: &str, value: &str) -> anyhow::Result<()> {
    let url =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("{name} must be a valid URL"))?;
    let loopback_host = matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
    );
    if url.scheme() != "http"
        || !loopback_host
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("{name} must be an origin-only HTTP URL on an explicit loopback port");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AlarmConfig, validate_api_broadcast_token, validate_distinct_credentials,
        validate_internal_service_url,
    };

    #[test]
    fn dedicated_api_broadcast_token_must_be_strong() {
        assert!(validate_api_broadcast_token("").is_err());
        assert!(validate_api_broadcast_token("change-me-in-production").is_err());
        assert!(validate_api_broadcast_token(" alarm-service-token-0123456789abcdef ").is_err());
        assert!(validate_api_broadcast_token("alarm-service-token-0123456789 abcdef").is_err());
        assert!(validate_api_broadcast_token("alarm-service-token-0123456789abcdef").is_ok());
        assert!(validate_distinct_credentials("same-token", "same-token").is_err());
        assert!(validate_distinct_credentials("jwt-token", "alarm-token").is_ok());
    }

    #[test]
    fn notification_credentials_can_only_be_sent_to_loopback_origins() {
        for allowed in [
            "http://127.0.0.1:6005",
            "http://localhost:6006",
            "http://[::1]:6005",
        ] {
            assert!(validate_internal_service_url("TEST_URL", allowed).is_ok());
        }
        for rejected in [
            "https://127.0.0.1:6005",
            "http://192.0.2.10:6005",
            "http://attacker.invalid:6005",
            "http://127.0.0.1:6005/path",
            "http://user:password@127.0.0.1:6005",
            "http://127.0.0.1",
        ] {
            assert!(validate_internal_service_url("TEST_URL", rejected).is_err());
        }
    }

    #[test]
    fn internal_api_bind_accepts_ipv4_and_ipv6_loopback() {
        let mut config = AlarmConfig::default();
        for host in ["127.0.0.1", "127.66.1.5", "::1"] {
            config.api_host = host.to_owned();
            assert!(config.api_bind_address().is_ok(), "{host} must be accepted");
        }
    }

    #[test]
    fn internal_api_bind_rejects_unspecified_addresses() {
        let mut config = AlarmConfig::default();
        for host in ["0.0.0.0", "::"] {
            config.api_host = host.to_owned();
            assert!(
                config.api_bind_address().is_err(),
                "{host} must not expose aether-alarm"
            );
        }
    }
}
