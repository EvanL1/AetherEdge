//! Read-only instance routing queries and validation.
//!
//! Logical routing mutations enter the typed measurement/action routing
//! applications. `InstanceManager` deliberately exposes no direct mutation
//! helper, including in test builds.

use anyhow::Result;
use common::{ValidationLevel, ValidationResult};

use crate::routing_loader::{
    ActionRouting, ActionRoutingRow, MeasurementRouting, MeasurementRoutingRow,
};

use super::instance_manager::InstanceManager;

impl InstanceManager {
    /// Get all enabled measurement routes for an instance.
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

    /// Get all enabled action routes for an instance.
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

    /// Validate a measurement routing entry against instance and product facts.
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
            |channel_type| channel_type.is_input(),
            "T or S",
            |product| {
                product
                    .measurements
                    .iter()
                    .any(|measurement| measurement.measurement_id == routing.measurement_id)
            },
        )
        .await
    }

    /// Validate an action routing entry against instance and product facts.
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
            |channel_type| channel_type.is_output(),
            "C or A",
            |product| {
                product
                    .actions
                    .iter()
                    .any(|action| action.action_id == routing.action_id)
            },
        )
        .await
    }

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
            errors.push(format!("Instance {instance_name} does not exist"));
        }

        if let Some(channel_type) = channel_type
            && !is_valid_direction(channel_type)
        {
            errors.push(format!(
                "Invalid channel_type for {point_label}: {channel_type}. Must be {direction_label}"
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
                "{point_label} point {point_id} not found for instance {instance_name}"
            ));
        }

        let mut result = ValidationResult::new(ValidationLevel::Business);
        for error in errors {
            result.add_error(error);
        }
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::sync::Arc;

    use common::FourRemote;
    use sqlx::SqlitePool;

    use super::*;

    async fn fixture() -> (tempfile::TempDir, InstanceManager) {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("routing-validation.db");
        let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", database.display()))
            .await
            .unwrap();
        common::test_utils::schema::init_automation_schema(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (1001, 'battery_test', 'Battery')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let loader = Arc::new(crate::product_loader::test_energy_product_loader(
            pool.clone(),
        ));
        (directory, InstanceManager::new(pool, loader))
    }

    #[tokio::test]
    async fn validates_existing_measurement_from_selected_product() {
        let (_directory, manager) = fixture().await;
        let routing = MeasurementRoutingRow {
            channel_id: Some(3001),
            channel_type: Some(FourRemote::Telemetry),
            channel_point_id: Some(101),
            measurement_id: 1,
        };

        let result = manager
            .validate_measurement_routing(&routing, "battery_test")
            .await
            .unwrap();

        assert!(result.is_valid);
        assert!(result.errors.is_empty());
    }

    #[tokio::test]
    async fn rejects_wrong_direction_and_unknown_product_point() {
        let (_directory, manager) = fixture().await;
        let routing = MeasurementRoutingRow {
            channel_id: Some(3001),
            channel_type: Some(FourRemote::Control),
            channel_point_id: Some(101),
            measurement_id: 9999,
        };

        let result = manager
            .validate_measurement_routing(&routing, "battery_test")
            .await
            .unwrap();

        assert!(!result.is_valid);
        assert_eq!(result.errors.len(), 2);
    }
}
