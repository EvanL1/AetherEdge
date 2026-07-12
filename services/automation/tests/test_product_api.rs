//! Product API Integration Tests
//!
//! Tests product management with an explicitly loaded Energy Pack fixture:
//! - Lightweight product name listing
//! - Detailed product information with measurements/actions/properties
//!
//! Production selects these models only through validated active Pack roots.

#![allow(clippy::disallowed_methods)] // Integration test - unwrap is acceptable

mod common;

use anyhow::Result;
use common::{TestEnv, energy_product_loader};

#[tokio::test]
async fn test_product_list_lightweight() -> Result<()> {
    // 1. Create test environment
    let env = TestEnv::create().await?;

    // 2. Create product loader (products are compile-time constants)
    let product_loader = energy_product_loader(env.pool().clone());

    // 3. Call get_all_product_names (lightweight method)
    // The fixture explicitly selected the Energy Pack model directory.
    let product_names = product_loader.get_all_product_names();

    // 4. Verify specific selected products exist
    let battery = product_names
        .iter()
        .find(|(name, _)| name == "Battery")
        .expect("Should find Battery");
    assert_eq!(battery.1, Some("ESS".to_string()));

    let station = product_names
        .iter()
        .find(|(name, _)| name == "Station")
        .expect("Should find Station");
    assert_eq!(station.1, None, "Station should be a root product");

    let ess = product_names
        .iter()
        .find(|(name, _)| name == "ESS")
        .expect("Should find ESS");
    assert_eq!(ess.1, Some("Station".to_string()));

    // 6. Cleanup
    env.cleanup().await?;

    Ok(())
}

#[tokio::test]
async fn test_product_detail_complete() -> Result<()> {
    // 1. Create test environment
    let env = TestEnv::create().await?;

    // 2. Create product loader
    let product_loader = energy_product_loader(env.pool().clone());

    // 3. Call get_product for Battery (detailed method)
    let product = product_loader
        .get_product("Battery")
        .expect("get_product should succeed for Battery");

    // 4. Verify complete response structure
    assert_eq!(product.product_name, "Battery");
    assert_eq!(product.parent_name, Some("ESS".to_string()));

    // 5. Verify measurements exist
    assert!(
        !product.measurements.is_empty(),
        "Battery should have measurements"
    );

    // 6. Verify actions exist
    assert!(!product.actions.is_empty(), "Battery should have actions");

    // 7. Cleanup
    env.cleanup().await?;

    Ok(())
}

#[tokio::test]
async fn test_product_closed_loop() -> Result<()> {
    // 1. Create test environment
    let env = TestEnv::create().await?;

    // 2. Create product loader
    let product_loader = energy_product_loader(env.pool().clone());

    // 3. STEP 1: Get product list (lightweight)
    let product_names = product_loader.get_all_product_names();

    // 4. STEP 2: For each product, fetch detailed information
    for (product_name, parent_name) in &product_names {
        // Fetch detail
        let product = product_loader
            .get_product(product_name)
            .unwrap_or_else(|_| panic!("get_product should succeed for {}", product_name));

        // Verify product name matches
        assert_eq!(
            &product.product_name, product_name,
            "Product name should match"
        );
        assert_eq!(
            &product.parent_name, parent_name,
            "Parent name should match"
        );

        // Note: Container products (ESS, Generator) may have empty measurements
        // They aggregate data from child products rather than having their own points
    }

    // 5. Verify specific products
    let battery = product_loader.get_product("Battery")?;
    assert_eq!(battery.parent_name, Some("ESS".to_string()));

    let pcs = product_loader.get_product("PCS")?;
    assert_eq!(pcs.parent_name, Some("ESS".to_string()));

    // 6. Cleanup
    env.cleanup().await?;

    Ok(())
}

#[tokio::test]
async fn test_product_not_found() -> Result<()> {
    // 1. Create test environment
    let env = TestEnv::create().await?;

    // 2. Create product loader
    let product_loader = energy_product_loader(env.pool().clone());

    // 3. Try to get a non-existent product
    let result = product_loader.get_product("nonexistent_product");

    // 4. Verify error
    assert!(
        result.is_err(),
        "Should return error for non-existent product"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("not found") || error_msg.contains("Product not found"),
        "Error message should indicate not found: {}",
        error_msg
    );

    // 5. Cleanup
    env.cleanup().await?;

    Ok(())
}

#[tokio::test]
async fn test_product_hierarchy() -> Result<()> {
    // 1. Create test environment
    let env = TestEnv::create().await?;

    // 2. Create product loader
    let product_loader = energy_product_loader(env.pool().clone());

    // 3. Get product list
    let product_names = product_loader.get_all_product_names();

    // 4. Verify hierarchy relationships for selected products
    // Station is root
    let station = product_names
        .iter()
        .find(|(name, _)| name == "Station")
        .expect("Should find Station");
    assert_eq!(station.1, None);

    // ESS -> Station
    let ess = product_names
        .iter()
        .find(|(name, _)| name == "ESS")
        .expect("Should find ESS");
    assert_eq!(ess.1, Some("Station".to_string()));

    // Battery -> ESS
    let battery = product_names
        .iter()
        .find(|(name, _)| name == "Battery")
        .expect("Should find Battery");
    assert_eq!(battery.1, Some("ESS".to_string()));

    // PCS -> ESS
    let pcs = product_names
        .iter()
        .find(|(name, _)| name == "PCS")
        .expect("Should find PCS");
    assert_eq!(pcs.1, Some("ESS".to_string()));

    // 5. Cleanup
    env.cleanup().await?;

    Ok(())
}

#[tokio::test]
async fn test_product_exists() -> Result<()> {
    // 1. Create test environment
    let env = TestEnv::create().await?;

    // 2. Create product loader
    let product_loader = energy_product_loader(env.pool().clone());

    // 3. Verify selected products exist
    assert!(product_loader.product_exists("Battery"));
    assert!(product_loader.product_exists("PCS"));
    assert!(product_loader.product_exists("Station"));
    assert!(product_loader.product_exists("ESS"));
    assert!(product_loader.product_exists("Generator"));

    // 4. Verify non-existent products don't exist
    assert!(!product_loader.product_exists("NonExistentProduct"));
    assert!(!product_loader.product_exists("FakeProduct"));

    // 5. Cleanup
    env.cleanup().await?;

    Ok(())
}
