//! Instance Manager - Core Lifecycle Operations
//!
//! This module provides the core instance lifecycle management.
//! Extended functionality is provided in separate modules:
//! - `instance_routing.rs` - Routing CRUD operations
//! - `instance_data.rs` - Data loading and querying

use aether_model::validate_instance_name;
use anyhow::{Result, anyhow};
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::config::TopologyNode;
use crate::error::AutomationError;
use crate::infra::shm_dispatch::ActionDispatch;
use crate::product_loader::{CreateInstanceRequest, Instance, ProductLoader};

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
    pub(crate) routing_cache: Arc<aether_routing::RoutingCache>,
    pub(crate) product_loader: Arc<ProductLoader>,
    /// Instance name → instance_id cache (for fast API lookups)
    pub(crate) name_cache: DashMap<String, u32>,
    // ========== Dynamic Slot Allocation ==========
    /// Dynamic SHM slot management (optional, graceful no-ops when disabled)
    slot_runtime: crate::runtime::dynamic_slot_runtime::DynamicSlotRuntime,
    // ========== Action Dispatch (SHM/UDS or Noop) ==========
    /// Dispatches M2C action commands via SHM+UDS (production) or noop (tests)
    pub(crate) dispatch: Arc<dyn ActionDispatch>,
    /// Atomically replaceable SHM reader used by HTTP queries.
    pub(crate) live_reader: arc_swap::ArcSwapOption<aether_rtdb_shm::UnifiedReaderHandle>,
    /// Self-healing SHM reader for per-channel online/offline state.
    pub(crate) channel_health_reader:
        arc_swap::ArcSwapOption<aether_shm_bridge::ShmChannelHealthReader>,
}

impl InstanceManager {
    pub fn new(
        pool: SqlitePool,
        routing_cache: Arc<aether_routing::RoutingCache>,
        product_loader: Arc<ProductLoader>,
        dispatch: Arc<dyn ActionDispatch>,
    ) -> Self {
        Self {
            pool,
            routing_cache,
            product_loader,
            name_cache: DashMap::new(),
            slot_runtime: crate::runtime::dynamic_slot_runtime::DynamicSlotRuntime::new(),
            dispatch,
            live_reader: arc_swap::ArcSwapOption::empty(),
            channel_health_reader: arc_swap::ArcSwapOption::empty(),
        }
    }

    /// Publish the current SHM reader for live instance queries.
    pub fn set_live_reader(&self, reader: Arc<aether_rtdb_shm::UnifiedReaderHandle>) {
        self.live_reader.store(Some(reader));
    }

    /// Publish the self-healing channel-health reader used by the M2C gate.
    pub fn set_channel_health_reader(
        &self,
        reader: Arc<aether_shm_bridge::ShmChannelHealthReader>,
    ) {
        self.channel_health_reader.store(Some(reader));
    }

    /// Load per-instance property values from `instance_properties`, resolving
    /// each `property_id` back to its `name` via the product PropertyTemplate
    /// (compile-time constants). Returns `name -> value` for use as
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

