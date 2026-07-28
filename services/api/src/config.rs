use std::env;

#[derive(Clone)]
pub struct GatewayConfig {
    pub api_host: String,
    pub api_port: u16,
    pub shm_path: String,
    pub channel_health_shm_path: String,
    pub shm_writer_stale_after_ms: u64,
    pub shm_identity_check_interval_ms: u64,
    pub shm_topology_refresh_interval_ms: u64,
    pub point_watch_socket: String,
    pub point_watch_debounce_ms: u64,
    pub db_path: String,
    pub jwt_secret: String,
    pub access_token_expire_minutes: i64,
    pub refresh_token_expire_days: i64,
    pub allow_public_registration: bool,
    pub network_config_dir: String,
    pub data_fetch_interval_secs: u64,
    pub io_service_url: String,
    pub automation_service_url: String,
    pub history_service_url: String,
    pub uplink_service_url: String,
    pub alarm_service_url: String,
    pub service_request_timeout_secs: u64,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        let db_path =
            env::var("AETHER_DB_PATH").unwrap_or_else(|_| "/app/data/aether.db".to_string());
        let shm_path = aether_shm_bridge::default_shm_path();
        let channel_health_shm_path = env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| aether_shm_bridge::channel_health_path_from_shm(&shm_path));

        Self {
            api_host: env::var("API_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            api_port: env::var("API_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6005),
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
            point_watch_socket: env::var("AETHER_API_POINT_WATCH_SOCKET").unwrap_or_else(|_| {
                aether_shm_bridge::point_watch_socket_from_shm(&shm_path, "api")
                    .to_string_lossy()
                    .into_owned()
            }),
            point_watch_debounce_ms: env::var("POINT_WATCH_DEBOUNCE_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(25),
            db_path,
            jwt_secret: env::var("JWT_SECRET_KEY").unwrap_or_default(),
            access_token_expire_minutes: env::var("ACCESS_TOKEN_EXPIRE_MINUTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            refresh_token_expire_days: env::var("REFRESH_TOKEN_EXPIRE_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            allow_public_registration: env::var("AETHER_ALLOW_PUBLIC_REGISTRATION")
                .ok()
                .is_some_and(|value| explicit_opt_in(&value)),
            network_config_dir: env::var("NETWORK_CONFIG_DIR")
                .unwrap_or_else(|_| "/etc/systemd/network".to_string()),
            data_fetch_interval_secs: env::var("DATA_FETCH_INTERVAL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            io_service_url: env::var("AETHER_IO_SERVICE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6001".to_string()),
            automation_service_url: env::var("AETHER_AUTOMATION_SERVICE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6002".to_string()),
            history_service_url: env::var("AETHER_HISTORY_SERVICE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6004".to_string()),
            uplink_service_url: env::var("AETHER_UPLINK_SERVICE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6006".to_string()),
            alarm_service_url: env::var("AETHER_ALARM_SERVICE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6007".to_string()),
            service_request_timeout_secs: env::var("AETHER_SERVICE_REQUEST_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(60),
        }
    }
}

fn explicit_opt_in(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl GatewayConfig {
    /// Loads configuration and rejects a missing or weak JWT signing secret.
    pub fn from_env() -> anyhow::Result<Self> {
        let jwt_secret = env::var("JWT_SECRET_KEY")
            .map_err(|_| anyhow::anyhow!("JWT_SECRET_KEY is required"))?;
        validate_jwt_secret(&jwt_secret).map_err(anyhow::Error::msg)?;

        let config = Self {
            jwt_secret,
            ..Self::default()
        };
        for (name, value) in [
            ("AETHER_IO_SERVICE_URL", config.io_service_url.as_str()),
            (
                "AETHER_AUTOMATION_SERVICE_URL",
                config.automation_service_url.as_str(),
            ),
            (
                "AETHER_HISTORY_SERVICE_URL",
                config.history_service_url.as_str(),
            ),
            (
                "AETHER_UPLINK_SERVICE_URL",
                config.uplink_service_url.as_str(),
            ),
            (
                "AETHER_ALARM_SERVICE_URL",
                config.alarm_service_url.as_str(),
            ),
        ] {
            validate_internal_service_url(value)
                .map_err(|message| anyhow::anyhow!("{name}: {message}"))?;
        }
        Ok(config)
    }
}

fn validate_internal_service_url(value: &str) -> Result<(), &'static str> {
    let url = reqwest::Url::parse(value).map_err(|_| "must be a valid URL")?;
    let loopback_host = matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
    );
    if url.scheme() != "http"
        || !loopback_host
        || url.port().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("must be an origin-only HTTP URL on an explicit loopback port");
    }
    Ok(())
}

fn validate_jwt_secret(secret: &str) -> Result<(), &'static str> {
    if secret.len() < 32 {
        return Err("JWT_SECRET_KEY must contain at least 32 bytes");
    }
    if matches!(
        secret,
        "change-me-in-production" | "your-secret-key-here-change-in-production"
    ) {
        return Err("JWT_SECRET_KEY must not use a documented placeholder");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{explicit_opt_in, validate_internal_service_url, validate_jwt_secret};

    #[test]
    fn jwt_secret_must_be_at_least_256_bits() {
        assert!(validate_jwt_secret("").is_err());
        assert!(validate_jwt_secret("change-me-in-production").is_err());
        assert!(validate_jwt_secret("0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn public_registration_requires_an_explicit_true_value() {
        for enabled in ["1", "true", "TRUE", "yes", "on"] {
            assert!(explicit_opt_in(enabled));
        }
        for disabled in ["", "0", "false", "no", "invalid"] {
            assert!(!explicit_opt_in(disabled));
        }
    }

    #[test]
    fn internal_service_urls_are_loopback_origins_only() {
        for allowed in [
            "http://127.0.0.1:6001",
            "http://localhost:6002",
            "http://[::1]:6004",
        ] {
            assert!(validate_internal_service_url(allowed).is_ok(), "{allowed}");
        }
        for rejected in [
            "https://127.0.0.1:6001",
            "http://192.168.30.62:6001",
            "http://attacker.invalid:6001",
            "http://127.0.0.1:6001/api",
            "http://user:password@127.0.0.1:6001",
            "http://127.0.0.1",
        ] {
            assert!(
                validate_internal_service_url(rejected).is_err(),
                "{rejected}"
            );
        }
    }
}
