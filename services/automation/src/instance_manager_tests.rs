#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable.

use std::collections::HashMap;
use std::sync::Arc;

use tempfile::TempDir;

use super::*;

async fn test_manager() -> (TempDir, InstanceManager) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("instance-manager.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&db_url).await.unwrap();
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .unwrap();
    let product_loader = Arc::new(crate::product_loader::test_energy_product_loader(
        pool.clone(),
    ));
    let manager = InstanceManager::new(
        pool,
        Arc::new(aether_routing::RoutingCache::new()),
        product_loader,
    );
    (temp_dir, manager)
}

async fn setup_hierarchy(manager: &InstanceManager) -> u32 {
    manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(1),
            instance_name: "station_root".to_string(),
            product_name: "Station".to_string(),
            parent_id: None,
            properties: HashMap::new(),
        })
        .await
        .unwrap();
    manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(2),
            instance_name: "ess_parent".to_string(),
            product_name: "ESS".to_string(),
            parent_id: Some(1),
            properties: HashMap::new(),
        })
        .await
        .unwrap();
    2
}

#[tokio::test]
async fn create_get_and_delete_instance_use_local_state_only() {
    let (_temp_dir, manager) = test_manager().await;
    let parent_id = setup_hierarchy(&manager).await;

    let created = manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(1001),
            instance_name: "battery_01".to_string(),
            product_name: "Battery".to_string(),
            parent_id: Some(parent_id),
            properties: HashMap::new(),
        })
        .await
        .unwrap();
    assert_eq!(created.instance_id(), 1001);
    assert_eq!(manager.get_instance_id("battery_01").await.unwrap(), 1001);
    assert_eq!(
        manager.get_instance(1001).await.unwrap().instance_id(),
        1001
    );

    manager.delete_instance(1001).await.unwrap();
    assert!(manager.get_instance(1001).await.is_err());
    assert!(manager.get_instance_id("battery_01").await.is_err());
}

#[tokio::test]
async fn rename_updates_the_process_local_name_cache() {
    let (_temp_dir, manager) = test_manager().await;
    let parent_id = setup_hierarchy(&manager).await;
    manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(1001),
            instance_name: "before".to_string(),
            product_name: "Battery".to_string(),
            parent_id: Some(parent_id),
            properties: HashMap::new(),
        })
        .await
        .unwrap();

    manager.rename_instance(1001, "after").await.unwrap();

    assert!(manager.get_instance_id("before").await.is_err());
    assert_eq!(manager.get_instance_id("after").await.unwrap(), 1001);
}

#[tokio::test]
async fn instance_properties_are_persisted_in_sqlite() {
    let (_temp_dir, manager) = test_manager().await;
    common::test_utils::schema::init_io_schema(manager.pool())
        .await
        .unwrap();
    let parent_id = setup_hierarchy(&manager).await;
    let mut properties = HashMap::new();
    properties.insert("Max Power".to_string(), serde_json::json!(5000.0));
    manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(2001),
            instance_name: "pcs_01".to_string(),
            product_name: "PCS".to_string(),
            parent_id: Some(parent_id),
            properties,
        })
        .await
        .unwrap();

    let value: String = sqlx::query_scalar(
        "SELECT value_json FROM instance_properties WHERE instance_id = 2001 AND property_id = 1",
    )
    .fetch_one(manager.pool())
    .await
    .unwrap();
    assert_eq!(value, "5000.0");
}
