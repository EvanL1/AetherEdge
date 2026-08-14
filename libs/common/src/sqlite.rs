//! Per-service configuration stored in the local SQLite database.
//!
//! Reads the service's port and any extra keys the composition root wrote, so
//! a service does not need a YAML file beside its binary. Live point state
//! never comes from here — SHM remains the authority for that.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info};

/// Service configuration loader for SQLite-based config management
/// Each service has its own SQLite database with configuration
#[derive(Clone)]
pub struct ServiceConfigLoader {
    pool: SqlitePool,
    service_name: String,
    default_port: u16,
}

/// Generic service configuration stored in database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    /// Service name
    pub service_name: String,
    /// Service port
    pub port: u16,
    /// Additional configuration as JSON
    pub extra_config: serde_json::Value,
}

impl ServiceConfigLoader {
    /// Create a new service config loader
    pub async fn new(
        db_path: impl AsRef<Path>,
        service_name: impl Into<String>,
        default_port: u16,
    ) -> Result<Self> {
        let db_path = db_path.as_ref();
        let service_name = service_name.into();

        // Check if database exists
        if !db_path.exists() {
            return Err(anyhow::anyhow!(
                "Service database not found: {:?}. Please run aether sync first.",
                db_path
            ));
        }

        // Connect to database
        let db_url = format!("sqlite://{}", db_path.display());
        let pool = SqlitePool::connect(&db_url).await?;

        info!(
            "Connected to service database for {}: {:?}",
            service_name, db_path
        );

        Ok(Self::from_pool(pool, service_name, default_port))
    }

    /// Create a service config loader from an existing SQLite pool.
    #[must_use]
    pub fn from_pool(pool: SqlitePool, service_name: impl Into<String>, default_port: u16) -> Self {
        Self {
            pool,
            service_name: service_name.into(),
            default_port,
        }
    }

    /// Initialize database schema for service configuration
    pub async fn init_schema(&self) -> Result<()> {
        // Create service_config table with composite primary key
        // Supports both global and service-specific configuration
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS service_config (
                service_name TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                type TEXT DEFAULT 'string',
                description TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (service_name, key)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        debug!(
            "Service config schema initialized for {}",
            self.service_name
        );
        Ok(())
    }

    /// Load service configuration from database
    /// Merges global configuration with service-specific configuration
    /// Priority: service-specific > global
    pub async fn load_config(&self) -> Result<ServiceConfig> {
        // Load global config first, then service-specific config (UNION ALL for single query)
        // Service-specific config will override global config with same key
        let rows = sqlx::query(
            "SELECT key, value, type FROM service_config WHERE service_name = 'global'
             UNION ALL
             SELECT key, value, type FROM service_config WHERE service_name = ?",
        )
        .bind(&self.service_name)
        .fetch_all(&self.pool)
        .await?;

        let mut config_map = HashMap::new();

        for row in rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;
            let value_type: String = row
                .try_get("type")
                .with_context(|| format!("configuration key {key:?} has no valid type"))?;

            // Parse value based on type
            let parsed_value = match value_type.as_str() {
                "string" => serde_json::Value::String(value),
                "number" => match serde_json::from_str::<serde_json::Value>(&value)
                    .with_context(|| format!("configuration key {key:?} is not a JSON number"))?
                {
                    serde_json::Value::Number(number) => serde_json::Value::Number(number),
                    _ => return Err(anyhow!("configuration key {key:?} is not a JSON number")),
                },
                "boolean" => match value.as_str() {
                    "true" => serde_json::Value::Bool(true),
                    "false" => serde_json::Value::Bool(false),
                    _ => {
                        return Err(anyhow!(
                            "configuration key {key:?} must be exactly true or false"
                        ));
                    },
                },
                "json" => serde_json::from_str(&value)
                    .with_context(|| format!("configuration key {key:?} contains invalid JSON"))?,
                _ => {
                    return Err(anyhow!(
                        "configuration key {key:?} has unsupported type {value_type:?}"
                    ));
                },
            };

            config_map.insert(key, parsed_value);
        }

        // Extract standard fields - only support dotted key format
        let port = match config_map.get("service.port") {
            None => self.default_port,
            Some(value) => value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
                .ok_or_else(|| {
                    anyhow!("configuration key \"service.port\" must be an integer in 1..=65535")
                })?,
        };

        // Remove standard fields from map
        config_map.remove("service.port");

        Ok(ServiceConfig {
            service_name: self.service_name.clone(),
            port,
            extra_config: serde_json::Value::Object(config_map.into_iter().collect()),
        })
    }

    /// Store a configuration value
    pub async fn set_config(&self, key: &str, value: &str, value_type: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO service_config (service_name, key, value, type, updated_at)
            VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(service_name, key) DO UPDATE SET
                value = excluded.value,
                type = excluded.type,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(&self.service_name)
        .bind(key)
        .bind(value)
        .bind(value_type)
        .execute(&self.pool)
        .await?;

        debug!(
            "Set config [{}] {}={} (type: {})",
            self.service_name, key, value, value_type
        );
        Ok(())
    }

    /// Get a specific configuration value
    pub async fn get_config(&self, key: &str) -> Result<Option<String>> {
        let result = sqlx::query_scalar::<_, String>(
            "SELECT value FROM service_config WHERE service_name = ? AND key = ?",
        )
        .bind(&self.service_name)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(result)
    }

    /// Get the database pool for custom queries
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn loader() -> ServiceConfigLoader {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory service config database");
        let loader = ServiceConfigLoader::from_pool(pool, "aether-test", 6000);
        loader.init_schema().await.expect("service config schema");
        loader
    }

    #[tokio::test]
    async fn stored_config_uses_one_strict_shape_per_declared_type() {
        let loader = loader().await;
        loader
            .set_config("service.port", "6001", "number")
            .await
            .unwrap();
        loader
            .set_config("enabled", "true", "boolean")
            .await
            .unwrap();
        loader.set_config("ratio", "1.5", "number").await.unwrap();
        loader
            .set_config("policy", r#"{"mode":"strict"}"#, "json")
            .await
            .unwrap();
        loader.set_config("label", "edge", "string").await.unwrap();

        let config = loader.load_config().await.unwrap();
        assert_eq!(config.port, 6001);
        assert_eq!(config.extra_config["enabled"], true);
        assert_eq!(config.extra_config["ratio"], 1.5);
        assert_eq!(config.extra_config["policy"]["mode"], "strict");
        assert_eq!(config.extra_config["label"], "edge");
    }

    #[tokio::test]
    async fn corrupt_stored_config_fails_closed_instead_of_changing_shape_or_defaulting() {
        let loader = loader().await;
        for (key, value, value_type) in [
            ("number-text", "not-a-number", "number"),
            ("quoted-number", "\"12\"", "number"),
            ("boolean-case", "TRUE", "boolean"),
            ("boolean-integer", "1", "boolean"),
            ("broken-json", "{", "json"),
            ("unknown-type", "value", "other"),
            ("service.port", "70000", "number"),
            ("service.port", "1.5", "number"),
        ] {
            sqlx::query("DELETE FROM service_config")
                .execute(loader.pool())
                .await
                .unwrap();
            loader.set_config(key, value, value_type).await.unwrap();
            assert!(
                loader.load_config().await.is_err(),
                "corrupt {key}={value:?} ({value_type}) was accepted"
            );
        }

        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) VALUES (?, ?, ?, NULL)",
        )
        .bind("aether-test")
        .bind("missing-type")
        .bind("value")
        .execute(loader.pool())
        .await
        .unwrap();
        assert!(loader.load_config().await.is_err());
    }
}
