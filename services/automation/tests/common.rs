//! Shared SQLite-only integration-test scaffolding.

#![allow(clippy::disallowed_methods)]
#![allow(dead_code)]

use aether_application::{Actor, RequestContext};
use aether_automation::instance_configuration::{
    InstanceConfigurationApplication, InstanceConfigurationMutation, InstanceConfigurationPayload,
    InstanceConfigurationRevision, initialize_instance_configuration_revision,
};
use aether_automation::product_loader::{CreateInstanceRequest, Instance, ProductLoader};
use aether_automation::{AutomationError, InstanceManager};
use aether_domain::TimestampMs;
use aether_model::product_lib::ProductLibrary;
use aether_ports::AuditSink;
use anyhow::Result;
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

pub fn energy_product_loader(pool: SqlitePool) -> ProductLoader {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/energy/models");
    let library = ProductLibrary::load(Some(&directory)).expect("load Energy Pack model fixture");
    ProductLoader::with_library(pool, Arc::new(library))
}

/// Test-only facade that keeps legacy lifecycle suites on the governed online
/// command boundary while retaining convenient query access through `Deref`.
pub struct GovernedInstanceManager {
    manager: Arc<InstanceManager>,
    application: InstanceConfigurationApplication,
    revision: AtomicU64,
}

impl GovernedInstanceManager {
    pub async fn new(pool: SqlitePool, product_loader: ProductLoader) -> Self {
        initialize_instance_configuration_revision(&pool)
            .await
            .expect("instances revision");
        let revision: i64 = sqlx::query_scalar(
            "SELECT revision FROM configuration_revisions WHERE scope = 'instances'",
        )
        .fetch_one(&pool)
        .await
        .expect("current instances revision");
        let manager = Arc::new(InstanceManager::new(pool, Arc::new(product_loader)));
        manager
            .populate_name_cache()
            .await
            .expect("initial instance index");
        let audit: Arc<dyn AuditSink> = Arc::new(aether_store_local::MemoryAuditSink::new());
        let application = InstanceConfigurationApplication::new(Arc::clone(&manager), audit);
        Self {
            manager,
            application,
            revision: AtomicU64::new(u64::try_from(revision).expect("positive revision")),
        }
    }

    pub async fn create_instance(
        &self,
        request: CreateInstanceRequest,
    ) -> Result<Instance, AutomationError> {
        let acceptance = self
            .application
            .mutate(
                &self.context(),
                InstanceConfigurationMutation::Create {
                    request,
                    expected_revision: self.expected_revision(),
                },
            )
            .await?;
        self.revision
            .store(acceptance.resulting_revision().get(), Ordering::Release);
        match acceptance.payload() {
            InstanceConfigurationPayload::Created(instance) => Ok(instance.clone()),
            _ => Err(AutomationError::InternalError(
                "governed create returned unexpected payload".to_string(),
            )),
        }
    }

    pub async fn rename_instance(
        &self,
        instance_id: u32,
        name: &str,
    ) -> Result<(), AutomationError> {
        let acceptance = self
            .application
            .mutate(
                &self.context(),
                InstanceConfigurationMutation::Update {
                    instance_id,
                    instance_name: Some(name.to_string()),
                    properties: None,
                    expected_revision: self.expected_revision(),
                },
            )
            .await?;
        self.revision
            .store(acceptance.resulting_revision().get(), Ordering::Release);
        Ok(())
    }

    pub async fn delete_instance(&self, instance_id: u32) -> Result<(), AutomationError> {
        let acceptance = self
            .application
            .mutate(
                &self.context(),
                InstanceConfigurationMutation::DeleteSubtree {
                    instance_id,
                    expected_revision: self.expected_revision(),
                },
            )
            .await?;
        self.revision
            .store(acceptance.resulting_revision().get(), Ordering::Release);
        Ok(())
    }

    fn expected_revision(&self) -> InstanceConfigurationRevision {
        InstanceConfigurationRevision::new(self.revision.load(Ordering::Acquire))
    }

    fn context(&self) -> RequestContext {
        RequestContext::new(
            uuid::Uuid::new_v4().to_string(),
            Actor::new("test-commissioner").with_permission("automation.instance.manage"),
            true,
            TimestampMs::new(1_720_000_000_000),
        )
    }
}

impl std::ops::Deref for GovernedInstanceManager {
    type Target = InstanceManager;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
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
