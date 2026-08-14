use std::env;
use std::net::IpAddr;
use std::path::PathBuf;

use aether_cloudlink_mqtt::{
    CloudLinkMqttConfig, CloudLinkTlsConfig, MqttClientIdentity, SecretString,
};
use aether_store_local::{
    CLOUDLINK_SPOOL_MIN_JOURNAL_HEADROOM_BYTES, DEFAULT_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
    DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES, MAX_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
    MAX_CLOUDLINK_SPOOL_MAX_LIVE_BYTES, MIN_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
};

const ALARM_BROADCAST_TOKEN_ENV: &str = "AETHER_ALARM_BROADCAST_TOKEN";
/// Static service configuration. CloudLink is the only cloud protocol surface.
pub struct EnvConfig {
    pub api_host: String,
    pub api_port: u16,
    pub shm_path: String,
    pub channel_health_shm_path: String,
    pub shm_writer_stale_after_ms: u64,
    pub shm_identity_check_interval_ms: u64,
    pub shm_topology_refresh_interval_ms: u64,
    pub db_path: String,
    pub spool_path: PathBuf,
    pub spool_capacity: usize,
    pub acknowledged_receipt_capacity: usize,
    pub spool_max_live_bytes: u64,
    pub spool_max_journal_bytes: u64,
    pub cloudlink: Option<CloudLinkSettings>,
}

/// Explicit CloudLink composition. There is no generic MQTT fallback.
pub struct CloudLinkSettings {
    pub broker_host: String,
    pub broker_port: u16,
    pub topic_prefix: String,
    pub username: Option<String>,
    pub password: Option<SecretString>,
    pub tls: CloudLinkTlsConfig,
    pub keep_alive_secs: u64,
    pub reconnect_delay_secs: u64,
    pub request_capacity: usize,
    pub identity_directory: PathBuf,
    pub cloud_key_id: String,
    pub cloud_verifying_key: String,
    pub credential_id: String,
    pub credential_generation: u64,
    pub challenge_ledger_path: PathBuf,
    pub challenge_ledger_capacity: usize,
    pub challenge_request_ttl_ms: u64,
    pub telemetry_interval_secs: u64,
    pub runtime_manifest_path: PathBuf,
}

impl core::fmt::Debug for CloudLinkSettings {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CloudLinkSettings")
            .field("broker_host", &self.broker_host)
            .field("broker_port", &self.broker_port)
            .field("topic_prefix", &self.topic_prefix)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("tls", &self.tls)
            .field("identity_directory", &self.identity_directory)
            .field("cloud_key_id", &self.cloud_key_id)
            .field("cloud_verifying_key", &"[REDACTED]")
            .field("credential_id", &self.credential_id)
            .field("credential_generation", &self.credential_generation)
            .finish_non_exhaustive()
    }
}

impl CloudLinkSettings {
    pub fn mqtt_config(&self, gateway_id: &str) -> CloudLinkMqttConfig {
        CloudLinkMqttConfig {
            broker_host: self.broker_host.clone(),
            broker_port: self.broker_port,
            client_id: gateway_id.to_owned(),
            username: self.username.clone(),
            password: self.password.clone(),
            tls: self.tls.clone(),
            keep_alive_secs: self.keep_alive_secs,
            reconnect_delay_secs: self.reconnect_delay_secs,
            request_capacity: self.request_capacity,
            maximum_packet_bytes: aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES,
        }
    }
}

