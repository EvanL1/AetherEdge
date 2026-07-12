//! Product configuration loaded by the automation composition root.
//!
//! Product queries use an explicitly assembled runtime [`ProductLibrary`]. The
//! default loader is empty and never selects a domain Pack implicitly.

use aether_model::product_lib::{self, BuiltinProduct, PointDef, ProductLibrary};
use anyhow::{Context, Result};
use common::test_utils::schema::INSTANCES_TABLE;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::debug;

// Re-export types from local config for other modules
pub use crate::config::{
    ActionPoint, CreateInstanceRequest, Instance, MeasurementPoint, Product, ProductHierarchy,
    PropertyTemplate,
};
pub use aether_model::PointRole;

/// Product loader that provides access to products
///
/// The optional library is populated by startup after active Pack validation.
/// `None` is the fail-safe empty state retained for constructor compatibility.
#[derive(Clone)]
pub struct ProductLoader {
    /// SQLite pool for instance schema initialization (not for product queries)
    pool: SqlitePool,
    /// Optional runtime product library (with external overrides)
    library: Option<Arc<ProductLibrary>>,
}

impl ProductLoader {
    /// Creates a ProductLoader with no domain products.
    ///
    /// The pool is only used for schema initialization, not product queries.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            library: None,
        }
    }

    /// Create a ProductLoader with a runtime product library
    ///
    /// When a library is provided, all product queries use its active Pack and
    /// site-selected products.
    pub fn with_library(pool: SqlitePool, library: Arc<ProductLibrary>) -> Self {
        Self {
            pool,
            library: Some(library),
        }
    }

    /// Initialize database schema for instances and mappings
    ///
    /// Note: product tables are not created; definitions come from the selected
    /// runtime library.
    /// This method only creates instance-related tables.
    pub async fn init_schema(&self) -> Result<()> {
        debug!("Init instance tables");

        // Reuse canonical DDL from common crate (single source of truth)
        sqlx::query(INSTANCES_TABLE).execute(&self.pool).await?;

        // Create point mappings table for channel-instance point routing
        // UNIQUE constraint ensures each instance point has only one data source
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS point_mappings (
                mapping_id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                channel_type TEXT NOT NULL CHECK(channel_type IN ('T','S','C','A')),
                channel_point_id INTEGER NOT NULL,
                instance_type TEXT NOT NULL CHECK(instance_type IN ('M','A')),
                instance_point_id INTEGER NOT NULL,
                description TEXT,
                enabled BOOLEAN DEFAULT TRUE,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(instance_id, instance_type, instance_point_id),
                FOREIGN KEY (instance_id) REFERENCES instances(instance_id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_mapping_channel ON point_mappings(channel_id, channel_type)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_mapping_instance ON point_mappings(instance_id)",
        )
        .execute(&self.pool)
        .await?;

        debug!("Instance tables ready");
        Ok(())
    }

    // ============ Product Query Methods ============

    /// Get a complete product with nested structure
    pub fn get_product(&self, product_name: &str) -> Result<Product> {
        if let Some(lib) = &self.library {
            let builtin = lib
                .get(product_name)
                .context(format!("Product not found: {}", product_name))?;
            return Ok(convert_builtin_to_product(builtin));
        }

        let builtin = product_lib::get_builtin_product(product_name)
            .context(format!("Product not found: {}", product_name))?;
        Ok(convert_builtin_to_product(builtin))
    }

    /// Get all products
    pub fn get_all_products(&self) -> Vec<Product> {
        if let Some(lib) = &self.library {
            return lib.all().iter().map(convert_builtin_to_product).collect();
        }

        product_lib::get_builtin_products()
            .iter()
            .map(convert_builtin_to_product)
            .collect()
    }

    /// Get product hierarchy (product_name, parent_name) tuples
    pub fn get_product_hierarchy(&self) -> ProductHierarchy {
        if let Some(lib) = &self.library {
            return lib
                .all()
                .iter()
                .map(|p| (p.name.clone(), p.parent_name.clone()))
                .collect();
        }

        product_lib::get_builtin_products()
            .iter()
            .map(|p| (p.name.clone(), p.parent_name.clone()))
            .collect()
    }

    /// Get all product names without loading point details
    ///
    /// Returns Vec of (product_name, parent_name) tuples.
    /// Ideal for frontend dropdown lists or selection interfaces.
    pub fn get_all_product_names(&self) -> Vec<(String, Option<String>)> {
        if let Some(lib) = &self.library {
            return lib
                .all()
                .iter()
                .map(|p| (p.name.clone(), p.parent_name.clone()))
                .collect();
        }

        product_lib::get_builtin_products()
            .iter()
            .map(|p| (p.name.clone(), p.parent_name.clone()))
            .collect()
    }

    /// Get the parent product name for a given product (from pName field in JSON)
    ///
    /// Returns None for root products (e.g., Station).
    /// Returns Some("ESS") for products like Battery, PCS, etc.
    pub fn get_product_parent_name(&self, product_name: &str) -> Option<String> {
        if let Some(lib) = &self.library {
            return lib.get(product_name).and_then(|p| p.parent_name.clone());
        }
        product_lib::get_builtin_product(product_name).and_then(|p| p.parent_name.clone())
    }

    /// Check if a product exists
    pub fn product_exists(&self, name: &str) -> bool {
        if let Some(lib) = &self.library {
            return lib.exists(name);
        }
        product_lib::product_exists(name)
    }

    /// Get the number of products
    pub fn product_count(&self) -> usize {
        if let Some(lib) = &self.library {
            return lib.len();
        }
        product_lib::get_builtin_products().len()
    }
}

