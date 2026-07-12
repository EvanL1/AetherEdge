//! SQLite-backed instance lifecycle integration tests.

#![allow(clippy::disallowed_methods)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use aether_automation::instance_manager::InstanceManager;
use aether_automation::product_loader::CreateInstanceRequest;
use aether_routing::RoutingCache;
use anyhow::Result;
use common::{TestEnv, energy_product_loader, fixtures, helpers};

fn manager(env: &TestEnv) -> InstanceManager {
    InstanceManager::new(
        env.pool().clone(),
        Arc::new(RoutingCache::new()),
        Arc::new(energy_product_loader(env.pool().clone())),
    )
}

async fn create_hierarchy(manager: &InstanceManager) -> Result<()> {
    manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(9901),
            instance_name: "test_station_root".to_string(),
            product_name: "Station".to_string(),
            parent_id: None,
            properties: HashMap::new(),
        })
        .await?;
    manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(9902),
            instance_name: "test_ess_parent".to_string(),
            product_name: "ESS".to_string(),
            parent_id: Some(9901),
            properties: HashMap::new(),
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn create_instance_full_flow_requires_no_external_service() -> Result<()> {
    let env = TestEnv::create().await?;
    let manager = manager(&env);
    create_hierarchy(&manager).await?;

    let instance = manager
        .create_instance(CreateInstanceRequest {
            instance_id: Some(1001),
            instance_name: "battery_001".to_string(),
            product_name: "Battery".to_string(),
            parent_id: Some(9902),
            properties: fixtures::create_test_instance_properties(),
        })
        .await?;

    assert_eq!(instance.instance_id(), 1001);
    assert_eq!(instance.instance_name(), "battery_001");
    assert!(helpers::assert_instance_exists(env.pool(), 1001).await?);
    assert_eq!(manager.get_instance_id("battery_001").await?, 1001);
    env.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn duplicate_instance_is_rejected_by_local_persistence() -> Result<()> {
    let env = TestEnv::create().await?;
    let manager = manager(&env);
    create_hierarchy(&manager).await?;
    let request = CreateInstanceRequest {
        instance_id: Some(1001),
        instance_name: "battery_001".to_string(),
        product_name: "Battery".to_string(),
        parent_id: Some(9902),
        properties: fixtures::create_test_instance_properties(),
    };

    manager.create_instance(request.clone()).await?;
    assert!(manager.create_instance(request).await.is_err());

    env.cleanup().await?;
    Ok(())
}
