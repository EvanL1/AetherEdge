/// uplink runtime configuration stored in the shared SQLite database.
use sqlx::SqlitePool;
use std::borrow::Cow;
use tracing::info;

use crate::models::NetConfig;

const DEFAULTS: &[(&str, &str, &str)] = &[
    ("product_sn", "AetherHub", "Product serial number"),
    (
        "device_sn",
        "auto",
        "Device SN or 'auto' to read from hardware",
    ),
    ("broker_host", "localhost", "MQTT broker hostname"),
    ("broker_port", "8883", "MQTT broker port"),
    ("broker_keepalive_secs", "120", "MQTT keepalive (seconds)"),
    (
        "client_id",
        "auto",
        "MQTT client ID ('auto' = use device_sn)",
    ),
    ("username", "", "MQTT username (empty = no auth)"),
    ("password", "", "MQTT password"),
    ("ssl_enabled", "false", "Enable TLS for MQTT"),
    (
        "reconnect_delay_secs",
        "10",
        "Seconds between reconnect attempts",
    ),
    (
        "reconnect_max_attempts",
        "50",
        "Maximum reconnect attempts (0 = unlimited)",
    ),
    (
        "report_interval_secs",
        "50",
        "Data upload interval (seconds)",
    ),
    ("report_batch_size", "50", "Max entries per MQTT message"),
    (
        "system_monitor_enabled",
        "true",
        "Collect and upload system metrics",
    ),
    (
        "system_monitor_interval_secs",
        "10",
        "System metrics collection interval (s)",
    ),
    (
        "telemetry_enabled",
        "false",
        "Publish acquisition-path telemetry; requires broker policy for the telemetry topic",
    ),
    (
        "telemetry_interval_secs",
        "30",
        "Telemetry sampling interval (s)",
    ),
    (
        "subscribe_patterns",
        r#"["inst:*:M","inst:*:A"]"#,
        "JSON array of logical SHM group patterns",
    ),
    (
        "exclude_patterns",
        "[]",
        "JSON array of regex exclude patterns",
    ),
    (
        "alarm_url",
        "http://localhost:6007",
        "alarm base URL for call-alarm",
    ),
    (
        "automation_url",
        "http://localhost:6002",
        "automation base URL for inst-sync queries",
    ),
];