impl EnvConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::default();
        let api_host = optional("API_HOST")?.unwrap_or(config.api_host);
        config.api_host = loopback_host(&api_host)?;
        config.api_port = bounded(
            "API_PORT",
            optional_parse("API_PORT", config.api_port)?,
            1,
            u16::MAX,
        )?;
        config.db_path = optional("AETHER_DB_PATH")?.unwrap_or(config.db_path);
        config.spool_path = optional("AETHER_CLOUDLINK_SPOOL_PATH")?
            .map(PathBuf::from)
            .unwrap_or(config.spool_path);
        config.spool_capacity = bounded(
            "AETHER_CLOUDLINK_SPOOL_CAPACITY",
            optional_parse("AETHER_CLOUDLINK_SPOOL_CAPACITY", 1_024)?,
            1,
            65_536,
        )?;
        config.acknowledged_receipt_capacity = bounded(
            "AETHER_CLOUDLINK_RECEIPT_CAPACITY",
            optional_parse("AETHER_CLOUDLINK_RECEIPT_CAPACITY", 100_000)?,
            1,
            1_000_000,
        )?;
        config.spool_max_live_bytes = bounded(
            "AETHER_CLOUDLINK_SPOOL_MAX_LIVE_BYTES",
            optional_parse(
                "AETHER_CLOUDLINK_SPOOL_MAX_LIVE_BYTES",
                DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
            )?,
            MIN_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
            MAX_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
        )?;
        config.spool_max_journal_bytes = bounded(
            "AETHER_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES",
            optional_parse(
                "AETHER_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES",
                DEFAULT_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
            )?,
            MIN_CLOUDLINK_SPOOL_MAX_LIVE_BYTES + CLOUDLINK_SPOOL_MIN_JOURNAL_HEADROOM_BYTES,
            MAX_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
        )?;
        validate_spool_byte_limits(config.spool_max_live_bytes, config.spool_max_journal_bytes)?;
        let shm_path = optional("AETHER_SHM_PATH")?
            .map(PathBuf::from)
            .unwrap_or_else(aether_shm_bridge::default_shm_path);
        config.shm_path = shm_path.to_string_lossy().into_owned();
        config.channel_health_shm_path = optional("AETHER_CHANNEL_HEALTH_SHM_PATH")?
            .map(PathBuf::from)
            .unwrap_or_else(|| aether_shm_bridge::channel_health_path_from_shm(&shm_path))
            .to_string_lossy()
            .into_owned();
        config.shm_writer_stale_after_ms = bounded(
            "SHM_WRITER_STALE_AFTER_MS",
            optional_parse("SHM_WRITER_STALE_AFTER_MS", 30_000)?,
            1,
            3_600_000,
        )?;
        config.shm_identity_check_interval_ms = bounded(
            "SHM_IDENTITY_CHECK_INTERVAL_MS",
            optional_parse("SHM_IDENTITY_CHECK_INTERVAL_MS", 250)?,
            1,
            60_000,
        )?;
        config.shm_topology_refresh_interval_ms = bounded(
            "SHM_TOPOLOGY_REFRESH_INTERVAL_MS",
            optional_parse("SHM_TOPOLOGY_REFRESH_INTERVAL_MS", 1_000)?,
            100,
            60_000,
        )?;
        config.cloudlink = match strict_bool("AETHER_CLOUDLINK_ENABLED", false)? {
            false => None,
            true => Some(CloudLinkSettings::from_env()?),
        };
        Ok(config)
    }
}

impl Default for EnvConfig {
    fn default() -> Self {
        let shm_path = aether_shm_bridge::default_shm_path();
        let channel_health_shm_path = aether_shm_bridge::channel_health_path_from_shm(&shm_path);
        Self {
            api_host: common::DEFAULT_API_HOST.to_owned(),
            api_port: 6006,
            shm_path: shm_path.to_string_lossy().into_owned(),
            channel_health_shm_path: channel_health_shm_path.to_string_lossy().into_owned(),
            shm_writer_stale_after_ms: 30_000,
            shm_identity_check_interval_ms: 250,
            shm_topology_refresh_interval_ms: 1_000,
            db_path: "/app/data/aether.db".to_owned(),
            spool_path: PathBuf::from("/app/data/cloudlink.spool"),
            spool_capacity: 1_024,
            acknowledged_receipt_capacity: 100_000,
            spool_max_live_bytes: DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
            spool_max_journal_bytes: DEFAULT_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
            cloudlink: None,
        }
    }
}

