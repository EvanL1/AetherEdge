#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable.

use std::collections::HashMap;
use std::sync::Arc;

use tempfile::TempDir;

use super::*;

fn noop_dispatch() -> Arc<dyn crate::infra::shm_dispatch::ActionDispatch> {
    Arc::new(crate::infra::shm_dispatch::NoopDispatch)
}

async fn test_manager() -> (TempDir, InstanceManager) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("instance-manager.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&db_url).await.unwrap();
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .unwrap();
    let product_loader = Arc::new(ProductLoader::new(pool.clone()));
    let manager = InstanceManager::new(
        pool,
        Arc::new(aether_routing::RoutingCache::new()),
        product_loader,
        noop_dispatch(),
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

#[tokio::test]
async fn action_without_route_is_rejected_instead_of_stored_elsewhere() {
    let (_temp_dir, manager) = test_manager().await;

    let result = manager.execute_action(1001, "1", 42.0).await;

    assert!(matches!(result, Err(AutomationError::InvalidRouting(_))));
}

#[tokio::test]
async fn routed_action_requires_channel_health_shm() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("routed-action.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&db_url).await.unwrap();
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .unwrap();
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (2, 'device', 'virtual', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO adjustment_points
         (channel_id, point_id, signal_name, min_value, max_value, step)
         VALUES (2, 5, 'setpoint', 0.0, 100.0, 1.0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut routes = HashMap::new();
    routes.insert("1001:A:1".to_string(), "2:A:5".to_string());
    let routing_cache = Arc::new(aether_routing::RoutingCache::from_maps(
        HashMap::new(),
        routes,
        HashMap::new(),
    ));
    let manager = InstanceManager::new(
        pool.clone(),
        routing_cache,
        Arc::new(ProductLoader::new(pool)),
        noop_dispatch(),
    );

    let unsafe_value = manager.execute_action(1001, "1", 101.0).await;
    assert!(matches!(unsafe_value, Err(AutomationError::InvalidData(_))));

    let result = manager.execute_action(1001, "1", 42.0).await;

    assert!(matches!(result, Err(AutomationError::DispatchDegraded(_))));
}
