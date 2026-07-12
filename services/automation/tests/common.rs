//! Shared SQLite-only integration-test scaffolding.

#![allow(clippy::disallowed_methods)]
#![allow(dead_code)]

use aether_automation::product_loader::ProductLoader;
use aether_model::product_lib::ProductLibrary;
use anyhow::Result;
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

pub fn energy_product_loader(pool: SqlitePool) -> ProductLoader {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/energy/models");
    let library = ProductLibrary::load(Some(&directory)).expect("load Energy Pack model fixture");
    ProductLoader::with_library(pool, Arc::new(library))
}

pub struct TestEnv {
    pub pool: SqlitePool,
    _temp_dir: TempDir,
}

impl TestEnv {
    pub async fn create() -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        let db_path = temp_dir.path().join("test_aether.db");
        let pool = SqlitePool::connect(&format!("sqlite:{}?mode=rwc", db_path.display())).await?;
        init_test_schema(&pool).await?;
        Ok(Self {
            pool,
            _temp_dir: temp_dir,
        })
    }

    pub async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

async fn init_test_schema(pool: &SqlitePool) -> Result<()> {
    common::test_utils::schema::init_automation_schema(pool).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS calculations (
            calculation_id INTEGER PRIMARY KEY AUTOINCREMENT,
            calculation_name TEXT NOT NULL UNIQUE,
            product_name TEXT NOT NULL,
            result_point_id INTEGER NOT NULL,
            expression TEXT NOT NULL,
            description TEXT,
            enabled BOOLEAN DEFAULT TRUE,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub mod fixtures {
    use std::collections::HashMap;

    use serde_json::json;

    pub fn create_test_instance_properties() -> HashMap<String, serde_json::Value> {
        let mut props = HashMap::new();
        props.insert("Max Capacity".to_string(), json!(500));
        props.insert("Min SOC".to_string(), json!(10));
        props.insert("Max SOC".to_string(), json!(95));
        props
    }
}

pub mod helpers {
    use anyhow::Result;
    use sqlx::SqlitePool;

    pub async fn assert_instance_exists(pool: &SqlitePool, instance_id: u16) -> Result<bool> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM instances WHERE instance_id = ?)",
        )
        .bind(i64::from(instance_id))
        .fetch_one(pool)
        .await?;
        Ok(exists)
    }

    pub async fn cleanup_test_data(pool: &SqlitePool) -> Result<()> {
        sqlx::query("DELETE FROM measurement_routing")
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM action_routing")
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM instances").execute(pool).await?;
        Ok(())
    }
}
