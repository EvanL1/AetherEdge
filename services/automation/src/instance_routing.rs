//! Instance Routing Management
//!
//! This module provides routing CRUD operations for measurement and action points.
//! Extracted from instance_manager.rs for better code organization.

use aether_model::PointType;
use anyhow::{Result, anyhow};
use common::{ValidationLevel, ValidationResult};
use tracing::{debug, warn};

use crate::routing_loader::{
    ActionRouting, ActionRoutingRow, MeasurementRouting, MeasurementRoutingRow,
};

use super::instance_manager::InstanceManager;
use aether_rtdb_shm::SharedSlotRef;

impl InstanceManager {
    /// Create or update routing for a single measurement point (UPSERT)
    pub async fn upsert_measurement_routing(
        &self,
        instance_id: u32,
        point_id: u32,
        request: crate::dto::SinglePointRoutingRequest,
    ) -> Result<()> {
        // Validate channel_type (must be T or S for measurement, skip if None - unbound)
        if let Some(ref fr) = request.four_remote
            && !fr.is_input()
        {
            return Err(anyhow!(
                "Invalid channel_type '{}' for measurement routing (must be T or S)",
                fr
            ));
        }

        // Get instance_name for routing table denormalization
        let instance_name = sqlx::query_scalar::<_, String>(
            "SELECT instance_name FROM instances WHERE instance_id = ?",
        )
        .bind(instance_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow!("Instance {} not found: {}", instance_id, e))?;

        // UPSERT into measurement_routing
        sqlx::query(
            r#"
            INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type, channel_point_id,
             measurement_id, enabled)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(instance_id, measurement_id)
            DO UPDATE SET
                channel_id = excluded.channel_id,
                channel_type = excluded.channel_type,
                channel_point_id = excluded.channel_point_id,
                enabled = excluded.enabled,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(instance_id as i64)
        .bind(instance_name)
        .bind(request.channel_id)
        .bind(request.four_remote.map(|fr| fr.as_str()))
        .bind(request.channel_point_id)
        .bind(point_id)
        .bind(request.enabled)
        .execute(&self.pool)
        .await?;

        // Dynamic Slot Allocation: Add shared_slot reference for C2M routing
        // Only for enabled bound routing (has channel info)
        if request.enabled
            && let (Some(index), Some(channel_id), Some(channel_type), Some(channel_point_id)) = (
                self.dynamic_instance_index(),
                request.channel_id,
                request.four_remote,
                request.channel_point_id,
            )
        {
            let channel_id = channel_id as u32;
            let slot_ref = SharedSlotRef {
                // slot_id = 0: Delayed computation - resolve when accessing shared memory
                slot_id: 0,
                // point_type = source channel type (T/S for measurement routing)
                point_type: channel_type,
                point_id,
                source_channel_id: channel_id,
                source_point_type: channel_type,
                source_point_id: channel_point_id,
            };

            match index.add_shared_slot(instance_id, slot_ref) {
                Ok(()) => {
                    debug!(
                        "Inst{}:M:{} shared_slot added (ch:{}:{}:{})",
                        instance_id, point_id, channel_id, channel_type, channel_point_id
                    );
                },
                Err(e) => {
                    warn!(
                        "Inst{}:M:{} shared_slot add failed: {}",
                        instance_id, point_id, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Create or update routing for a single action point (UPSERT)
    pub async fn upsert_action_routing(
        &self,
        instance_id: u32,
        point_id: u32,
        request: crate::dto::SinglePointRoutingRequest,
    ) -> Result<()> {
        // Validate channel_type (must be C or A for action, skip if None - unbound)
        if let Some(ref fr) = request.four_remote
            && !fr.is_output()
        {
            return Err(anyhow!(
                "Invalid channel_type '{}' for action routing (must be C or A)",
                fr
            ));
        }

        // Get instance_name for routing table denormalization
        let instance_name = sqlx::query_scalar::<_, String>(
            "SELECT instance_name FROM instances WHERE instance_id = ?",
        )
        .bind(instance_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow!("Instance {} not found: {}", instance_id, e))?;

        // UPSERT into action_routing
        sqlx::query(
            r#"
            INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(instance_id, action_id)
            DO UPDATE SET
                channel_id = excluded.channel_id,
                channel_type = excluded.channel_type,
                channel_point_id = excluded.channel_point_id,
                enabled = excluded.enabled,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(instance_id as i64)
        .bind(instance_name)
        .bind(point_id)
        .bind(request.channel_id)
        .bind(request.four_remote.map(|fr| fr.as_str()))
        .bind(request.channel_point_id)
        .bind(request.enabled)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Delete routing for a single measurement point
    pub async fn delete_measurement_routing(&self, instance_id: u32, point_id: u32) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM measurement_routing WHERE instance_id = ? AND measurement_id = ?",
        )
        .bind(instance_id as i64)
        .bind(point_id)
        .execute(&self.pool)
        .await?;

        // Dynamic Slot Allocation: Remove shared_slot reference
        // Try both Telemetry and Signal types since measurement routing uses T/S
        if result.rows_affected() > 0
            && let Some(index) = self.dynamic_instance_index()
        {
            // Try Telemetry first, then Signal
            let removed = index
                .remove_shared_slot(instance_id, PointType::Telemetry, point_id)
                .ok()
                .flatten()
                .or_else(|| {
                    index
                        .remove_shared_slot(instance_id, PointType::Signal, point_id)
                        .ok()
                        .flatten()
                });

            match removed {
                Some(slot_ref) => {
                    debug!(
                        "Inst{}:M:{} shared_slot removed (ch:{}:{}:{})",
                        instance_id,
                        point_id,
                        slot_ref.source_channel_id,
                        slot_ref.source_point_type,
                        slot_ref.source_point_id
                    );
                },
                None => {
                    // No shared_slot found - might be unbound routing
                    debug!(
                        "Inst{}:M:{} no shared_slot to remove",
                        instance_id, point_id
                    );
                },
            }
        }

        Ok(result.rows_affected())
    }

    /// Delete routing for a single action point
    pub async fn delete_action_routing(&self, instance_id: u32, point_id: u32) -> Result<u64> {
        let result =
            sqlx::query("DELETE FROM action_routing WHERE instance_id = ? AND action_id = ?")
                .bind(instance_id as i64)
                .bind(point_id)
                .execute(&self.pool)
                .await?;

        Ok(result.rows_affected())
    }

    /// Toggle enabled state for a single measurement point routing
    pub async fn toggle_measurement_routing(
        &self,
        instance_id: u32,
        point_id: u32,
        enabled: bool,
    ) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE measurement_routing
            SET enabled = ?, updated_at = CURRENT_TIMESTAMP
            WHERE instance_id = ? AND measurement_id = ?
            "#,
        )
        .bind(enabled)
        .bind(instance_id as i64)
        .bind(point_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Toggle enabled state for a single action point routing
    pub async fn toggle_action_routing(
        &self,
        instance_id: u32,
        point_id: u32,
        enabled: bool,
    ) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE action_routing
            SET enabled = ?, updated_at = CURRENT_TIMESTAMP
            WHERE instance_id = ? AND action_id = ?
            "#,
        )
        .bind(enabled)
        .bind(instance_id as i64)
        .bind(point_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Get all measurement routing for an instance
    ///
    /// Retrieves all enabled measurement routing entries for the specified instance.
    pub async fn get_measurement_routing(
        &self,
        instance_id: u32,
    ) -> Result<Vec<MeasurementRouting>> {
        let routing = sqlx::query_as::<_, MeasurementRouting>(
            r#"
            SELECT * FROM measurement_routing
            WHERE instance_id = ? AND enabled = TRUE
            ORDER BY channel_id, channel_type, channel_point_id
            "#,
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(routing)
    }

    /// Get all action routing for an instance
    ///
    /// Retrieves all enabled action routing entries for the specified instance.
    pub async fn get_action_routing(&self, instance_id: u32) -> Result<Vec<ActionRouting>> {
        let routing = sqlx::query_as::<_, ActionRouting>(
            r#"
            SELECT * FROM action_routing
            WHERE instance_id = ? AND enabled = TRUE
            ORDER BY action_id
            "#,
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(routing)
    }

    /// Validate a measurement routing entry
    pub async fn validate_measurement_routing(
        &self,
        routing: &MeasurementRoutingRow,
        instance_name: &str,
    ) -> Result<ValidationResult> {
        self.validate_routing_impl(
            instance_name,
            routing.measurement_id,
            "measurement",
            &routing.channel_type,
            |ct| ct.is_input(),
            "T or S",
            |product| {
                product
                    .measurements
                    .iter()
                    .any(|m| m.measurement_id == routing.measurement_id)
            },
        )
        .await
    }

    /// Validate an action routing entry
    pub async fn validate_action_routing(
        &self,
        routing: &ActionRoutingRow,
        instance_name: &str,
    ) -> Result<ValidationResult> {
        self.validate_routing_impl(
            instance_name,
            routing.action_id,
            "action",
            &routing.channel_type,
            |ct| ct.is_output(),
            "C or A",
            |product| {
                product
                    .actions
                    .iter()
                    .any(|a| a.action_id == routing.action_id)
            },
        )
        .await
    }

    /// Common validation logic for routing entries (measurement or action)
    #[allow(clippy::too_many_arguments)]
    async fn validate_routing_impl(
        &self,
        instance_name: &str,
        point_id: u32,
        point_label: &str,
        channel_type: &Option<common::FourRemote>,
        is_valid_direction: impl Fn(&common::FourRemote) -> bool,
        direction_label: &str,
        check_point_in_product: impl FnOnce(&crate::config::Product) -> bool,
    ) -> Result<ValidationResult> {
        let mut errors = Vec::new();

        let instance_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM instances WHERE instance_name = ?)",
        )
        .bind(instance_name)
        .fetch_one(&self.pool)
        .await?;

        if !instance_exists {
            errors.push(format!("Instance {} does not exist", instance_name));
        }

        if let Some(ct) = channel_type
            && !is_valid_direction(ct)
        {
            errors.push(format!(
                "Invalid channel_type for {}: {}. Must be {}",
                point_label, ct, direction_label
            ));
        }

        let point_exists = if instance_exists {
            let product_name = sqlx::query_scalar::<_, String>(
                "SELECT product_name FROM instances WHERE instance_name = ?",
            )
            .bind(instance_name)
            .fetch_one(&self.pool)
            .await?;

            self.product_loader
                .get_product(&product_name)
                .as_ref()
                .map(check_point_in_product)
                .unwrap_or(false)
        } else {
            false
        };

        if !point_exists {
            errors.push(format!(
                "{} point {} not found for instance {}",
                point_label, point_id, instance_name
            ));
        }

        let mut result = ValidationResult::new(ValidationLevel::Business);
        for error in errors {
            result.add_error(error);
        }
        Ok(result)
    }

    /// Delete all routing for an instance
    ///
    /// Removes all measurement and action routing entries for the specified instance.
    /// Also clears all shared_slots from InstanceIndex.
    pub async fn delete_all_routing(&self, instance_id: u32) -> Result<(u64, u64)> {
        let measurement_result =
            sqlx::query("DELETE FROM measurement_routing WHERE instance_id = ?")
                .bind(instance_id as i64)
                .execute(&self.pool)
                .await?;

        let action_result = sqlx::query("DELETE FROM action_routing WHERE instance_id = ?")
            .bind(instance_id as i64)
            .execute(&self.pool)
            .await?;

        // Dynamic Slot Allocation: Clear all shared_slots for this instance
        // Note: We don't remove the instance from InstanceIndex, just clear its shared_slots
        if measurement_result.rows_affected() > 0
            && let Some(index) = self.dynamic_instance_index()
        {
            match index.clear_shared_slots(instance_id) {
                Ok(cleared_count) if cleared_count > 0 => {
                    debug!(
                        "Inst{} cleared {} shared_slots (routing deleted)",
                        instance_id, cleared_count
                    );
                },
                Ok(_) => {},
                Err(e) => {
                    // Instance not found in InstanceIndex - might not be using dynamic allocation
                    debug!("Inst{} clear_shared_slots skipped: {}", instance_id, e);
                },
            }
        }

        Ok((
            measurement_result.rows_affected(),
            action_result.rows_affected(),
        ))
    }
}

// ============================================================================
// Unit Tests for Instance Routing
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use crate::product_loader::ProductLoader;
    use aether_routing::RoutingCache;
    use common::FourRemote;
    use sqlx::SqlitePool;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::TempDir;

    // Helper: Create test database with full automation schema
    async fn create_test_database() -> (TempDir, SqlitePool) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_routing.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        let pool = SqlitePool::connect(&db_url).await.unwrap();

        // Use standard automation schema
        common::test_utils::schema::init_automation_schema(&pool)
            .await
            .unwrap();

        // Note: measurement_points and action_points tables are no longer needed.
        // Validation tests now use built-in product definitions from aether-model crate
        // (Battery has 19 measurements + 3 actions, PCS has its own set, etc.)

        (temp_dir, pool)
    }

    // Helper: Create InstanceManager for testing
    fn create_test_instance_manager(pool: SqlitePool) -> InstanceManager {
        let routing_cache = Arc::new(RoutingCache::new());
        let product_loader = Arc::new(ProductLoader::new(pool.clone()));

        let dispatch: Arc<dyn crate::infra::shm_dispatch::ActionDispatch> =
            Arc::new(crate::infra::shm_dispatch::NoopDispatch);
        InstanceManager::new(pool, routing_cache, product_loader, dispatch)
    }

    // Helper: Create a test instance
    async fn create_test_instance(
        manager: &InstanceManager,
        instance_id: u32,
        instance_name: &str,
        product_name: &str,
        parent_id: Option<u32>,
    ) {
        let req = crate::product_loader::CreateInstanceRequest {
            instance_id: Some(instance_id),
            instance_name: instance_name.to_string(),
            product_name: product_name.to_string(),
            parent_id,
            properties: HashMap::new(),
        };
        manager
            .create_instance(req)
            .await
            .expect("Failed to create instance");
    }

    /// Setup standard hierarchy: Station(1) -> ESS(2), returns ESS instance_id
    async fn setup_hierarchy(manager: &InstanceManager) -> u32 {
        let station_req = crate::product_loader::CreateInstanceRequest {
            instance_id: Some(1),
            instance_name: "station_root".to_string(),
            product_name: "Station".to_string(),
            parent_id: None,
            properties: HashMap::new(),
        };
        manager
            .create_instance(station_req)
            .await
            .expect("Failed to create Station");

        let ess_req = crate::product_loader::CreateInstanceRequest {
            instance_id: Some(2),
            instance_name: "ess_parent".to_string(),
            product_name: "ESS".to_string(),
            parent_id: Some(1),
            properties: HashMap::new(),
        };
        manager
            .create_instance(ess_req)
            .await
            .expect("Failed to create ESS");

        2
    }

    // Helper: Create a test channel in the database
    async fn create_test_channel(pool: &SqlitePool, channel_id: i32, name: &str) {
        sqlx::query(
            r#"INSERT INTO channels (channel_id, name, protocol, enabled)
               VALUES (?, ?, 'Virtual', 1)"#,
        )
        .bind(channel_id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    // ========== upsert_measurement_routing tests ==========

    #[tokio::test]
    async fn test_upsert_measurement_routing_success() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        // Setup hierarchy: Station -> ESS, then create Battery under ESS
        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;

        // Create channel (required for routing FK)
        create_test_channel(&pool, 3001, "test_channel").await;

        // Create routing
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3001),
            four_remote: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            enabled: true,
        };

        let result = manager.upsert_measurement_routing(1001, 1, request).await;

        assert!(result.is_ok(), "Upsert should succeed: {:?}", result.err());

        // Verify routing was created
        let routing = manager.get_measurement_routing(1001).await.unwrap();
        assert_eq!(routing.len(), 1);
        assert_eq!(routing[0].measurement_id, 1);
        assert_eq!(routing[0].channel_id, Some(3001));
    }

    #[tokio::test]
    async fn test_upsert_measurement_routing_invalid_channel_type() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;

        // Try to create measurement routing with Control type (invalid)
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3001),
            four_remote: Some(FourRemote::Control), // Invalid for measurement
            channel_point_id: Some(101),
            enabled: true,
        };

