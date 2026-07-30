//! Instance Manager - Core Lifecycle Operations
//!
//! This module provides the core instance lifecycle management.
//! Extended functionality is provided in separate modules:
//! - `instance_routing.rs` - Routing CRUD operations
//! - `instance_data.rs` - Data loading and querying

use anyhow::{Result, anyhow};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

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

fn pagination_offset(page: u32, page_size: u32) -> Result<u64> {
    if page == 0 || page_size == 0 {
        return Err(anyhow!("page and page_size must be positive"));
    }
    Ok(u64::from(page - 1) * u64::from(page_size))
}

/// Instance Manager handles runtime instance lifecycle
pub struct InstanceManager {
    pub(crate) pool: SqlitePool,
    pub(crate) product_loader: Arc<ProductLoader>,
    /// Production runtime view that pins point, health, and logical routing
    /// to one service generation.
    pub(crate) runtime_topology: Arc<crate::infra::runtime_topology::AutomationTopologyHandle>,
}

impl InstanceManager {
    pub fn new(
        pool: SqlitePool,
        product_loader: Arc<ProductLoader>,
        runtime_topology: Arc<crate::infra::runtime_topology::AutomationTopologyHandle>,
    ) -> Self {
        Self {
            pool,
            product_loader,
            runtime_topology,
        }
    }

    /// Returns the mandatory coherent production topology.
    #[must_use]
    pub fn runtime_topology(
        &self,
    ) -> &Arc<crate::infra::runtime_topology::AutomationTopologyHandle> {
        &self.runtime_topology
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
            .get_definition(product_name)
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
                .find(|p| i64::from(p.id) == property_id)
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
        instances: &[(u32, &str)],
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
        let product_by_instance: HashMap<u32, &str> = instances
            .iter()
            .map(|(id, product)| (*id, *product))
            .collect();
        let mut out: HashMap<u32, HashMap<String, serde_json::Value>> = HashMap::new();
        for (instance_id, property_id, value_json) in rows {
            let instance_id = instance_id as u32;
            let Some(product_name) = product_by_instance.get(&instance_id) else {
                continue;
            };
            let Ok(product) = self.product_loader.get_definition(product_name) else {
                continue;
            };
            let Some(tpl) = product
                .properties
                .iter()
                .find(|p| i64::from(p.id) == property_id)
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
        let lookup: Vec<(u32, &str)> = instances
            .iter()
            .map(|i| (i.core.instance_id, i.core.product_name.as_str()))
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
            .get_definition(product_name)
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
            .bind(i64::from(tpl.id))
            .bind(value_json)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// Reconcile routing from the authoritative SQLite topology.
    ///
    /// Production publishes routing together with the validated point/health
    /// generation.
    pub async fn refresh_routing(&self) -> anyhow::Result<usize> {
        self.runtime_topology
            .refresh_or_revoke_commands(&self.pool)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(self.runtime_topology.load().route_count())
    }

    /// Get the product loader reference
    ///
    /// Returns a reference to the product loader for accessing product templates.
    pub fn product_loader(&self) -> &Arc<ProductLoader> {
        &self.product_loader
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
        let offset = pagination_offset(page, page_size)?;

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
        .bind(i64::try_from(offset)?)
        .fetch_all(&self.pool)
        .await?;

        let mut instances = rows
            .into_iter()
            .map(build_instance_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_properties_batch(&mut instances).await?;

        Ok((u32::try_from(total).unwrap_or(u32::MAX), instances))
    }

    /// List every commissioned instance in stable identifier order.
    pub async fn list_instances(&self) -> Result<Vec<Instance>> {
        let rows: Vec<InstanceRow> = sqlx::query_as(
            "SELECT instance_id, instance_name, product_name, parent_id, created_at \
             FROM instances ORDER BY instance_id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut instances = rows
            .into_iter()
            .map(build_instance_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_properties_batch(&mut instances).await?;
        Ok(instances)
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
        let offset = pagination_offset(page, page_size)?;
        let like_pattern = format!("%{}%", escape_like(keyword));

        let (total,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM instances WHERE instance_name LIKE ? ESCAPE '\\' AND (? IS NULL OR product_name = ?)",
        )
        .bind(&like_pattern)
        .bind(product_name)
        .bind(product_name)
        .fetch_one(&self.pool)
        .await?;

        let instances = self
            .query_instances(keyword, product_name, &[], page_size, offset)
            .await?;

        Ok((u32::try_from(total).unwrap_or(u32::MAX), instances))
    }

    /// Find a bounded set of instances in one SQLite query.
    pub(crate) async fn find_instances(
        &self,
        keyword: &str,
        instance_ids: &[u32],
        limit: u32,
    ) -> Result<Vec<Instance>> {
        self.query_instances(keyword, None, instance_ids, limit, 0)
            .await
    }

    /// List the minimal instance identities without loading product properties.
    pub(crate) async fn list_instance_identities(&self) -> Result<Vec<(u32, String)>> {
        Ok(
            sqlx::query_as("SELECT instance_id, instance_name FROM instances ORDER BY instance_id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn query_instances(
        &self,
        keyword: &str,
        product_name: Option<&str>,
        instance_ids: &[u32],
        limit: u32,
        offset: u64,
    ) -> Result<Vec<Instance>> {
        let like_pattern = format!("%{}%", escape_like(keyword));
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT instance_id, instance_name, product_name, parent_id, created_at \
             FROM instances WHERE instance_name LIKE ",
        );
        query.push_bind(like_pattern).push(" ESCAPE '\\'");
        if let Some(product_name) = product_name {
            query.push(" AND product_name = ").push_bind(product_name);
        }
        if !instance_ids.is_empty() {
            query.push(" AND instance_id IN (");
            let mut ids = query.separated(", ");
            for instance_id in instance_ids {
                ids.push_bind(i64::from(*instance_id));
            }
            ids.push_unseparated(")");
        }
        query
            .push(" ORDER BY instance_id ASC LIMIT ")
            .push_bind(i64::from(limit))
            .push(" OFFSET ")
            .push_bind(i64::try_from(offset)?);

        let rows = query
            .build_query_as::<InstanceRow>()
            .fetch_all(&self.pool)
            .await?;
        let mut instances = rows
            .into_iter()
            .map(build_instance_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_properties_batch(&mut instances).await?;
        Ok(instances)
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
