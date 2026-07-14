//! Product configuration loaded by the automation composition root.
//!
//! Product queries use an explicitly assembled runtime [`ProductLibrary`]. The
//! default loader is empty and never selects a domain Pack implicitly.

use aether_model::product_lib::{BuiltinProduct, PointDef, ProductLibrary};
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
/// The library is populated explicitly by startup after active Pack validation.
/// The empty constructor remains the intentional no-Pack kernel composition.
#[derive(Clone)]
pub struct ProductLoader {
    /// SQLite pool for instance schema initialization (not for product queries)
    pool: SqlitePool,
    /// Explicit runtime product library (empty when no Pack is active).
    library: Arc<ProductLibrary>,
}

impl ProductLoader {
    /// Creates a ProductLoader with no domain products.
    ///
    /// The pool is only used for schema initialization, not product queries.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            library: Arc::new(ProductLibrary::default()),
        }
    }

    /// Create a ProductLoader with a runtime product library
    ///
    /// When a library is provided, all product queries use its active Pack and
    /// site-selected products.
    pub fn with_library(pool: SqlitePool, library: Arc<ProductLibrary>) -> Self {
        Self { pool, library }
    }

    /// Initialize database schema for instances.
    ///
    /// Note: product tables are not created; definitions come from the selected
    /// runtime library.
    /// Logical measurement/action routes are owned by the canonical SQLite
    /// routing tables; the removed legacy mapping compatibility table is not
    /// recreated here.
    pub async fn init_schema(&self) -> Result<()> {
        debug!("Init instance tables");

        // Reuse canonical DDL from common crate (single source of truth)
        sqlx::query(INSTANCES_TABLE).execute(&self.pool).await?;

        debug!("Instance tables ready");
        Ok(())
    }

    // ============ Product Query Methods ============

    /// Get a complete product with nested structure
    pub fn get_product(&self, product_name: &str) -> Result<Product> {
        let builtin = self
            .library
            .get(product_name)
            .context(format!("Product not found: {}", product_name))?;
        Ok(convert_builtin_to_product(builtin))
    }

    /// Get all products
    pub fn get_all_products(&self) -> Vec<Product> {
        self.library
            .all()
            .iter()
            .map(convert_builtin_to_product)
            .collect()
    }

    /// Get product hierarchy (product_name, parent_name) tuples
    pub fn get_product_hierarchy(&self) -> ProductHierarchy {
        self.library
            .all()
            .iter()
            .map(|p| (p.name.clone(), p.parent_name.clone()))
            .collect()
    }

    /// Get all product names without loading point details
    ///
    /// Returns Vec of (product_name, parent_name) tuples.
    /// Ideal for frontend dropdown lists or selection interfaces.
    pub fn get_all_product_names(&self) -> Vec<(String, Option<String>)> {
        self.library
            .all()
            .iter()
            .map(|p| (p.name.clone(), p.parent_name.clone()))
            .collect()
    }

    /// Get the parent product name for a given product (from pName field in JSON)
    ///
    /// Returns None for root products (e.g., Station).
    /// Returns Some("ESS") for products like Battery, PCS, etc.
    pub fn get_product_parent_name(&self, product_name: &str) -> Option<String> {
        self.library
            .get(product_name)
            .and_then(|p| p.parent_name.clone())
    }

    /// Check if a product exists
    pub fn product_exists(&self, name: &str) -> bool {
        self.library.exists(name)
    }

    /// Get the number of products
    pub fn product_count(&self) -> usize {
        self.library.len()
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
