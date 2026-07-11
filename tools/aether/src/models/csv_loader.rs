//! CSV loader for product definitions
//!
//! Products are compile-time built-in constants from aether-model crate.
//! Only list_available_products is retained for development/debugging purposes.

use anyhow::Result;
use std::fs;
use std::path::Path;

/// List available products in the products/ directory
/// This is kept for development purposes to see custom product definitions.
pub fn list_available_products() -> Result<()> {
    let products_dir = Path::new("products");

    if !products_dir.exists() {
        println!("No products directory found");
        println!("Note: Products are now built-in from aether-model crate.");
        println!("Use 'aether models products list' to see built-in products.");
        return Ok(());
    }

    println!("Available product definitions in products/ directory:");

    for entry in fs::read_dir(products_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir()
            && let Some(name) = path.file_name()
            && let Some(name_str) = name.to_str()
        {
            // Check if it has at least one CSV file
            let has_csv = ["measurements.csv", "actions.csv", "properties.csv"]
                .iter()
                .any(|f| path.join(f).exists());

            if has_csv {
                println!("  - {}", name_str);
            }
        }
    }

    println!("\nNote: Products are now built-in from aether-model crate.");
    println!("These CSV files are for reference only.");

    Ok(())
}
