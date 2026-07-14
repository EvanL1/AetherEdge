//! Instance Manager - Core Lifecycle Operations
//!
//! This module provides the core instance lifecycle management.
//! Extended functionality is provided in separate modules:
//! - `instance_routing.rs` - Routing CRUD operations
//! - `instance_data.rs` - Data loading and querying

use anyhow::{Result, anyhow};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tracing::{debug, warn};

use crate::config::TopologyNode;
use crate::product_loader::{Instance, ProductLoader};

/// Row type returned by SQLite instance queries
/// Row shape for instance SELECTs (post v5 migration, no `properties` column).
type InstanceRow = (u32, String, String, Option<u32>, String);

/// Build a partial Instance from a database row.
///
/// `core.properties` is left empty here — callers must fill it with
/// `fill_properties` / `fill_properties_batch` after the SELECT. We do not
/// load properties inside this helper to avoid hidden N+1 queries when
/// building a list of instances.
fn build_instance_from_row(row: InstanceRow) -> Result<Instance> {
    let (instance_id, instance_name, product_name, parent_id, _created_at) = row;
    Ok(Instance {
        core: crate::config::InstanceCore {
            instance_id,
            instance_name,
            product_name,
            parent_id,
            properties: HashMap::new(),
        },
        created_at: None,
    })
}

/// Escape SQL LIKE metacharacters (`%`, `_`, `\`) so user input is treated as literal text.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Instance Manager handles runtime instance lifecycle
pub struct InstanceManager {
    pub(crate) pool: SqlitePool,
    pub(crate) product_loader: Arc<ProductLoader>,
    /// Instance name → instance_id cache (for fast API lookups)
    /// Atomically replaceable instance-name index. Readers see either the
    /// complete previous SQLite projection or the complete replacement; a
    /// reconciliation never exposes a partially rebuilt map.
    pub(crate) name_cache: arc_swap::ArcSwap<HashMap<String, u32>>,
    /// Production runtime view that pins point, health, and logical routing
    /// to one service generation.
    pub(crate) runtime_topology:
        OnceLock<Arc<crate::infra::runtime_topology::AutomationTopologyHandle>>,
}

impl InstanceManager {
    pub fn new(pool: SqlitePool, product_loader: Arc<ProductLoader>) -> Self {
        Self {
            pool,
            product_loader,
            name_cache: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            runtime_topology: OnceLock::new(),
        }
    }

    /// Installs the service-owned coherent runtime topology exactly once.
    pub fn set_runtime_topology(
        &self,
        topology: Arc<crate::infra::runtime_topology::AutomationTopologyHandle>,
    ) -> aether_ports::PortResult<()> {
        self.runtime_topology.set(topology).map_err(|_| {
            aether_ports::PortError::new(
                aether_ports::PortErrorKind::Conflict,
                "automation runtime topology is already configured",
            )
        })
    }

    /// Returns the coherent production topology when composition is complete.
    #[must_use]
    pub fn runtime_topology(
        &self,
    ) -> Option<&Arc<crate::infra::runtime_topology::AutomationTopologyHandle>> {
        self.runtime_topology.get()
    }

    /// Load per-instance property values from `instance_properties`, resolving
    /// each `property_id` back to its `name` via the product PropertyTemplate
    /// (selected runtime definitions). Returns `name -> value` for use as
    /// `InstanceCore.properties`.
    pub(crate) async fn fetch_properties(
        &self,
        instance_id: u32,
        product_name: &str,
    ) -> Result<HashMap<String, serde_json::Value>> {
        let product = self
            .product_loader
            .get_product(product_name)
            .map_err(|e| anyhow!("Product '{}' not found: {}", product_name, e))?;

        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT property_id, value_json FROM instance_properties WHERE instance_id = ?",
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = HashMap::with_capacity(rows.len());
        for (property_id, value_json) in rows {
            let Some(tpl) = product
                .properties
                .iter()
                .find(|p| i64::from(p.property_id) == property_id)
            else {
                warn!(
                    "Instance {} has property_id={} not in product '{}' template, dropping from response",
                    instance_id, property_id, product_name
                );
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&value_json).map_err(|e| {
                anyhow!(
                    "Invalid value_json for instance {} property {}: {}",
                    instance_id,
                    property_id,
                    e
                )
            })?;
            out.insert(tpl.name.clone(), value);
        }
        Ok(out)
    }

