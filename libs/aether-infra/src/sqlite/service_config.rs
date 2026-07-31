use anyhow::Result;
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
            let value_type: String = row.try_get("type").unwrap_or_else(|_| "string".to_string());

            // Parse value based on type
            let parsed_value = match value_type.as_str() {
                "number" => {
                    if let Ok(n) = value.parse::<i64>() {
                        serde_json::Value::Number(n.into())
                    } else if let Ok(f) = value.parse::<f64>() {
                        serde_json::Number::from_f64(f)
                            .map(serde_json::Value::Number)
                            .unwrap_or_else(|| serde_json::Value::String(value.clone()))
                    } else {
                        serde_json::Value::String(value)
                    }
                },
                // Optimization: eq_ignore_ascii_case avoids to_lowercase() allocation
                "boolean" => serde_json::Value::Bool(value.trim().eq_ignore_ascii_case("true")),
                "json" => serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value)),
                _ => serde_json::Value::String(value),
            };

            config_map.insert(key, parsed_value);
        }

        // Extract standard fields - only support dotted key format
        let port = config_map
            .get("service.port")  // Standard dotted format from Aether
            .and_then(|v| v.as_i64())
            .unwrap_or(self.default_port as i64) as u16;

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