        let result = manager.upsert_measurement_routing(1001, 1, request).await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid channel_type")
        );
    }

    #[tokio::test]
    async fn test_upsert_measurement_routing_unbound() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;

        // Create unbound routing (all channel fields are None, enabled=true so we can verify)
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: None,
            four_remote: None,
            channel_point_id: None,
            enabled: true, // Enable so get_measurement_routing returns it
        };

        let result = manager.upsert_measurement_routing(1001, 1, request).await;

        assert!(result.is_ok());

        // Verify unbound routing via get_measurement_routing (only returns enabled=true)
        let routing = manager.get_measurement_routing(1001).await.unwrap();
        assert_eq!(routing.len(), 1);
        assert!(routing[0].channel_id.is_none());
        assert!(routing[0].channel_type.is_none());
        assert!(routing[0].channel_point_id.is_none());
    }

    // ========== upsert_action_routing tests ==========

    #[tokio::test]
    async fn test_upsert_action_routing_success() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        // Setup hierarchy and use PCS product which has action points
        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 2001, "pcs_test", "PCS", Some(ess_id)).await;

        // Create channel
        create_test_channel(&pool, 3002, "test_channel_2").await;

        // Create action routing with Adjustment type
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3002),
            four_remote: Some(FourRemote::Adjustment),
            channel_point_id: Some(201),
            enabled: true,
        };

        let result = manager.upsert_action_routing(2001, 1, request).await;

        assert!(result.is_ok(), "Upsert should succeed: {:?}", result.err());

        // Verify routing was created
        let routing = manager.get_action_routing(2001).await.unwrap();
        assert_eq!(routing.len(), 1);
        assert_eq!(routing[0].action_id, 1);
    }

    #[tokio::test]
    async fn test_upsert_action_routing_invalid_channel_type() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 2001, "pcs_test", "PCS", Some(ess_id)).await;

        // Try to create action routing with Telemetry type (invalid)
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3002),
            four_remote: Some(FourRemote::Telemetry), // Invalid for action
            channel_point_id: Some(201),
            enabled: true,
        };

        let result = manager.upsert_action_routing(2001, 1, request).await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid channel_type")
        );
    }

    // ========== delete_measurement_routing tests ==========

    #[tokio::test]
    async fn test_delete_measurement_routing_success() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;
        create_test_channel(&pool, 3001, "test_channel").await;

        // Create routing first
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3001),
            four_remote: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            enabled: true,
        };
        manager
            .upsert_measurement_routing(1001, 1, request)
            .await
            .unwrap();

        // Delete the routing
        let rows_affected = manager.delete_measurement_routing(1001, 1).await.unwrap();

        assert_eq!(rows_affected, 1);

        // Verify routing was deleted
        let routing = manager.get_measurement_routing(1001).await.unwrap();
        assert!(routing.is_empty());
    }

    #[tokio::test]
    async fn test_delete_measurement_routing_not_found() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;

        // Try to delete non-existent routing
        let rows_affected = manager.delete_measurement_routing(1001, 999).await.unwrap();

        assert_eq!(rows_affected, 0);
    }

    // ========== toggle_routing tests ==========

    #[tokio::test]
    async fn test_toggle_measurement_routing() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;
        create_test_channel(&pool, 3001, "test_channel").await;

        // Create routing (enabled)
        let request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3001),
            four_remote: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            enabled: true,
        };
        manager
            .upsert_measurement_routing(1001, 1, request)
            .await
            .unwrap();

        // Verify initially enabled
        let routing = manager.get_measurement_routing(1001).await.unwrap();
        assert_eq!(routing.len(), 1);
        assert!(routing[0].enabled);

        // Toggle to disabled
        let rows_affected = manager
            .toggle_measurement_routing(1001, 1, false)
            .await
            .unwrap();

        assert_eq!(rows_affected, 1);

        // Verify routing is now disabled (use raw SQL since get_measurement_routing only returns enabled)
        let enabled: bool = sqlx::query_scalar(
            "SELECT enabled FROM measurement_routing WHERE instance_id = ? AND measurement_id = ?",
        )
        .bind(1001i32)
        .bind(1u32)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!enabled, "Routing should be disabled after toggle");

        // get_measurement_routing should return empty for disabled routing
        let routing = manager.get_measurement_routing(1001).await.unwrap();
        assert!(routing.is_empty(), "No enabled routing should exist");

        // Toggle back to enabled
        manager
            .toggle_measurement_routing(1001, 1, true)
            .await
            .unwrap();

        // Now get_measurement_routing should return the routing
        let routing = manager.get_measurement_routing(1001).await.unwrap();
        assert_eq!(routing.len(), 1);
        assert!(routing[0].enabled);
    }

    // ========== validate_routing tests ==========

    #[tokio::test]
    async fn test_validate_measurement_routing_valid() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;
        create_test_channel(&pool, 3001, "test_channel").await;

        // Create valid routing row
        let routing_row = MeasurementRoutingRow {
            channel_id: Some(3001),
            channel_type: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            measurement_id: 1, // Battery has measurement point 1
        };

        let result = manager
            .validate_measurement_routing(&routing_row, "battery_test")
            .await
            .unwrap();

        assert!(result.is_valid);
        assert!(result.errors.is_empty());
    }

    #[tokio::test]
    async fn test_validate_measurement_routing_invalid_point() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 1001, "battery_test", "Battery", Some(ess_id)).await;
        create_test_channel(&pool, 3001, "test_channel").await;

        // Create routing row with non-existent measurement point
        let routing_row = MeasurementRoutingRow {
            channel_id: Some(3001),
            channel_type: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            measurement_id: 9999, // Invalid measurement ID
        };

        let result = manager
            .validate_measurement_routing(&routing_row, "battery_test")
            .await
            .unwrap();

        assert!(!result.is_valid);
        assert!(!result.errors.is_empty());
    }

    // ========== delete_all_routing tests ==========

    #[tokio::test]
    async fn test_delete_all_routing() {
        let (_temp_dir, pool) = create_test_database().await;
        let manager = create_test_instance_manager(pool.clone());

        // Setup hierarchy and use PCS which has both measurement and action points
        let ess_id = setup_hierarchy(&manager).await;
        create_test_instance(&manager, 2001, "pcs_test", "PCS", Some(ess_id)).await;
        create_test_channel(&pool, 3001, "test_channel").await;

        // Create measurement routing
        let m_request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3001),
            four_remote: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            enabled: true,
        };
        manager
            .upsert_measurement_routing(2001, 1, m_request)
            .await
            .unwrap();

        // Create action routing
        let a_request = crate::dto::SinglePointRoutingRequest {
            channel_id: Some(3001),
            four_remote: Some(FourRemote::Adjustment),
            channel_point_id: Some(201),
            enabled: true,
        };
        manager
            .upsert_action_routing(2001, 1, a_request)
            .await
            .unwrap();

        // Delete all routing
        let (m_deleted, a_deleted) = manager.delete_all_routing(2001).await.unwrap();

        assert_eq!(m_deleted, 1);
        assert_eq!(a_deleted, 1);

        // Verify all routing is deleted
        let m_routing = manager.get_measurement_routing(2001).await.unwrap();
        let a_routing = manager.get_action_routing(2001).await.unwrap();
        assert!(m_routing.is_empty());
        assert!(a_routing.is_empty());
    }
}