#[cfg(test)]
pub(crate) fn test_energy_product_loader(pool: SqlitePool) -> ProductLoader {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/energy/models");
    let library = ProductLibrary::load(Some(&directory)).expect("load Energy Pack model fixture");
    ProductLoader::with_library(pool, Arc::new(library))
}

// ============ Type Conversion Functions ============

/// Convert BuiltinProduct to Product
fn convert_builtin_to_product(builtin: &BuiltinProduct) -> Product {
    Product {
        product_name: builtin.name.clone(),
        parent_name: builtin.parent_name.clone(),
        measurements: builtin
            .measurements
            .iter()
            .map(convert_point_to_measurement)
            .collect(),
        actions: builtin
            .actions
            .iter()
            .map(convert_point_to_action)
            .collect(),
        properties: builtin
            .properties
            .iter()
            .map(convert_point_to_property)
            .collect(),
    }
}

fn convert_point_to_measurement(point: &PointDef) -> MeasurementPoint {
    MeasurementPoint {
        measurement_id: point.id,
        name: point.name.clone(),
        unit: if point.unit.is_empty() {
            None
        } else {
            Some(point.unit.clone())
        },
        description: None, // BuiltinProduct doesn't have description
    }
}

fn convert_point_to_action(point: &PointDef) -> ActionPoint {
    ActionPoint {
        action_id: point.id,
        name: point.name.clone(),
        unit: if point.unit.is_empty() {
            None
        } else {
            Some(point.unit.clone())
        },
        description: None,
    }
}

fn convert_point_to_property(point: &PointDef) -> PropertyTemplate {
    PropertyTemplate {
        property_id: point.id as i32,
        name: point.name.clone(),
        unit: if point.unit.is_empty() {
            None
        } else {
            Some(point.unit.clone())
        },
        description: None,
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_loader_exposes_no_domain_products() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let loader = ProductLoader::new(pool);

        assert_eq!(loader.product_count(), 0);
        assert!(loader.get_all_products().is_empty());
        assert!(!loader.product_exists("Battery"));
    }

    #[test]
    fn test_get_product() {
        // Create a dummy pool for testing (not used for product queries)
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
            let loader = test_energy_product_loader(pool);

            let product = loader.get_product("Battery").expect("Battery should exist");
            assert_eq!(product.product_name, "Battery");
            assert_eq!(product.parent_name, Some("ESS".to_string()));
            assert!(!product.measurements.is_empty());
        });
    }

    #[test]
    fn test_get_all_products() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
            let loader = test_energy_product_loader(pool);

            let products = loader.get_all_products();
            let names: Vec<&str> = products.iter().map(|p| p.product_name.as_str()).collect();
            assert!(names.contains(&"Battery"));
            assert!(names.contains(&"PCS"));
            assert!(names.contains(&"Station"));
        });
    }

    #[test]
    fn test_product_exists() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
            let loader = test_energy_product_loader(pool);

            assert!(loader.product_exists("Battery"));
            assert!(loader.product_exists("PCS"));
            assert!(!loader.product_exists("NonExistent"));
        });
    }

    #[test]
    fn test_product_hierarchy() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
            let loader = test_energy_product_loader(pool);

            let hierarchy = loader.get_product_hierarchy();
            assert!(!hierarchy.is_empty());

            // Check Station is root
            let station = hierarchy.iter().find(|(name, _)| name == "Station");
            assert!(station.is_some());
            assert!(station.unwrap().1.is_none());

            // Check Battery -> ESS
            let battery = hierarchy.iter().find(|(name, _)| name == "Battery");
            assert!(battery.is_some());
            assert_eq!(battery.unwrap().1, Some("ESS".to_string()));
        });
    }
}