impl CloudLinkSettings {
    fn from_env() -> anyhow::Result<Self> {
        let identity_directory = optional("AETHER_GATEWAY_IDENTITY_DIR")?
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/app/data/uplink/identity"));
        let tls = tls_from_env()?;
        Ok(Self {
            broker_host: required("AETHER_CLOUDLINK_BROKER_HOST")?,
            broker_port: bounded(
                "AETHER_CLOUDLINK_BROKER_PORT",
                optional_parse("AETHER_CLOUDLINK_BROKER_PORT", 8_883)?,
                1,
                u16::MAX,
            )?,
            topic_prefix: optional("AETHER_CLOUDLINK_TOPIC_PREFIX")?
                .unwrap_or_else(|| "aether".to_owned()),
            username: optional_compose_value("AETHER_CLOUDLINK_BROKER_USERNAME")?,
            password: optional_compose_value("AETHER_CLOUDLINK_BROKER_PASSWORD")?
                .map(SecretString::new),
            tls,
            keep_alive_secs: bounded(
                "AETHER_CLOUDLINK_KEEP_ALIVE_SECS",
                optional_parse("AETHER_CLOUDLINK_KEEP_ALIVE_SECS", 30)?,
                5,
                3_600,
            )?,
            reconnect_delay_secs: bounded(
                "AETHER_CLOUDLINK_RECONNECT_DELAY_SECS",
                optional_parse("AETHER_CLOUDLINK_RECONNECT_DELAY_SECS", 5)?,
                1,
                3_600,
            )?,
            request_capacity: bounded(
                "AETHER_CLOUDLINK_REQUEST_CAPACITY",
                optional_parse("AETHER_CLOUDLINK_REQUEST_CAPACITY", 64)?,
                1,
                4_096,
            )?,
            challenge_ledger_path: identity_directory.join("challenge-ledger.json"),
            identity_directory,
            cloud_key_id: required("AETHER_CLOUDLINK_CLOUD_KEY_ID")?,
            cloud_verifying_key: required("AETHER_CLOUDLINK_CLOUD_VERIFYING_KEY")?,
            credential_id: required("AETHER_CLOUDLINK_CREDENTIAL_ID")?,
            credential_generation: bounded(
                "AETHER_CLOUDLINK_CREDENTIAL_GENERATION",
                required_parse("AETHER_CLOUDLINK_CREDENTIAL_GENERATION")?,
                1,
                u64::MAX,
            )?,
            challenge_ledger_capacity: bounded(
                "AETHER_CLOUDLINK_CHALLENGE_LEDGER_CAPACITY",
                optional_parse("AETHER_CLOUDLINK_CHALLENGE_LEDGER_CAPACITY", 64)?,
                1,
                256,
            )?,
            challenge_request_ttl_ms: bounded(
                "AETHER_CLOUDLINK_CHALLENGE_REQUEST_TTL_MS",
                optional_parse("AETHER_CLOUDLINK_CHALLENGE_REQUEST_TTL_MS", 60_000)?,
                1_000,
                600_000,
            )?,
            telemetry_interval_secs: bounded(
                "AETHER_CLOUDLINK_TELEMETRY_INTERVAL_SECS",
                optional_parse("AETHER_CLOUDLINK_TELEMETRY_INTERVAL_SECS", 30)?,
                1,
                86_400,
            )?,
            runtime_manifest_path: optional("AETHER_RUNTIME_MANIFEST_PATH")?
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/app/config/runtime-manifest.json")),
        })
    }
}