    /// Bulk variant of `fetch_properties` — one query for all instances, then
    /// group by `instance_id`. Used by `list_instances_paginated` /
    /// `search_instances` / `get_children` to avoid N+1.
    pub(crate) async fn fetch_properties_batch(
        &self,
        instances: &[(u32, String)],
    ) -> Result<HashMap<u32, HashMap<String, serde_json::Value>>> {
        if instances.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = instances.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT instance_id, property_id, value_json FROM instance_properties \
             WHERE instance_id IN ({})",
            placeholders
        );
        let mut q = sqlx::query_as::<_, (i64, i64, String)>(&sql);
        for (id, _) in instances {
            q = q.bind(*id as i64);
        }
        let rows = q.fetch_all(&self.pool).await?;

        // Group rows by instance_id, resolve property_id -> name per product.
        let product_by_instance: HashMap<u32, &str> =
            instances.iter().map(|(id, p)| (*id, p.as_str())).collect();
        let mut out: HashMap<u32, HashMap<String, serde_json::Value>> = HashMap::new();
        for (instance_id, property_id, value_json) in rows {
            let instance_id = instance_id as u32;
            let Some(product_name) = product_by_instance.get(&instance_id) else {
                continue;
            };
            let Ok(product) = self.product_loader.get_product(product_name) else {
                continue;
            };
            let Some(tpl) = product
                .properties
                .iter()
                .find(|p| i64::from(p.property_id) == property_id)
            else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&value_json).map_err(|e| {
                anyhow!(
                    "Invalid value_json for instance {} property {}: {}",
                    instance_id,
                    property_id,
                    e
                )
            })?;
            out.entry(instance_id)
                .or_default()
                .insert(tpl.name.clone(), value);
        }
        Ok(out)
    }

    /// Hydrate `core.properties` on each instance in a slice using one batch
    /// query — used after the SELECT in list/search/get_children paths.
    pub(crate) async fn attach_properties_batch(&self, instances: &mut [Instance]) -> Result<()> {
        if instances.is_empty() {
            return Ok(());
        }
        let lookup: Vec<(u32, String)> = instances
            .iter()
            .map(|i| (i.core.instance_id, i.core.product_name.clone()))
            .collect();
        let mut grouped = self.fetch_properties_batch(&lookup).await?;
        for inst in instances {
            if let Some(map) = grouped.remove(&inst.core.instance_id) {
                inst.core.properties = map;
            }
        }
        Ok(())
    }

    /// Persist a properties map for an instance: validates each key against
    /// the product's PropertyTemplate, then `INSERT OR REPLACE`s one row per
    /// recognised key. Unknown keys are rejected (returns Err). Pass an
    /// existing transaction so the write joins the surrounding atomic op.
    pub(crate) async fn write_properties_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        instance_id: u32,
        product_name: &str,
        properties: &HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        if properties.is_empty() {
            return Ok(());
        }
        let product = self
            .product_loader
            .get_product(product_name)
            .map_err(|e| anyhow!("Product '{}' not found: {}", product_name, e))?;

        for (name, value) in properties {
            let Some(tpl) = product.properties.iter().find(|p| p.name == *name) else {
                return Err(anyhow!(
                    "Property '{}' not declared by product '{}' template",
                    name,
                    product_name
                ));
            };
            let value_json = serde_json::to_string(value)?;
            sqlx::query(
                "INSERT INTO instance_properties (instance_id, property_id, value_json) \
                 VALUES (?, ?, ?) \
                 ON CONFLICT(instance_id, property_id) DO UPDATE SET \
                    value_json = excluded.value_json, \
                    updated_at = CURRENT_TIMESTAMP",
            )
            .bind(instance_id as i64)
            .bind(i64::from(tpl.property_id))
            .bind(value_json)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// Get the SQLite pool reference
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Reconcile routing from the authoritative SQLite topology.
    ///
    /// Production publishes routing together with the validated point/health
    /// generation. A composition without that runtime may validate durable
    /// rows, but it publishes no alternate in-memory routing owner.
    pub async fn refresh_routing(&self) -> anyhow::Result<usize> {
        if let Some(topology) = self.runtime_topology.get() {
            topology
                .refresh_or_revoke_commands(&self.pool)
                .await
                .map_err(|error| anyhow!(error.to_string()))?;
            return Ok(topology.load().route_count());
        }
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT (SELECT COUNT(*) FROM measurement_routing WHERE enabled = 1) + \
                    (SELECT COUNT(*) FROM action_routing WHERE enabled = 1)",
        )
        .fetch_one(&self.pool)
        .await?;
        usize::try_from(count).map_err(|_| anyhow!("logical route count is outside usize range"))
    }

    /// Get the product loader reference
    ///
    /// Returns a reference to the product loader for accessing product templates.
    pub fn product_loader(&self) -> &ProductLoader {
        &self.product_loader
    }

    // ============================================================================
    // Instance name → ID translation methods
    // ============================================================================

    /// Get instance_id by instance_name (with caching)
    ///
    /// This method provides fast lookup of instance IDs from names, using a
    /// DashMap cache for sub-microsecond performance on cache hits.
    ///
    /// # Cache Strategy
    /// - Cache hit: Returns immediately (~100ns)
    /// - Cache miss: Queries SQLite and updates cache
    pub async fn get_instance_id(&self, instance_name: &str) -> Result<u32> {
        // 1. Fast path: Check cache first
        if let Some(id) = self.name_cache.load().get(instance_name) {
            return Ok(*id);
        }

        // 2. Slow path: Query database
        let id: u32 =
            sqlx::query_scalar("SELECT instance_id FROM instances WHERE instance_name = ?")
                .bind(instance_name)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| anyhow!("Instance not found: {}", instance_name))?;

        // 3. Update cache for next time
        self.update_name_cache(instance_name.to_string(), id);

        Ok(id)
    }

    /// Populate the name→id cache from database at startup
    ///
    /// This should be called once after creating the InstanceManager to pre-warm
    /// the cache with all existing instances.
    pub async fn populate_name_cache(&self) -> Result<()> {
        let instances: Vec<(String, u32)> =
            sqlx::query_as("SELECT instance_name, instance_id FROM instances")
                .fetch_all(&self.pool)
                .await?;

        let count = instances.len();
        self.name_cache
            .store(Arc::new(instances.into_iter().collect()));

        debug!("Name->ID cache: {} entries", count);
        Ok(())
    }

    /// Update cache entry (called on instance create/rename)
    pub fn update_name_cache(&self, instance_name: String, instance_id: u32) {
        self.name_cache.rcu(|current| {
            let mut replacement = (**current).clone();
            replacement.insert(instance_name.clone(), instance_id);
            Arc::new(replacement)
        });
    }

    /// Remove entry from cache (called on instance delete)
    pub fn remove_from_name_cache(&self, instance_name: &str) {
        self.name_cache.rcu(|current| {
            let mut replacement = (**current).clone();
            replacement.remove(instance_name);
            Arc::new(replacement)
        });
    }

    /// List instances with pagination
    ///
    /// Uses SQL `? IS NULL OR product_name = ?` pattern to handle optional filter
    /// in a single query without Rust-side branching.
    pub async fn list_instances_paginated(
        &self,
        product_name: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<(u32, Vec<Instance>)> {
        let offset = (page - 1) * page_size;

        let (total,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM instances WHERE (? IS NULL OR product_name = ?)")
                .bind(product_name)
                .bind(product_name)
                .fetch_one(&self.pool)
                .await?;

        let rows: Vec<InstanceRow> = sqlx::query_as(
            r#"SELECT instance_id, instance_name, product_name, parent_id, created_at
               FROM instances
               WHERE (? IS NULL OR product_name = ?)
               ORDER BY instance_id ASC
               LIMIT ? OFFSET ?"#,
        )
        .bind(product_name)
        .bind(product_name)
        .bind(page_size as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut instances = rows
            .into_iter()
            .map(build_instance_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_properties_batch(&mut instances).await?;

        Ok((u32::try_from(total).unwrap_or(u32::MAX), instances))
    }

    /// Search instances by name with fuzzy matching
    ///
    /// Uses SQL `? IS NULL OR product_name = ?` pattern to handle optional filter
    /// in a single query without Rust-side branching.
    pub async fn search_instances(
        &self,
        keyword: &str,
        product_name: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<(u32, Vec<Instance>)> {
        let offset = (page - 1) * page_size;
        let like_pattern = format!("%{}%", escape_like(keyword));

        let (total,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM instances WHERE instance_name LIKE ? ESCAPE '\\' AND (? IS NULL OR product_name = ?)",
        )
        .bind(&like_pattern)
        .bind(product_name)
        .bind(product_name)
        .fetch_one(&self.pool)
        .await?;

        let rows: Vec<InstanceRow> = sqlx::query_as(
            r#"SELECT instance_id, instance_name, product_name, parent_id, created_at
               FROM instances
               WHERE instance_name LIKE ? ESCAPE '\' AND (? IS NULL OR product_name = ?)
               ORDER BY instance_id ASC
               LIMIT ? OFFSET ?"#,
        )
        .bind(&like_pattern)
        .bind(product_name)
        .bind(product_name)
        .bind(page_size as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut instances = rows
            .into_iter()
            .map(build_instance_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_properties_batch(&mut instances).await?;

        Ok((u32::try_from(total).unwrap_or(u32::MAX), instances))
    }

    /// Get instance by ID
    pub async fn get_instance(&self, instance_id: u32) -> Result<Instance> {
        let row = sqlx::query_as::<_, (String, String, Option<u32>, String)>(
            r#"
            SELECT instance_name, product_name, parent_id, created_at
            FROM instances
            WHERE instance_id = ?
            "#,
        )
        .bind(instance_id as i64)
        .fetch_optional(&self.pool)
        .await?;

        let row = row.ok_or_else(|| anyhow!("Instance not found: {}", instance_id))?;

        let (instance_name, product_name, parent_id, _created_at) = row;
        let properties = self.fetch_properties(instance_id, &product_name).await?;

        Ok(Instance {
            core: crate::config::InstanceCore {
                instance_id,
                instance_name,
                product_name,
                parent_id,
                properties,
            },
            created_at: None,
        })
    }

    // ============================================================================
    // Topology Query Methods
    // ============================================================================

    /// Get direct child instances of a given parent
    pub async fn get_children(&self, instance_id: u32) -> Result<Vec<Instance>> {
        let rows: Vec<InstanceRow> = sqlx::query_as(
            r#"
                SELECT instance_id, instance_name, product_name, parent_id, created_at
                FROM instances
                WHERE parent_id = ?
                ORDER BY instance_id ASC
                "#,
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut instances = rows
            .into_iter()
            .map(build_instance_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_properties_batch(&mut instances).await?;
        Ok(instances)
    }

    /// Get full topology tree starting from all root instances (Station)
    ///
    /// Returns a flat list of topology nodes with parent_id for tree reconstruction.
    pub async fn get_topology_tree(&self) -> Result<Vec<TopologyNode>> {
        let rows: Vec<(u32, String, String, Option<u32>)> = sqlx::query_as(
            r#"
            SELECT instance_id, instance_name, product_name, parent_id
            FROM instances
            ORDER BY parent_id NULLS FIRST, instance_id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(instance_id, instance_name, product_name, parent_id)| TopologyNode {
                    instance_id,
                    instance_name,
                    product_name,
                    parent_id,
                },
            )
            .collect())
    }
}
