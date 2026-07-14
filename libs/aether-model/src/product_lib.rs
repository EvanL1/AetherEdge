//! Runtime Product Library
//!
//! The industry-neutral kernel embeds no domain products. Composition roots
//! explicitly select validated Pack model directories and optional site-owned
//! product directories at runtime.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// Point definition for measurements, actions, and properties
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointDef {
    /// Point ID (unique within product)
    pub id: u32,
    /// Point name
    pub name: String,
    /// Unit of measurement (empty string if none)
    #[serde(default)]
    pub unit: String,
    /// Value type (number, string, etc.)
    #[serde(rename = "type", default)]
    pub value_type: String,
}

/// Built-in product definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinProduct {
    /// Product name (unique identifier)
    pub name: String,
    /// Parent product name for hierarchy (e.g., Battery -> ESS -> Station)
    #[serde(rename = "pName")]
    pub parent_name: Option<String>,
    /// Property definitions (P)
    #[serde(rename = "P", default)]
    pub properties: Vec<PointDef>,
    /// Measurement point definitions (M)
    #[serde(rename = "M", default)]
    pub measurements: Vec<PointDef>,
    /// Action point definitions (A)
    #[serde(rename = "A", default)]
    pub actions: Vec<PointDef>,
}

const MAX_PRODUCT_JSON_BYTES: u64 = 1024 * 1024;

fn product_json_paths(directory: &Path) -> Result<Vec<std::path::PathBuf>> {
    let canonical_directory = std::fs::canonicalize(directory)
        .with_context(|| format!("Failed to resolve products dir: {}", directory.display()))?;
    let entries = std::fs::read_dir(&canonical_directory).with_context(|| {
        format!(
            "Failed to read products dir: {}",
            canonical_directory.display()
        )
    })?;
    let mut paths = entries
        .map(|entry| entry.map(|value| value.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_unstable();

    let mut validated = Vec::new();
    for path in paths {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            bail!(
                "Product entry has a non-UTF-8 filename in {}",
                canonical_directory.display()
            );
        };
        if Path::new(file_name)
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("Product JSON symlink is forbidden: {}", path.display());
        }
        if !metadata.file_type().is_file() {
            bail!("Product JSON must be a regular file: {}", path.display());
        }
        if metadata.len() > MAX_PRODUCT_JSON_BYTES {
            bail!(
                "Product JSON exceeds {} bytes: {}",
                MAX_PRODUCT_JSON_BYTES,
                path.display()
            );
        }
        let resolved = std::fs::canonicalize(&path)
            .with_context(|| format!("Failed to resolve {}", path.display()))?;
        if !resolved.starts_with(&canonical_directory) {
            bail!(
                "Product JSON escapes selected directory {}: {}",
                canonical_directory.display(),
                resolved.display()
            );
        }
        validated.push(resolved);
    }
    Ok(validated)
}

fn read_product(path: &Path) -> Result<BuiltinProduct> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("JSON parse error: {}", path.display()))
}

/// Runtime product library assembled from explicitly selected directories.
///
/// Later directories override earlier directories by product name. This lets a
/// site-owned directory deliberately refine models supplied by an active Pack.
///
/// # Example
/// ```ignore
/// let lib = ProductLibrary::load(Some(Path::new("config/products")))?;
/// let battery = lib.get("Battery").expect("Battery product");
/// ```
#[derive(Debug, Default)]
pub struct ProductLibrary {
    products: Vec<BuiltinProduct>,
}

impl ProductLibrary {
    /// Loads one explicitly selected directory.
    ///
    /// `None` or a missing directory produces an empty library; it never falls
    /// back to a domain Pack embedded in the kernel.
    pub fn load(products_dir: Option<&Path>) -> Result<Self> {
        let directories = products_dir.into_iter().collect::<Vec<_>>();
        Self::load_directories(&directories)
    }

    /// Loads product JSON from explicitly ordered directories.
    pub fn load_directories(directories: &[&Path]) -> Result<Self> {
        let mut products = Vec::new();
        for directory in directories.iter().copied().filter(|path| path.is_dir()) {
            let mut directory_names = BTreeSet::new();
            for resolved in product_json_paths(directory)? {
                let product = read_product(&resolved)?;
                if !directory_names.insert(product.name.clone()) {
                    bail!(
                        "Product '{}' is declared more than once in {}",
                        product.name,
                        directory.display()
                    );
                }

                if let Some(index) = products
                    .iter()
                    .position(|existing: &BuiltinProduct| existing.name == product.name)
                {
                    tracing::info!(
                        "Product '{}' overridden from {}",
                        product.name,
                        resolved.display()
                    );
                    products[index] = product;
                } else {
                    tracing::info!(
                        "Product '{}' loaded from {}",
                        product.name,
                        resolved.display()
                    );
                    products.push(product);
                }
            }
        }
        Ok(Self { products })
    }

    /// Get all products
    pub fn all(&self) -> &[BuiltinProduct] {
        &self.products
    }

