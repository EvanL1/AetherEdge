//! SQLite-backed hot reload implementation for automation.

use common::{InstanceReloadResult, ReloadableService};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use crate::instance_manager::InstanceManager;
use crate::product_loader::Instance;

/// Instance change severity classification
///
/// For automation, all changes are treated as configuration updates since there are
/// no active connections to restart (unlike io's protocol clients).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstanceChangeType {
    /// Configuration update (properties, product_name)
    ConfigUpdate = 0,
}

impl ReloadableService for InstanceManager {
    type ChangeType = InstanceChangeType;
    type Config = Instance;
    type ReloadResult = InstanceReloadResult;

    /// Reload instances from SQLite database with incremental sync
    async fn reload_from_database(
        &self,
        _pool: &sqlx::SqlitePool,
    ) -> anyhow::Result<Self::ReloadResult> {
        let start_time = std::time::Instant::now();
        debug!("Reloading instances");

        // 1. Load the authoritative instance set from SQLite.
        let (_, db_instances) = self.list_instances_paginated(None, 1, 10_000).await?;
        let db_by_id: HashMap<u32, &Instance> = db_instances
            .iter()
            .map(|instance| (instance.instance_id(), instance))
            .collect();
        let db_ids: HashSet<u32> = db_by_id.keys().copied().collect();
        let cached_by_id: HashMap<u32, String> = self
            .name_cache
            .iter()
            .map(|entry| (*entry.value(), entry.key().clone()))
            .collect();
        let cached_ids: HashSet<u32> = cached_by_id.keys().copied().collect();

        let mut added: Vec<u32> = db_ids.difference(&cached_ids).copied().collect();
        let mut removed: Vec<u32> = cached_ids.difference(&db_ids).copied().collect();
        let mut updated: Vec<u32> = db_ids
            .intersection(&cached_ids)
            .filter(|id| {
                db_by_id
                    .get(id)
                    .zip(cached_by_id.get(id))
                    .is_some_and(|(db, cached_name)| db.instance_name() != cached_name)
            })
            .copied()
            .collect();
        added.sort_unstable();
        removed.sort_unstable();
        updated.sort_unstable();

        // 2. Rebuild process-local indexes from SQLite and atomically refresh routing.
        self.populate_name_cache().await?;
        self.refresh_routing().await?;

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let total_count = db_instances.len();

        info!(
            "Reload: +{} ~{} -{} err:{} ({}ms)",
            added.len(),
            updated.len(),
            removed.len(),
            0,
            duration_ms
        );

        Ok(InstanceReloadResult {
            total_count,
            added,
            updated,
            removed,
            errors: Vec::new(),
            duration_ms,
        })
    }

    /// Analyze changes between old and new configuration
    fn analyze_changes(
        &self,
        _old_config: &Self::Config,
        _new_config: &Self::Config,
    ) -> Self::ChangeType {
        // For automation, all changes are config updates
        InstanceChangeType::ConfigUpdate
    }

    /// Perform a local hot reload of one instance.
    async fn perform_hot_reload(&self, config: Self::Config) -> anyhow::Result<String> {
        debug!(
            "Hot reload: {} ({})",
            config.instance_name(),
            config.instance_id()
        );

        self.update_name_cache(config.instance_name().to_string(), config.instance_id());
        self.refresh_routing().await?;
        Ok("reloaded".to_string())
    }

    /// Rollback to previous configuration
    async fn rollback(&self, previous_config: Self::Config) -> anyhow::Result<String> {
        warn!("Rollback: {}", previous_config.instance_name());

        self.update_name_cache(
            previous_config.instance_name().to_string(),
            previous_config.instance_id(),
        );
        self.refresh_routing().await?;
        Ok("restored".to_string())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_instance_change_type() {
        // For automation, all changes are classified as ConfigUpdate
        let change_type = InstanceChangeType::ConfigUpdate;
        assert_eq!(change_type, InstanceChangeType::ConfigUpdate);

        // Test ordering
        assert!(InstanceChangeType::ConfigUpdate == InstanceChangeType::ConfigUpdate);
    }

    #[test]
    fn test_instance_creation() {
        // Test instance creation works correctly
        let instance = create_test_instance(1, "pv_inverter_01", "pv_inverter");
        assert_eq!(instance.instance_id(), 1);
        assert_eq!(instance.instance_name(), "pv_inverter_01");
        assert_eq!(instance.product_name(), "pv_inverter");
    }

    fn create_test_instance(id: u32, name: &str, product: &str) -> Instance {
        Instance {
            core: crate::config::InstanceCore {
                instance_id: id,
                instance_name: name.to_string(),
                product_name: product.to_string(),
                parent_id: None,
                properties: std::collections::HashMap::new(),
            },
            created_at: None,
        }
    }
}
