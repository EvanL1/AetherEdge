//! Transitional SQLite service-configuration reader.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// Generic service configuration stored in the shared site database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    /// Service name.
    pub service_name: String,
    /// Service port.
    pub port: u16,
    /// Additional configuration as JSON.
    pub extra_config: serde_json::Value,
}

/// Reads one service's configuration from the shared SQLite authority.
pub struct ServiceConfigLoader {
    pool: SqlitePool,
    service_name: String,
    default_port: u16,
}

impl ServiceConfigLoader {
    /// Opens an existing site database.
    pub async fn new(
        db_path: impl AsRef<Path>,
        service_name: impl Into<String>,
        default_port: u16,
    ) -> Result<Self> {
        let db_path = db_path.as_ref();
        if !db_path.exists() {
            anyhow::bail!(
                "service database not found at {}; run `aether sync` first",
                db_path.display()
            );
        }

        let pool = SqlitePool::connect(&format!("sqlite://{}", db_path.display()))
            .await
            .with_context(|| format!("open service database {}", db_path.display()))?;
        Ok(Self {
            pool,
            service_name: service_name.into(),
            default_port,
        })
    }

    /// Loads global values followed by service-specific overrides.
    pub async fn load_config(&self) -> Result<ServiceConfig> {
        let rows = sqlx::query(
            "SELECT key, value, type FROM service_config WHERE service_name = 'global' \
             UNION ALL \
             SELECT key, value, type FROM service_config WHERE service_name = ?",
        )
        .bind(&self.service_name)
        .fetch_all(&self.pool)
        .await?;

        let mut values = HashMap::new();
        for row in rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;
            let value_type: String = row.try_get("type").unwrap_or_else(|_| "string".into());
            values.insert(key, parse_value(value, &value_type));
        }

        let port = values
            .remove("service.port")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(self.default_port);

        Ok(ServiceConfig {
            service_name: self.service_name.clone(),
            port,
            extra_config: serde_json::Value::Object(values.into_iter().collect()),
        })
    }

    /// Returns the pool for service-owned configuration queries.
    #[must_use]
    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

fn parse_value(value: String, value_type: &str) -> serde_json::Value {
    match value_type {
        "number" => {
            if let Ok(integer) = value.parse::<i64>() {
                integer.into()
            } else if let Some(number) = value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
            {
                serde_json::Value::Number(number)
            } else {
                serde_json::Value::String(value)
            }
        },
        "boolean" => serde_json::Value::Bool(value.trim().eq_ignore_ascii_case("true")),
        "json" => serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value)),
        _ => serde_json::Value::String(value),
    }
}