fn tls_from_env() -> anyhow::Result<CloudLinkTlsConfig> {
    let ca_path = optional_compose_value("AETHER_CLOUDLINK_BROKER_CA")?.map(PathBuf::from);
    let certificate_path =
        optional_compose_value("AETHER_CLOUDLINK_BROKER_CLIENT_CERT")?.map(PathBuf::from);
    let private_key_path =
        optional_compose_value("AETHER_CLOUDLINK_BROKER_CLIENT_KEY")?.map(PathBuf::from);
    match (ca_path, certificate_path, private_key_path) {
        (None, None, None) => Ok(CloudLinkTlsConfig::SystemRoots),
        (Some(ca_path), None, None) => Ok(CloudLinkTlsConfig::Custom {
            ca_path,
            client_identity: None,
        }),
        (Some(ca_path), Some(certificate_path), Some(private_key_path)) => {
            Ok(CloudLinkTlsConfig::Custom {
                ca_path,
                client_identity: Some(MqttClientIdentity {
                    certificate_path,
                    private_key_path,
                }),
            })
        },
        _ => anyhow::bail!(
            "AETHER_CLOUDLINK_BROKER_CA and the complete client certificate/key pair must be configured together"
        ),
    }
}

pub fn alarm_broadcast_token_from_env() -> anyhow::Result<String> {
    let token = env::var(ALARM_BROADCAST_TOKEN_ENV)
        .map_err(|_| anyhow::anyhow!("{ALARM_BROADCAST_TOKEN_ENV} is required"))?;
    validate_alarm_broadcast_token(&token)?;
    Ok(token)
}

fn validate_alarm_broadcast_token(token: &str) -> anyhow::Result<()> {
    if token.len() < 32
        || token.trim() != token
        || token
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        anyhow::bail!(
            "{ALARM_BROADCAST_TOKEN_ENV} must contain at least 32 bytes without whitespace or control characters"
        );
    }
    if matches!(
        token,
        "change-me-in-production"
            | "your-service-token-here"
            | "replace-with-a-strong-random-token"
            | "<alarm-broadcast-token>"
    ) {
        anyhow::bail!("{ALARM_BROADCAST_TOKEN_ENV} must not use a documented placeholder");
    }
    Ok(())
}

fn strict_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    match env::var(name) {
        Ok(value) if value == "true" => Ok(true),
        Ok(value) if value == "false" => Ok(false),
        Ok(_) => anyhow::bail!("{name} must be exactly 'true' or 'false'"),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    optional(name)?.ok_or_else(|| anyhow::anyhow!("{name} is required when CloudLink is enabled"))
}

fn required_parse<T>(name: &str) -> anyhow::Result<T>
where
    T: core::str::FromStr,
    T::Err: core::fmt::Display,
{
    required(name)?
        .parse()
        .map_err(|error| anyhow::anyhow!("{name} is invalid: {error}"))
}

