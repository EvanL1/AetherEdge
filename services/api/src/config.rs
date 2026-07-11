use std::env;

pub struct GatewayConfig {
    pub api_host: String,
    pub api_port: u16,
    pub shm_path: String,
    pub channel_health_shm_path: String,
    pub shm_writer_stale_after_ms: u64,
    pub shm_identity_check_interval_ms: u64,
    pub point_watch_socket: String,
    pub point_watch_debounce_ms: u64,
    pub db_path: String,
    pub jwt_secret: String,
    pub access_token_expire_minutes: i64,
    pub refresh_token_expire_days: i64,
    pub allow_public_registration: bool,
    pub network_config_dir: String,
    pub data_fetch_interval_secs: u64,
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

        Ok(Self {
            jwt_secret,
            ..Self::default()
        })
    }
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
    use super::{explicit_opt_in, validate_jwt_secret};

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
}