    /// Get product by name
    pub fn get(&self, name: &str) -> Option<&BuiltinProduct> {
        self.products.iter().find(|p| p.name == name)
    }

    /// Get all product names
    pub fn names(&self) -> Vec<&str> {
        self.products.iter().map(|p| p.name.as_str()).collect()
    }

    /// Check if product exists
    pub fn exists(&self, name: &str) -> bool {
        self.products.iter().any(|p| p.name == name)
    }

    /// Get number of products
    pub fn len(&self) -> usize {
        self.products.len()
    }

    /// Check if library is empty
    pub fn is_empty(&self) -> bool {
        self.products.is_empty()
    }

    /// Get child products of a given parent
    pub fn children(&self, parent_name: &str) -> Vec<&BuiltinProduct> {
        self.products
            .iter()
            .filter(|p| p.parent_name.as_deref() == Some(parent_name))
            .collect()
    }
}

/// Validate product JSON files in a directory without loading them into a library
///
/// Returns a list of (filename, error_message) for invalid files.
/// Valid files return an empty list.
pub fn validate_product_dir(dir: &Path) -> Vec<(String, String)> {
    let mut errors = Vec::new();
    let paths = match product_json_paths(dir) {
        Ok(paths) => paths,
        Err(error) => {
            errors.push(("(directory)".to_string(), error.to_string()));
            return errors;
        },
    };
    let mut names = BTreeSet::new();

    for path in paths {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();

        match read_product(&path) {
            Ok(p) => {
                if p.name.is_empty() {
                    errors.push((filename, "product name is empty".to_string()));
                } else if !names.insert(p.name.clone()) {
                    errors.push((
                        filename,
                        format!("Product '{}' is declared more than once", p.name),
                    ));
                }
            },
            Err(error) => {
                errors.push((filename, error.to_string()));
            },
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_product_library_default_is_empty() {
        let lib = ProductLibrary::default();
        assert!(lib.is_empty());
        assert_eq!(lib.len(), 0);
    }

    #[test]
    fn test_product_library_load_no_dir() -> anyhow::Result<()> {
        let lib = ProductLibrary::load(None)?;
        assert!(lib.is_empty());
        Ok(())
    }

    #[test]
    fn test_product_library_load_nonexistent_dir() -> anyhow::Result<()> {
        let lib = ProductLibrary::load(Some(Path::new("/nonexistent/path")))?;
        assert!(lib.is_empty());
        Ok(())
    }

    #[test]
    fn test_product_library_load_with_override() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let products_dir = temp_dir.path();

        // An explicitly selected directory may define a product named Battery.
        let custom_battery = r#"{
            "name": "Battery",
            "pName": "ESS",
            "M": [{"id": 1, "name": "CustomVoltage", "unit": "V"}],
            "A": [],
            "P": []
        }"#;
        std::fs::write(products_dir.join("Battery.json"), custom_battery)?;

        let lib = ProductLibrary::load(Some(products_dir))?;
        assert_eq!(lib.len(), 1);

        let battery = lib.get("Battery").context("Battery not found")?;
        assert_eq!(battery.measurements.len(), 1);
        assert_eq!(battery.measurements[0].name, "CustomVoltage");
        Ok(())
    }

    #[test]
    fn test_product_library_load_with_new_product() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let products_dir = temp_dir.path();

        // Write a brand new product
        let custom_product = r#"{
            "name": "WindTurbine",
            "pName": "Station",
            "M": [{"id": 1, "name": "WindSpeed", "unit": "m/s"}],
            "A": [],
            "P": []
        }"#;
        std::fs::write(products_dir.join("WindTurbine.json"), custom_product)?;

        let lib = ProductLibrary::load(Some(products_dir))?;
        assert_eq!(lib.len(), 1);
        assert!(lib.exists("WindTurbine"));

        let wind = lib.get("WindTurbine").context("WindTurbine not found")?;
        assert_eq!(wind.parent_name.as_deref(), Some("Station"));
        Ok(())
    }

    #[test]
    fn explicitly_ordered_directories_allow_site_overrides() -> anyhow::Result<()> {
        let pack = tempfile::tempdir()?;
        let site = tempfile::tempdir()?;
        std::fs::write(
            pack.path().join("Battery.json"),
            r#"{"name":"Battery","M":[{"id":1,"name":"Pack SOC"}],"A":[],"P":[]}"#,
        )?;
        std::fs::write(
            site.path().join("Battery.json"),
            r#"{"name":"Battery","M":[{"id":1,"name":"Site SOC"}],"A":[],"P":[]}"#,
        )?;

        let lib = ProductLibrary::load_directories(&[pack.path(), site.path()])?;

        assert_eq!(lib.len(), 1);
        assert_eq!(
            lib.get("Battery").context("Battery")?.measurements[0].name,
            "Site SOC"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn selected_product_directory_rejects_json_symlinks() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let selected = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let target = outside.path().join("Escaped.json");
        std::fs::write(&target, r#"{"name":"Escaped","M":[],"A":[],"P":[]}"#)?;
        symlink(target, selected.path().join("Escaped.json"))?;

        let error = match ProductLibrary::load(Some(selected.path())) {
            Err(error) => error,
            Ok(_) => panic!("JSON symlink must be rejected"),
        };

        assert!(error.to_string().contains("symlink"));
        Ok(())
    }

    #[test]
    fn selected_product_directory_rejects_non_regular_json_entries() -> anyhow::Result<()> {
        let selected = tempfile::tempdir()?;
        std::fs::create_dir(selected.path().join("Directory.json"))?;

        let error = match ProductLibrary::load(Some(selected.path())) {
            Err(error) => error,
            Ok(_) => panic!("JSON directory must be rejected"),
        };

        assert!(error.to_string().contains("regular file"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn product_directory_validation_rejects_json_symlinks() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let selected = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let target = outside.path().join("Escaped.json");
        std::fs::write(&target, r#"{"name":"Escaped","M":[],"A":[],"P":[]}"#)?;
        symlink(target, selected.path().join("Escaped.json"))?;

        let errors = validate_product_dir(selected.path());

        assert_eq!(errors.len(), 1);
        assert!(errors[0].1.contains("symlink"));
        Ok(())
    }

    #[test]
    fn duplicate_product_names_within_one_directory_fail_closed() -> anyhow::Result<()> {
        let selected = tempfile::tempdir()?;
        let product = r#"{"name":"Duplicate","M":[],"A":[],"P":[]}"#;
        std::fs::write(selected.path().join("First.json"), product)?;
        std::fs::write(selected.path().join("Second.json"), product)?;

        let runtime_error = match ProductLibrary::load(Some(selected.path())) {
            Err(error) => error,
            Ok(_) => panic!("same-directory duplicate must be rejected"),
        };
        let validation_errors = validate_product_dir(selected.path());

        assert!(runtime_error.to_string().contains("Duplicate"));
        assert_eq!(validation_errors.len(), 1);
        assert!(validation_errors[0].1.contains("Duplicate"));
        Ok(())
    }

    #[test]
    fn oversized_product_json_fails_runtime_and_validation() -> anyhow::Result<()> {
        let selected = tempfile::tempdir()?;
        std::fs::write(
            selected.path().join("Huge.json"),
            vec![b'x'; MAX_PRODUCT_JSON_BYTES as usize + 1],
        )?;

        let runtime_error = ProductLibrary::load(Some(selected.path()))
            .expect_err("oversized product must fail runtime loading");
        let validation_errors = validate_product_dir(selected.path());

        assert!(runtime_error.to_string().contains("exceeds"));
        assert_eq!(validation_errors.len(), 1);
        assert!(validation_errors[0].1.contains("exceeds"));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_product_entry_fails_runtime_and_validation() -> anyhow::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let selected = tempfile::tempdir()?;
        let filename = OsString::from_vec(vec![0xff, b'.', b'j', b's', b'o', b'n']);
        std::fs::write(selected.path().join(filename), b"{}")?;

        let runtime_error = ProductLibrary::load(Some(selected.path()))
            .expect_err("non-UTF-8 product name must fail runtime loading");
        let validation_errors = validate_product_dir(selected.path());

        assert!(runtime_error.to_string().contains("non-UTF-8"));
        assert_eq!(validation_errors.len(), 1);
        assert!(validation_errors[0].1.contains("non-UTF-8"));
        Ok(())
    }

    #[test]
    fn test_product_library_names_and_children_come_only_from_selected_directory()
    -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        std::fs::write(
            temp_dir.path().join("Station.json"),
            r#"{"name":"Station","M":[],"A":[],"P":[]}"#,
        )?;
        std::fs::write(
            temp_dir.path().join("Battery.json"),
            r#"{"name":"Battery","pName":"Station","M":[],"A":[],"P":[]}"#,
        )?;
        let lib = ProductLibrary::load(Some(temp_dir.path()))?;

        assert_eq!(lib.names(), vec!["Battery", "Station"]);
        assert_eq!(lib.children("Station")[0].name, "Battery");
        Ok(())
    }

    #[test]
    fn test_validate_product_dir_valid() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let dir = temp_dir.path();

        let valid = r#"{"name": "Test", "M": [], "A": [], "P": []}"#;
        std::fs::write(dir.join("Test.json"), valid)?;

        let errors = validate_product_dir(dir);
        assert!(errors.is_empty());
        Ok(())
    }

    #[test]
    fn test_validate_product_dir_invalid_json() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let dir = temp_dir.path();

        std::fs::write(dir.join("Bad.json"), "not json")?;

        let errors = validate_product_dir(dir);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].1.contains("JSON parse error"));
        Ok(())
    }

    #[test]
    fn test_validate_product_dir_empty_name() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let dir = temp_dir.path();

        let empty_name = r#"{"name": "", "M": [], "A": [], "P": []}"#;
        std::fs::write(dir.join("Empty.json"), empty_name)?;

        let errors = validate_product_dir(dir);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].1.contains("empty"));
        Ok(())
    }
}