    /// Upsert a single property value by `property_id`. Validates the id
    /// against the instance's product PropertyTemplate (400 on unknown id;
    /// 404 on unknown instance).
    pub async fn upsert_single_property(
        &self,
        instance_id: u32,
        property_id: i32,
        value: serde_json::Value,
    ) -> Result<crate::dto::InstancePropertyPoint, AutomationError> {
        let product_name: String =
            sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = ?")
                .bind(instance_id as i64)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| AutomationError::DatabaseError(format!("DB error: {}", e)))?
                .ok_or_else(|| AutomationError::InstanceNotFound(instance_id.to_string()))?;

        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|e| {
                AutomationError::InternalError(format!(
                    "Product '{}' not found: {}",
                    product_name, e
                ))
            })?;

        let tpl = product
            .properties
            .iter()
            .find(|p| p.property_id == property_id)
            .ok_or_else(|| {
                AutomationError::InvalidData(format!(
                    "property_id {} not in product '{}' template",
                    property_id, product_name
                ))
            })?
            .clone();

        let value_json = serde_json::to_string(&value)
            .map_err(|e| AutomationError::InternalError(format!("encode value: {}", e)))?;

        sqlx::query(
            "INSERT INTO instance_properties (instance_id, property_id, value_json) \
             VALUES (?, ?, ?) \
             ON CONFLICT(instance_id, property_id) DO UPDATE SET \
                value_json = excluded.value_json, \
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(instance_id as i64)
        .bind(i64::from(property_id))
        .bind(value_json)
        .execute(&self.pool)
        .await
        .map_err(|e| AutomationError::DatabaseError(format!("upsert property: {}", e)))?;

        Ok(crate::dto::InstancePropertyPoint {
            property_id: tpl.property_id,
            name: tpl.name,
            unit: tpl.unit,
            description: tpl.description,
            value: Some(value),
        })
    }

    /// Delete a single property value by `property_id`. Returns the template
    /// metadata with `value` absent (i.e. the post-delete state). 404 on
    /// unknown instance or property_id not in template.
    pub async fn delete_single_property(
        &self,
        instance_id: u32,
        property_id: i32,
    ) -> Result<crate::dto::InstancePropertyPoint, AutomationError> {
        let product_name: String =
            sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = ?")
                .bind(instance_id as i64)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| AutomationError::DatabaseError(format!("DB error: {}", e)))?
                .ok_or_else(|| AutomationError::InstanceNotFound(instance_id.to_string()))?;

        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|e| {
                AutomationError::InternalError(format!(
                    "Product '{}' not found: {}",
                    product_name, e
                ))
            })?;

        let tpl = product
            .properties
            .iter()
            .find(|p| p.property_id == property_id)
            .ok_or_else(|| {
                AutomationError::InvalidData(format!(
                    "property_id {} not in product '{}' template",
                    property_id, product_name
                ))
            })?
            .clone();

        sqlx::query("DELETE FROM instance_properties WHERE instance_id = ? AND property_id = ?")
            .bind(instance_id as i64)
            .bind(i64::from(property_id))
            .execute(&self.pool)
            .await
            .map_err(|e| AutomationError::DatabaseError(format!("delete property: {}", e)))?;

        Ok(crate::dto::InstancePropertyPoint {
            property_id: tpl.property_id,
            name: tpl.name,
            unit: tpl.unit,
            description: tpl.description,
            value: None,
        })
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

    /// Configure dynamic slot allocation (optional feature)
    ///
    /// Call this after construction to enable unified pool architecture.
    /// If not called, dynamic features are disabled (backward compatible).
    pub fn with_dynamic_allocation(
        mut self,
        dynamic_index: Arc<aether_rtdb_shm::InstanceIndex>,
        slot_bitmap: Arc<parking_lot::RwLock<aether_rtdb_shm::SlotBitmap>>,
    ) -> Self {
        self.slot_runtime =
            crate::runtime::dynamic_slot_runtime::DynamicSlotRuntime::with_allocation(
                dynamic_index,
                slot_bitmap,
            );
        self
    }

    /// Get dynamic InstanceIndex (for external access, e.g., API stats)
    pub fn dynamic_instance_index(&self) -> Option<&Arc<aether_rtdb_shm::InstanceIndex>> {
        self.slot_runtime.instance_index()
    }

    /// Get SlotBitmap stats (for monitoring)
    pub fn slot_bitmap_stats(&self) -> Option<aether_rtdb_shm::BitmapStats> {
        self.slot_runtime.bitmap_stats()
    }

    /// Get the SQLite pool reference
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Get the routing cache reference
    ///
    /// Returns a reference to the shared routing cache for use in API handlers
    /// that need to refresh the cache after routing management operations.
    pub fn routing_cache(&self) -> &Arc<aether_routing::RoutingCache> {
        &self.routing_cache
    }

    /// Refresh routing cache from database (local operation only)
    ///
    /// SHM layout is based on channel points, not routing — no SHM rebuild needed.
    /// io independently refreshes its own routing cache via periodic polling.
    pub async fn refresh_routing(&self) -> anyhow::Result<usize> {
        crate::bootstrap::refresh_routing_cache(&self.pool, &self.routing_cache).await
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
        if let Some(id) = self.name_cache.get(instance_name) {
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
        self.name_cache.insert(instance_name.to_string(), id);

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

        self.name_cache.clear();
        for (name, id) in instances {
            self.name_cache.insert(name, id);
        }

        debug!("Name->ID cache: {} entries", self.name_cache.len());
        Ok(())
    }

    /// Update cache entry (called on instance create/rename)
    pub fn update_name_cache(&self, instance_name: String, instance_id: u32) {
        self.name_cache.insert(instance_name, instance_id);
    }

    /// Remove entry from cache (called on instance delete)
    pub fn remove_from_name_cache(&self, instance_name: &str) {
        self.name_cache.remove(instance_name);
    }

    /// Create a new instance based on a product template.
    ///
    /// Writes the instance row and properties to SQLite in one transaction.
    pub async fn create_instance(
        &self,
        req: CreateInstanceRequest,
    ) -> Result<Instance, AutomationError> {
        info!(
            "Creating instance: {} (id: {:?}) for product: {}",
            req.instance_name, req.instance_id, req.product_name
        );

        // 1. Validate instance name format
        if let Err(e) = validate_instance_name(&req.instance_name) {
            return Err(AutomationError::InvalidData(format!(
                "Invalid instance name: {}",
                e
            )));
        }

        // 2. Verify product exists (products are compile-time constants)
        // Note: Name uniqueness is enforced by database UNIQUE constraint.
        // We rely on the constraint rather than check-then-act to avoid race conditions.
        let product = self
            .product_loader
            .get_product(&req.product_name)
            .map_err(|e| AutomationError::InvalidData(format!("Unknown product: {}", e)))?;

        // 3. Hierarchy validation: soft check on pName (warn only, never block)
        //    Product JSON defines pName for documentation, but we don't enforce it
        //    since real-world topologies may differ from the product library defaults.
        let parent_name = self
            .product_loader
            .get_product_parent_name(&req.product_name);
        match (&parent_name, req.parent_id) {
            (None, Some(_)) => {
                warn!(
                    "Root product '{}' typically has no parent, but parent_id was provided",
                    req.product_name
                );
            },
            (None, None) => {},
            (Some(expected_parent), None) => {
                warn!(
                    "Product '{}' has pName='{}' but no parent_id provided — creating as standalone",
                    req.product_name, expected_parent
                );
            },
            (Some(expected_parent), Some(pid)) => {
                // Validate parent exists (hard check — referential integrity)
                let parent_product: Option<String> =
                    sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = ?")
                        .bind(pid as i64)
                        .fetch_optional(&self.pool)
                        .await?;

                let parent_product = parent_product.ok_or_else(|| {
                    AutomationError::InstanceNotFound(format!("Parent instance {}", pid))
                })?;

                if parent_product != *expected_parent {
                    warn!(
                        "Parent instance {} is '{}', but '{}' pName suggests '{}' — allowing anyway",
                        pid, parent_product, req.product_name, expected_parent
                    );
                }
            },
        }

        // 4. Begin transaction for atomic creation
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!(
                    "Failed to begin transaction for instance {}: {}",
                    req.instance_name, e
                );
                return Err(AutomationError::DatabaseError(format!(
                    "Transaction failed: {}",
                    e
                )));
            },
        };

        // 4. Create instance in SQLite within transaction
        // Bind instance_id as Option: NULL lets SQLite auto-assign via INTEGER PRIMARY KEY.
        // Property values are written to `instance_properties` below — the
        // `instances` table no longer carries them.
        let insert_result = sqlx::query(
            r#"
            INSERT INTO instances (instance_id, instance_name, product_name, parent_id)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(req.instance_id.map(|id| id as i64))
        .bind(&req.instance_name)
        .bind(&req.product_name)
        .bind(req.parent_id.map(|id| id as i64))
        .execute(&mut *tx)
        .await;

        let instance_id = match insert_result {
            Ok(r) => r.last_insert_rowid() as u32,
            Err(e) => {
                error!("Failed to insert instance {}: {}", req.instance_name, e);
                if let Err(rb_err) = tx.rollback().await {
                    error!("Transaction rollback failed: {}", rb_err);
                }
                // Classify UNIQUE constraint violation as InstanceExists
                let err_str = e.to_string();
                if err_str.contains("UNIQUE constraint") {
                    return Err(AutomationError::InstanceExists(req.instance_name));
                }
                return Err(AutomationError::DatabaseError(format!(
                    "Failed to create instance: {}",
                    e
                )));
            },
        };

        // 4a. Write property values to dedicated table (one row per property).
        // Unknown property names fail the whole transaction — strict schema.
        if let Err(e) = self
            .write_properties_tx(&mut tx, instance_id, &req.product_name, &req.properties)
            .await
        {
            error!(
                "Failed to write properties for instance {}: {}",
                req.instance_name, e
            );
            if let Err(rb_err) = tx.rollback().await {
                error!("Transaction rollback failed: {}", rb_err);
            }
            return Err(AutomationError::InvalidData(format!(
                "Failed to write properties: {}",
                e
            )));
        }

        // 5. Commit transaction first (ensure database persistence)
        if let Err(e) = tx.commit().await {
            error!(
                "Failed to commit transaction for instance {}: {}",
                req.instance_name, e
            );
            return Err(AutomationError::DatabaseError(format!(
                "Transaction commit failed: {}",
                e
            )));
        }

        info!("Successfully created instance {}", req.instance_name);

        // 6. Dynamic Slot Allocation: Add instance to InstanceIndex
        self.slot_runtime.add_instance(
            instance_id,
            [
                product.measurements.len() as u32,
                product.actions.len() as u32,
            ],
        );

        self.update_name_cache(req.instance_name.clone(), instance_id);

        // 7. Return created instance - move req fields into result (avoid clone)
        Ok(Instance {
            core: crate::config::InstanceCore {
                instance_id,
                instance_name: req.instance_name,
                product_name: req.product_name,
                parent_id: req.parent_id,
                properties: req.properties,
            },
            created_at: Some(chrono::Utc::now()),
        })
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

    /// Rename an instance
    pub async fn rename_instance(
        &self,
        instance_id: u32,
        new_name: &str,
    ) -> Result<(), AutomationError> {
        // Validate instance name format before checking uniqueness
        if let Err(e) = validate_instance_name(new_name) {
            return Err(AutomationError::InvalidData(format!(
                "Invalid instance name: {}",
                e
            )));
        }

        // Check if new name already exists
        let (count,): (i64,) = sqlx::query_as(
            r#"SELECT COUNT(*) FROM instances WHERE instance_name = ? AND instance_id != ?"#,
        )
        .bind(new_name)
        .bind(instance_id as i64)
        .fetch_one(&self.pool)
        .await?;

        if count > 0 {
            return Err(AutomationError::InstanceExists(new_name.to_string()));
        }

        let old_name: String =
            sqlx::query_scalar("SELECT instance_name FROM instances WHERE instance_id = ?")
                .bind(instance_id as i64)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| AutomationError::InstanceNotFound(instance_id.to_string()))?;

        // Start transaction
        let mut tx = self.pool.begin().await?;

        // Update instances table
        sqlx::query(
            r#"UPDATE instances SET instance_name = ?, updated_at = CURRENT_TIMESTAMP WHERE instance_id = ?"#,
        )
        .bind(new_name)
        .bind(instance_id as i64)
        .execute(&mut *tx)
        .await?;

        // Update measurement_routing table (redundant field)
        sqlx::query(r#"UPDATE measurement_routing SET instance_name = ? WHERE instance_id = ?"#)
            .bind(new_name)
            .bind(instance_id as i64)
            .execute(&mut *tx)
            .await?;

        // Update action_routing table (redundant field)
        sqlx::query(r#"UPDATE action_routing SET instance_name = ? WHERE instance_id = ?"#)
            .bind(new_name)
            .bind(instance_id as i64)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        self.remove_from_name_cache(&old_name);
        self.update_name_cache(new_name.to_string(), instance_id);

        info!(
            "Instance {} renamed to '{}' in SQLite",
            instance_id, new_name
        );
        Ok(())
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

    /// Collect all descendant instance IDs (BFS), returning them in leaf-first order
    ///
    /// Used for cascade delete: descendants must be deleted before the parent.
    async fn collect_descendants(&self, instance_id: u32) -> Result<Vec<u32>> {
        let mut all = Vec::new();
        let mut queue = vec![instance_id];
        while let Some(parent) = queue.pop() {
            let children: Vec<(u32,)> =
                sqlx::query_as("SELECT instance_id FROM instances WHERE parent_id = ?")
                    .bind(parent as i64)
                    .fetch_all(&self.pool)
                    .await?;
            for (child_id,) in children {
                all.push(child_id);
                queue.push(child_id);
            }
        }
        all.reverse(); // Leaf nodes first, parent nodes last
        Ok(all)
    }

    /// Delete a single instance by ID (internal — no cascade)
    ///
    /// Handles SQLite deletion, local caches, and dynamic slot deallocation.
    async fn delete_single_instance(&self, instance_id: u32) -> Result<(), AutomationError> {
        // 1. Query instance_name before deletion (needed for cache cleanup and logging)
        let instance_name: String =
            sqlx::query_scalar("SELECT instance_name FROM instances WHERE instance_id = ?")
                .bind(instance_id as i64)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| AutomationError::InstanceNotFound(instance_id.to_string()))?;

        // 2. Begin transaction for atomic deletion
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!(
                    "Failed to begin transaction for deleting instance {} ({}): {}",
                    instance_id, instance_name, e
                );
                return Err(AutomationError::DatabaseError(format!(
                    "Transaction failed: {}",
                    e
                )));
            },
        };

        // 3. Delete from SQLite within transaction (cascade will handle point routings)
        let result = sqlx::query("DELETE FROM instances WHERE instance_id = ?")
            .bind(instance_id as i64)
            .execute(&mut *tx)
            .await;

        match result {
            Ok(res) => {
                if res.rows_affected() == 0 {
                    // Rollback transaction
                    if let Err(rb_err) = tx.rollback().await {
                        error!("Transaction rollback failed: {}", rb_err);
                    }
                    return Err(AutomationError::InstanceNotFound(instance_id.to_string()));
                }
            },
            Err(e) => {
                error!(
                    "Failed to delete instance {} ({}) from SQLite: {}",
                    instance_id, instance_name, e
                );
                if let Err(rb_err) = tx.rollback().await {
                    error!("Transaction rollback failed: {}", rb_err);
                }
                return Err(AutomationError::DatabaseError(format!(
                    "Failed to delete instance: {}",
                    e
                )));
            },
        }

        // 4. Commit transaction first (ensure database persistence)
        if let Err(e) = tx.commit().await {
            error!(
                "Failed to commit transaction for deleting instance {} ({}): {}",
                instance_id, instance_name, e
            );
            return Err(AutomationError::DatabaseError(format!(
                "Transaction commit failed: {}",
                e
            )));
        }

        // 5. Dynamic Slot Deallocation: Remove instance from InstanceIndex and free slots
        self.slot_runtime.remove_instance(instance_id);

        // 6. Remove from name cache
        self.remove_from_name_cache(&instance_name);

        info!(
            "Successfully deleted instance: {} ({})",
            instance_id, instance_name
        );
        Ok(())
    }

    /// Delete an instance by ID with cascade delete of all descendants
    ///
    /// Collects all descendant instances (children, grandchildren, etc.),
    /// deletes them leaf-first so local derived state stays consistent,
    /// then deletes the target instance itself.
    pub async fn delete_instance(&self, instance_id: u32) -> Result<(), AutomationError> {
        // 1. Collect all descendants (leaf-first order)
        let descendants = self
            .collect_descendants(instance_id)
            .await
            .map_err(|e| AutomationError::DatabaseError(e.to_string()))?;

        if !descendants.is_empty() {
            info!(
                "Cascade delete: instance {} has {} descendants",
                instance_id,
                descendants.len()
            );
        }

        // 2. Delete descendants leaf-first (each goes through full cleanup)
        for desc_id in &descendants {
            if let Err(e) = self.delete_single_instance(*desc_id).await {
                warn!(
                    "Failed to cascade-delete descendant instance {}: {}",
                    desc_id, e
                );
                // Continue deleting other descendants
            }
        }

        // 3. Delete the target instance itself
        self.delete_single_instance(instance_id).await
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[path = "instance_manager_tests.rs"]
mod tests;
