//! Tests for refresh_routing() — local routing cache refresh
//!
//! After the SHM/routing decoupling, refresh_routing() is a pure local operation:
//! load routes from SQLite → update in-memory RoutingCache. No io HTTP call,
//! no SHM rebuild.

#![allow(clippy::disallowed_methods)] // test code — unwrap is acceptable

use aether_automation::infra::shm_dispatch::{ActionDispatch, NoopDispatch};
use aether_automation::instance_manager::InstanceManager;
use aether_automation::product_loader::ProductLoader;
use aether_routing::RoutingCache;
use common::ReloadableService;
use sqlx::SqlitePool;
use std::sync::Arc;
use tempfile::TempDir;

// ============================================================================
// Shared helpers
// ============================================================================

async fn create_test_db() -> (TempDir, SqlitePool) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&url).await.unwrap();
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .unwrap();
    (tmp, pool)
}

fn make_manager(pool: SqlitePool) -> InstanceManager {
    let routing_cache = Arc::new(RoutingCache::new());
    let product_loader = Arc::new(ProductLoader::new(pool.clone()));
    let dispatch: Arc<dyn ActionDispatch> = Arc::new(NoopDispatch);
    InstanceManager::new(pool, routing_cache, product_loader, dispatch)
}

async fn insert_action_routing(pool: &SqlitePool) {
    sqlx::query(
        "INSERT OR IGNORE INTO channels (channel_id, name, protocol, enabled) \
         VALUES (1, 'ch1', 'Virtual', 1)",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT OR IGNORE INTO instances \
         (instance_id, instance_name, product_name) \
         VALUES (1, 'inst1', 'Battery')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT OR IGNORE INTO action_routing \
         (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id, enabled) \
         VALUES (1, 'inst1', 1, 1, 'A', 1, 1)",
    )
    .execute(pool)
    .await
    .unwrap();
}

// ============================================================================
// refresh_routing() tests
// ============================================================================

/// After inserting routing data into SQLite, refresh_routing() must:
/// - Load routes from SQLite into the RoutingCache
/// - Return Ok(count) where count > 0
/// - The RoutingCache must now contain the loaded M2C entry
#[tokio::test]
async fn test_refresh_routing_updates_cache() {
    let (_tmp, pool) = create_test_db().await;
    insert_action_routing(&pool).await;

    let manager = make_manager(pool);

    assert!(
        manager.routing_cache().lookup_m2c("1:A:1").is_none(),
        "Cache should be empty before refresh"
    );

    let result = manager.refresh_routing().await;

    assert!(
        result.is_ok(),
        "refresh_routing should succeed: {:?}",
        result.err()
    );
    assert!(
        result.unwrap() > 0,
        "route count must be > 0 after inserting a row"
    );

    assert!(
        manager.routing_cache().lookup_m2c("1:A:1").is_some(),
        "RoutingCache must hold the M2C route after refresh"
    );
}

/// refresh_routing() succeeds even when no io is reachable.
///
/// This test also implicitly verifies that refresh_routing() triggers no SHM
/// operations: `NoopDispatch` is used, which performs no SHM reads or writes by
/// definition. If refresh_routing() attempted any SHM call it would panic or
/// require a real SHM file, causing the test to fail.
#[tokio::test]
async fn test_refresh_routing_succeeds_without_io() {
    let (_tmp, pool) = create_test_db().await;
    let manager = make_manager(pool);

    let result = manager.refresh_routing().await;

    assert!(
        result.is_ok(),
        "refresh_routing must succeed without io: {:?}",
        result.err()
    );
}

/// Two concurrent refresh_routing() calls on separate managers must both
/// complete without panic.
#[tokio::test]
async fn test_refresh_routing_concurrent_calls() {
    let (_tmp1, pool1) = create_test_db().await;
    let (_tmp2, pool2) = create_test_db().await;

    let manager1 = Arc::new(make_manager(pool1));
    let manager2 = Arc::new(make_manager(pool2));

    let m1 = Arc::clone(&manager1);
    let h1 = tokio::spawn(async move { m1.refresh_routing().await });

    let m2 = Arc::clone(&manager2);
    let h2 = tokio::spawn(async move { m2.refresh_routing().await });

    let (r1, r2) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        (h1.await, h2.await)
    })
    .await
    .expect("concurrent refresh calls must complete within 10s");

    assert!(r1.is_ok(), "task 1 panicked: {:?}", r1.err());
    assert!(r2.is_ok(), "task 2 panicked: {:?}", r2.err());
}

/// Reloading from SQLite must rebuild process-local derived state without an
/// RTDB mirror. A name inserted directly into the database becomes available
/// through the manager after reload.
#[tokio::test]
async fn reload_from_database_rebuilds_name_cache() {
    let (_tmp, pool) = create_test_db().await;
    let manager = make_manager(pool.clone());

    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) VALUES (7, 'pump_7', 'Battery')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = manager.reload_from_database(&pool).await.unwrap();

    assert_eq!(result.total_count, 1);
    assert_eq!(manager.get_instance_id("pump_7").await.unwrap(), 7);
}