pub async fn create_config_table(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS uplink_config (
            key         TEXT PRIMARY KEY,
            value       TEXT NOT NULL,
            description TEXT,
            updated_at  TEXT DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    for (key, value, desc) in DEFAULTS {
        sqlx::query(
            "INSERT OR IGNORE INTO uplink_config (key, value, description) VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(value)
        .bind(desc)
        .execute(pool)
        .await?;
    }

    info!("uplink_config table ready");
    Ok(())
}

pub async fn load_config(pool: &SqlitePool) -> anyhow::Result<NetConfig> {
    use std::collections::HashMap;

    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM uplink_config")
        .fetch_all(pool)
        .await?;
    let map: HashMap<String, String> = rows.into_iter().collect();

    let get = |k: &str, d: &str| map.get(k).cloned().unwrap_or_else(|| d.to_string());

    let mut cfg = NetConfig {
        product_sn: get("product_sn", "AetherHub"),
        device_sn: get("device_sn", "auto"),
        broker_host: get("broker_host", "localhost"),
        broker_port: get("broker_port", "8883").parse().unwrap_or(8883),
        broker_keepalive_secs: get("broker_keepalive_secs", "120").parse().unwrap_or(120),
        client_id: get("client_id", "auto"),
        username: {
            let v = get("username", "");
            if v.is_empty() { None } else { Some(v) }
        },
        password: {
            let v = get("password", "");
            if v.is_empty() { None } else { Some(v) }
        },
        ssl_enabled: get("ssl_enabled", "false") == "true",
        reconnect_delay_secs: get("reconnect_delay_secs", "10").parse().unwrap_or(10),
        reconnect_max_attempts: get("reconnect_max_attempts", "50").parse().unwrap_or(50),
        report_interval_secs: get("report_interval_secs", "50").parse().unwrap_or(50),
        report_batch_size: get("report_batch_size", "50").parse().unwrap_or(50),
        system_monitor_enabled: get("system_monitor_enabled", "true") == "true",
        system_monitor_interval_secs: get("system_monitor_interval_secs", "10")
            .parse()
            .unwrap_or(10),
        telemetry_enabled: get("telemetry_enabled", "false") == "true",
        telemetry_interval_secs: get("telemetry_interval_secs", "30").parse().unwrap_or(30),
        subscribe_patterns: serde_json::from_str(&get(
            "subscribe_patterns",
            r#"["inst:*:M","inst:*:A"]"#,
        ))
        .unwrap_or_else(|_| vec!["inst:*:M".to_string(), "inst:*:A".to_string()]),
        exclude_patterns: serde_json::from_str(&get("exclude_patterns", "[]")).unwrap_or_default(),
        alarm_url: get("alarm_url", "http://localhost:6007"),
        automation_url: get("automation_url", "http://localhost:6002"),
    };
    cfg.normalize();
    Ok(cfg)
}

pub async fn save_config(pool: &SqlitePool, cfg: &NetConfig) -> anyhow::Result<()> {
    let mut cfg = cfg.clone();
    cfg.normalize();
    let pairs: Vec<(&str, Cow<'_, str>)> = vec![
        ("product_sn", Cow::Borrowed(cfg.product_sn.as_str())),
        ("device_sn", Cow::Borrowed(cfg.device_sn.as_str())),
        ("broker_host", Cow::Borrowed(cfg.broker_host.as_str())),
        ("broker_port", Cow::Owned(cfg.broker_port.to_string())),
        (
            "broker_keepalive_secs",
            Cow::Owned(cfg.broker_keepalive_secs.to_string()),
        ),
        ("client_id", Cow::Borrowed(cfg.client_id.as_str())),
        (
            "username",
            Cow::Borrowed(cfg.username.as_deref().unwrap_or_default()),
        ),
        // TODO(security): MQTT password stored plaintext in local SQLite.
        // Acceptable for single-user device config; revisit if DB is shared.
        (
            "password",
            Cow::Borrowed(cfg.password.as_deref().unwrap_or_default()),
        ),
        ("ssl_enabled", Cow::Owned(cfg.ssl_enabled.to_string())),
        (
            "reconnect_delay_secs",
            Cow::Owned(cfg.reconnect_delay_secs.to_string()),
        ),
        (
            "reconnect_max_attempts",
            Cow::Owned(cfg.reconnect_max_attempts.to_string()),
        ),
        (
            "report_interval_secs",
            Cow::Owned(cfg.report_interval_secs.to_string()),
        ),
        (
            "report_batch_size",
            Cow::Owned(cfg.report_batch_size.to_string()),
        ),
        (
            "system_monitor_enabled",
            Cow::Owned(cfg.system_monitor_enabled.to_string()),
        ),
        (
            "system_monitor_interval_secs",
            Cow::Owned(cfg.system_monitor_interval_secs.to_string()),
        ),
        (
            "telemetry_enabled",
            Cow::Owned(cfg.telemetry_enabled.to_string()),
        ),
        (
            "telemetry_interval_secs",
            Cow::Owned(cfg.telemetry_interval_secs.to_string()),
        ),
        (
            "subscribe_patterns",
            Cow::Owned(serde_json::to_string(&cfg.subscribe_patterns)?),
        ),
        (
            "exclude_patterns",
            Cow::Owned(serde_json::to_string(&cfg.exclude_patterns)?),
        ),
        ("alarm_url", Cow::Borrowed(cfg.alarm_url.as_str())),
        ("automation_url", Cow::Borrowed(cfg.automation_url.as_str())),
    ];

    let mut tx = pool.begin().await?;
    for (key, value) in pairs {
        sqlx::query(
            "INSERT INTO uplink_config (key, value)
             VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = datetime('now')",
        )
        .bind(key)
        .bind(value.as_ref())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