fn optional_parse<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: core::str::FromStr,
    T::Err: core::fmt::Display,
{
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| anyhow::anyhow!("{name} is invalid: {error}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn bounded<T>(name: &str, value: T, minimum: T, maximum: T) -> anyhow::Result<T>
where
    T: Copy + Ord + core::fmt::Display,
{
    if value < minimum || value > maximum {
        anyhow::bail!("{name} must be between {minimum} and {maximum}");
    }
    Ok(value)
}

fn validate_spool_byte_limits(max_live_bytes: u64, max_journal_bytes: u64) -> anyhow::Result<()> {
    let required_journal_bytes = max_live_bytes
        .checked_add(CLOUDLINK_SPOOL_MIN_JOURNAL_HEADROOM_BYTES)
        .ok_or_else(|| anyhow::anyhow!("CloudLink spool byte limits overflow"))?;
    if max_journal_bytes < required_journal_bytes {
        anyhow::bail!(
            "AETHER_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES must be at least AETHER_CLOUDLINK_SPOOL_MAX_LIVE_BYTES plus {CLOUDLINK_SPOOL_MIN_JOURNAL_HEADROOM_BYTES} bytes"
        );
    }
    Ok(())
}

fn loopback_host(value: &str) -> anyhow::Result<String> {
    let address: IpAddr = value
        .parse()
        .map_err(|error| anyhow::anyhow!("API_HOST must be a loopback IP literal: {error}"))?;
    if !address.is_loopback() {
        anyhow::bail!("API_HOST must be a loopback IP address");
    }
    Ok(address.to_string())
}

fn optional(name: &str) -> anyhow::Result<Option<String>> {
    match env::var(name) {
        Ok(value)
            if !value.is_empty()
                && value.trim() == value
                && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value))
        },
        Ok(_) => anyhow::bail!(
            "{name} must be non-empty and contain no surrounding whitespace or control characters"
        ),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Docker Compose necessarily materializes optional interpolation as an empty
/// value. Only the explicitly optional broker-auth/TLS fields use this parser;
/// required values and optional numeric settings remain strict when present.
fn optional_compose_value(name: &str) -> anyhow::Result<Option<String>> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) if value.trim() == value && !value.chars().any(char::is_control) => {
            Ok(Some(value))
        },
        Ok(_) => {
            anyhow::bail!("{name} must contain no surrounding whitespace or control characters")
        },
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Mutex;

    use aether_cloudlink_mqtt::CloudLinkTlsConfig;

    use super::{
        CloudLinkSettings, EnvConfig, bounded, loopback_host, optional_parse,
        validate_alarm_broadcast_token, validate_spool_byte_limits,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvironmentRestore(Vec<(&'static str, Option<OsString>)>);

    impl EnvironmentRestore {
        fn set(values: &[(&'static str, &'static str)]) -> Self {
            let previous = values
                .iter()
                .map(|(name, value)| {
                    let previous = std::env::var_os(name);
                    // SAFETY: every environment-mutating test in this module is
                    // serialized by ENV_LOCK and this guard restores all keys.
                    unsafe { std::env::set_var(name, value) };
                    (*name, previous)
                })
                .collect();
            Self(previous)
        }
    }

    impl Drop for EnvironmentRestore {
        fn drop(&mut self) {
            for (name, previous) in self.0.drain(..) {
                // SAFETY: see EnvironmentRestore::set; ENV_LOCK remains held
                // until this guard is dropped.
                unsafe {
                    match previous {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    #[test]
    fn alarm_broadcast_token_must_be_strong_and_not_a_placeholder() {
        assert!(validate_alarm_broadcast_token("").is_err());
        assert!(validate_alarm_broadcast_token("change-me-in-production").is_err());
        assert!(validate_alarm_broadcast_token("replace-with-a-strong-random-token").is_err());
        assert!(validate_alarm_broadcast_token(" alarm-service-token-0123456789abcdef").is_err());
        assert!(validate_alarm_broadcast_token("alarm-service-token-0123456789abc\ndef").is_err());
        assert!(validate_alarm_broadcast_token("alarm-service-token-0123456789abcdef").is_ok());
    }

    #[test]
    fn present_but_invalid_numeric_environment_never_falls_back() {
        let _guard = ENV_LOCK.lock().expect("environment lock");
        const NAME: &str = "AETHER_UPLINK_TEST_STRICT_NUMBER";
        // SAFETY: this test serializes all environment mutation in this module
        // and restores the variable before releasing the lock.
        unsafe { std::env::set_var(NAME, "not-a-number") };
        let error = optional_parse::<u16>(NAME, 6006).expect_err("invalid present value");
        unsafe { std::env::remove_var(NAME) };
        assert!(error.to_string().contains(NAME));
    }

    #[test]
    fn numeric_safety_bounds_reject_zero_and_excessive_values() {
        assert!(bounded("API_PORT", 0_u16, 1, u16::MAX).is_err());
        assert!(bounded("AETHER_CLOUDLINK_SPOOL_CAPACITY", 65_537_usize, 1, 65_536).is_err());
        assert!(
            bounded(
                "AETHER_CLOUDLINK_RECEIPT_CAPACITY",
                1_000_001_usize,
                1,
                1_000_000
            )
            .is_err()
        );
        assert!(bounded("AETHER_CLOUDLINK_KEEP_ALIVE_SECS", 4_u64, 5, 3_600).is_err());
        assert!(bounded("AETHER_CLOUDLINK_CREDENTIAL_GENERATION", 0_u64, 1, u64::MAX).is_err());
        assert!(validate_spool_byte_limits(268_435_456, 268_500_991).is_err());
        assert!(validate_spool_byte_limits(268_435_456, 268_500_992).is_ok());
    }

    #[test]
    fn internal_http_bind_accepts_only_ipv4_or_ipv6_loopback_literals() {
        assert_eq!(
            loopback_host("127.0.0.1").expect("IPv4 loopback"),
            "127.0.0.1"
        );
        assert_eq!(
            loopback_host("127.42.0.9").expect("IPv4 loopback range"),
            "127.42.0.9"
        );
        assert_eq!(loopback_host("::1").expect("IPv6 loopback"), "::1");

        for exposed in ["0.0.0.0", "::", "192.168.1.10", "localhost"] {
            assert!(
                loopback_host(exposed).is_err(),
                "accepted exposed bind {exposed}"
            );
        }
    }

    #[test]
    fn disabled_cloudlink_still_rejects_invalid_spool_quota_environment() {
        let _guard = ENV_LOCK.lock().expect("environment lock");
        let _restore = EnvironmentRestore::set(&[
            ("AETHER_CLOUDLINK_ENABLED", "false"),
            ("AETHER_CLOUDLINK_SPOOL_MAX_LIVE_BYTES", "0"),
        ]);
        let Err(error) = EnvConfig::from_env() else {
            panic!("invalid durable spool quota was accepted");
        };
        assert!(
            error
                .to_string()
                .contains("AETHER_CLOUDLINK_SPOOL_MAX_LIVE_BYTES")
        );
    }

    #[test]
    fn enabled_compose_defaults_accept_system_roots_without_basic_auth() {
        let _guard = ENV_LOCK.lock().expect("environment lock");
        let _restore = EnvironmentRestore::set(&[
            ("AETHER_CLOUDLINK_BROKER_HOST", "broker.example"),
            ("AETHER_CLOUDLINK_BROKER_USERNAME", ""),
            ("AETHER_CLOUDLINK_BROKER_PASSWORD", ""),
            ("AETHER_CLOUDLINK_BROKER_CA", ""),
            ("AETHER_CLOUDLINK_BROKER_CLIENT_CERT", ""),
            ("AETHER_CLOUDLINK_BROKER_CLIENT_KEY", ""),
            ("AETHER_CLOUDLINK_CLOUD_KEY_ID", "cloud-key"),
            (
                "AETHER_CLOUDLINK_CLOUD_VERIFYING_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            ("AETHER_CLOUDLINK_CREDENTIAL_ID", "credential"),
            ("AETHER_CLOUDLINK_CREDENTIAL_GENERATION", "1"),
        ]);

        let settings = CloudLinkSettings::from_env().expect("minimal enabled CloudLink");
        assert!(settings.username.is_none());
        assert!(settings.password.is_none());
        assert!(matches!(settings.tls, CloudLinkTlsConfig::SystemRoots));
    }

    #[test]
    fn optional_empty_tls_values_do_not_hide_a_partial_client_identity() {
        let _guard = ENV_LOCK.lock().expect("environment lock");
        let _restore = EnvironmentRestore::set(&[
            ("AETHER_CLOUDLINK_BROKER_CA", ""),
            ("AETHER_CLOUDLINK_BROKER_CLIENT_CERT", "/cert/client.pem"),
            ("AETHER_CLOUDLINK_BROKER_CLIENT_KEY", ""),
        ]);
        assert!(super::tls_from_env().is_err());
    }
}
