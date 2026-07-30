//! Product configuration loaded by the automation composition root.
//!
//! Product queries use an explicitly assembled runtime [`ProductLibrary`]. The
//! default loader is empty and never selects a domain Pack implicitly.

use aether_pack::{ProductDefinition, ProductLibrary, ProductPointDefinition};
use anyhow::{Context, Result};
use common::test_utils::schema::INSTANCES_TABLE;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::debug;

// Re-export types from local config for other modules
pub use crate::config::{
    ActionPoint, CreateInstanceRequest, Instance, MeasurementPoint, Product, PropertyTemplate,
};
pub use common::PointRole;

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

    /// Borrow the validated Pack-owned definition without building an API DTO.
    pub(crate) fn get_definition(&self, product_name: &str) -> Result<&ProductDefinition> {
        self.library
            .get(product_name)
            .context(format!("Product not found: {product_name}"))
    }

    /// Get a complete product with nested structure
    pub fn get_product(&self, product_name: &str) -> Result<Product> {
        convert_product_definition(self.get_definition(product_name)?)
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

/// Convert one validated Pack product definition to the service DTO.
fn convert_product_definition(builtin: &ProductDefinition) -> Result<Product> {
    Ok(Product {
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
            .collect::<Result<Vec<_>>>()?,
    })
}

pub(crate) fn convert_point_to_measurement(point: &ProductPointDefinition) -> MeasurementPoint {
    MeasurementPoint {
        measurement_id: point.id,
        name: point.name.clone(),
        unit: if point.unit.is_empty() {
            None
        } else {
            Some(point.unit.clone())
        },
        description: None, // Pack product point definitions do not carry descriptions yet.
    }
}

pub(crate) fn convert_point_to_action(point: &ProductPointDefinition) -> ActionPoint {
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

pub(crate) fn convert_point_to_property(
    point: &ProductPointDefinition,
) -> Result<PropertyTemplate> {
    Ok(PropertyTemplate {
        property_id: i32::try_from(point.id)
            .context("Pack property ID exceeds the Automation API range")?,
        name: point.name.clone(),
        unit: if point.unit.is_empty() {
            None
        } else {
            Some(point.unit.clone())
        },
        description: None,
    })
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
        assert!(loader.get_product("Battery").is_err());
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
    fn validated_product_definitions_are_borrowed_without_reconversion() {
        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        rt.block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:")
                .await
                .expect("test database");
            let loader = test_energy_product_loader(pool);

            let first = loader
                .get_definition("Battery")
                .expect("first product definition");
            let second = loader
                .get_definition("Battery")
                .expect("second product definition");

            assert!(std::ptr::eq(first, second));
        });
    }
}
