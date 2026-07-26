//! Validated product/model definitions supplied by active Packs or a selected site directory.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_PRODUCT_JSON_BYTES: u64 = 1024 * 1024;

/// One property, measurement, or action definition in a product model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductPointDefinition {
    /// Point identifier, unique within its product role.
    pub id: u32,
    /// Operator-visible point name.
    pub name: String,
    /// Unit of measurement, or an empty string when dimensionless.
    #[serde(default)]
    pub unit: String,
    /// Compatibility value-type identifier.
    #[serde(rename = "type", default)]
    pub value_type: String,
}

/// One declarative product model loaded from a validated directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductDefinition {
    /// Product identity.
    pub name: String,
    /// Optional parent product identity.
    #[serde(rename = "pName")]
    pub parent_name: Option<String>,
    /// Property definitions (`P`).
    #[serde(rename = "P", default)]
    pub properties: Vec<ProductPointDefinition>,
    /// Measurement definitions (`M`).
    #[serde(rename = "M", default)]
    pub measurements: Vec<ProductPointDefinition>,
    /// Action definitions (`A`).
    #[serde(rename = "A", default)]
    pub actions: Vec<ProductPointDefinition>,
}

/// Fail-closed error while loading Pack product models.
#[derive(Debug, Error)]
pub enum ProductLibraryError {
    /// The selected directory could not be resolved or inspected.
    #[error("failed to resolve products directory {path}: {source}")]
    ResolveDirectory {
        /// Selected directory.
        path: PathBuf,
        /// File-system failure.
        #[source]
        source: std::io::Error,
    },
    /// The selected product directory was a symbolic link.
    #[error("product directory symlink is forbidden: {path}")]
    DirectorySymlink {
        /// Rejected directory path.
        path: PathBuf,
    },
    /// The selected product path was not a directory.
    #[error("product path must be a directory: {path}")]
    NotDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// The selected directory could not be enumerated.
    #[error("failed to read products directory {path}: {source}")]
    ReadDirectory {
        /// Selected directory.
        path: PathBuf,
        /// File-system failure.
        #[source]
        source: std::io::Error,
    },
    /// One directory entry could not be read.
    #[error("failed to read an entry in products directory {path}: {source}")]
    ReadEntry {
        /// Selected directory.
        path: PathBuf,
        /// File-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A directory entry did not have a UTF-8 filename.
    #[error("product entry has a non-UTF-8 filename in {directory}")]
    NonUtf8Filename {
        /// Selected directory.
        directory: PathBuf,
    },
    /// Metadata for a candidate model could not be read.
    #[error("failed to inspect product JSON {path}: {source}")]
    InspectFile {
        /// Candidate path.
        path: PathBuf,
        /// File-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A product model was a symbolic link.
    #[error("product JSON symlink is forbidden: {path}")]
    Symlink {
        /// Rejected path.
        path: PathBuf,
    },
    /// A product model was not a regular file.
    #[error("product JSON must be a regular file: {path}")]
    NotRegularFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// A product model exceeded the local safety bound.
    #[error("product JSON exceeds {limit} bytes: {path}")]
    TooLarge {
        /// Rejected path.
        path: PathBuf,
        /// Maximum accepted size.
        limit: u64,
    },
    /// A candidate product model could not be canonicalized.
    #[error("failed to resolve product JSON {path}: {source}")]
    ResolveFile {
        /// Candidate path.
        path: PathBuf,
        /// File-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A resolved candidate escaped the selected directory.
    #[error("product JSON escapes selected directory {directory}: {path}")]
    EscapesDirectory {
        /// Canonical selected directory.
        directory: PathBuf,
        /// Escaping resolved path.
        path: PathBuf,
    },
    /// A product model could not be read.
    #[error("failed to read product JSON {path}: {source}")]
    ReadFile {
        /// Candidate path.
        path: PathBuf,
        /// File-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A product model did not match the JSON contract.
    #[error("JSON parse error in {path}: {source}")]
    InvalidJson {
        /// Candidate path.
        path: PathBuf,
        /// JSON decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// A product model omitted its required identity.
    #[error("product name is empty in {path}")]
    EmptyProductName {
        /// Rejected model path.
        path: PathBuf,
    },
    /// One selected directory declared the same product more than once.
    #[error("product {name:?} is declared more than once in {directory}")]
    DuplicateProduct {
        /// Duplicated product identity.
        name: String,
        /// Directory containing both declarations.
        directory: PathBuf,
    },
}

fn product_json_paths(directory: &Path) -> Result<Vec<PathBuf>, ProductLibraryError> {
    let directory_metadata = std::fs::symlink_metadata(directory).map_err(|source| {
        ProductLibraryError::ResolveDirectory {
            path: directory.to_path_buf(),
            source,
        }
    })?;
    if directory_metadata.file_type().is_symlink() {
        return Err(ProductLibraryError::DirectorySymlink {
            path: directory.to_path_buf(),
        });
    }
    if !directory_metadata.file_type().is_dir() {
        return Err(ProductLibraryError::NotDirectory {
            path: directory.to_path_buf(),
        });
    }

    let canonical_directory = std::fs::canonicalize(directory).map_err(|source| {
        ProductLibraryError::ResolveDirectory {
            path: directory.to_path_buf(),
            source,
        }
    })?;
    let entries = std::fs::read_dir(&canonical_directory).map_err(|source| {
        ProductLibraryError::ReadDirectory {
            path: canonical_directory.clone(),
            source,
        }
    })?;
    let mut paths = entries
        .map(|entry| {
            entry
                .map(|value| value.path())
                .map_err(|source| ProductLibraryError::ReadEntry {
                    path: canonical_directory.clone(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort_unstable();

    let mut validated = Vec::new();
    for path in paths {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ProductLibraryError::NonUtf8Filename {
                directory: canonical_directory.clone(),
            })?;
        if Path::new(file_name)
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }

        let metadata = std::fs::symlink_metadata(&path).map_err(|source| {
            ProductLibraryError::InspectFile {
                path: path.clone(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ProductLibraryError::Symlink { path });
        }
        if !metadata.file_type().is_file() {
            return Err(ProductLibraryError::NotRegularFile { path });
        }
        if metadata.len() > MAX_PRODUCT_JSON_BYTES {
            return Err(ProductLibraryError::TooLarge {
                path,
                limit: MAX_PRODUCT_JSON_BYTES,
            });
        }

        let resolved =
            std::fs::canonicalize(&path).map_err(|source| ProductLibraryError::ResolveFile {
                path: path.clone(),
                source,
            })?;
        if !resolved.starts_with(&canonical_directory) {
            return Err(ProductLibraryError::EscapesDirectory {
                directory: canonical_directory,
                path: resolved,
            });
        }
        validated.push(resolved);
    }
    Ok(validated)
}

fn read_product(path: &Path) -> Result<ProductDefinition, ProductLibraryError> {
    let content =
        std::fs::read_to_string(path).map_err(|source| ProductLibraryError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    serde_json::from_str(&content).map_err(|source| ProductLibraryError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })
}

/// Runtime product library assembled from explicitly selected directories.
///
/// Later directories override earlier directories by product name. This lets a
/// site-owned directory deliberately refine models supplied by an active Pack.
#[derive(Debug, Default)]
pub struct ProductLibrary {
    products: Vec<ProductDefinition>,
}

impl ProductLibrary {
    /// Loads one explicitly selected directory.
    ///
    /// `None` produces an empty library. An explicitly selected missing,
    /// symlinked, or non-directory path fails closed; there is no embedded
    /// fallback product set.
    pub fn load(products_dir: Option<&Path>) -> Result<Self, ProductLibraryError> {
        let directories = products_dir.into_iter().collect::<Vec<_>>();
        Self::load_directories(&directories)
    }

    /// Loads product JSON from explicitly ordered directories.
    pub fn load_directories(directories: &[&Path]) -> Result<Self, ProductLibraryError> {
        let mut products = Vec::new();
        for directory in directories.iter().copied() {
            let mut directory_names = BTreeSet::new();
            for resolved in product_json_paths(directory)? {
                let product = read_product(&resolved)?;
                if product.name.is_empty() {
                    return Err(ProductLibraryError::EmptyProductName { path: resolved });
                }
                if !directory_names.insert(product.name.clone()) {
                    return Err(ProductLibraryError::DuplicateProduct {
                        name: product.name,
                        directory: directory.to_path_buf(),
                    });
                }

                if let Some(index) = products
                    .iter()
                    .position(|existing: &ProductDefinition| existing.name == product.name)
                {
                    products[index] = product;
                } else {
                    products.push(product);
                }
            }
        }
        Ok(Self { products })
    }

    /// Returns all selected product definitions.
    #[must_use]
    pub fn all(&self) -> &[ProductDefinition] {
        &self.products
    }

    /// Returns a product by identity.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ProductDefinition> {
        self.products.iter().find(|product| product.name == name)
    }

    /// Returns all product identities in deterministic load order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.products
            .iter()
            .map(|product| product.name.as_str())
            .collect()
    }

    /// Returns whether a product exists.
    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        self.products.iter().any(|product| product.name == name)
    }

    /// Returns the number of selected products.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.products.len()
    }

    /// Returns whether no product model was selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.products.is_empty()
    }

    /// Returns direct children of one parent product.
    #[must_use]
    pub fn children(&self, parent_name: &str) -> Vec<&ProductDefinition> {
        self.products
            .iter()
            .filter(|product| product.parent_name.as_deref() == Some(parent_name))
            .collect()
    }
}

/// Validates product JSON files without retaining their decoded definitions.
///
/// Returns `(filename, error)` entries for invalid files. A valid directory
/// returns an empty list.
#[must_use]
pub fn validate_product_directory(directory: &Path) -> Vec<(String, String)> {
    let paths = match product_json_paths(directory) {
        Ok(paths) => paths,
        Err(error) => return vec![("(directory)".to_string(), error.to_string())],
    };
    let mut errors = Vec::new();
    let mut names = BTreeSet::new();

    for path in paths {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
            .to_string();
        match read_product(&path) {
            Ok(product) if product.name.is_empty() => {
                errors.push((filename, "product name is empty".to_string()));
            },
            Ok(product) if !names.insert(product.name.clone()) => {
                errors.push((
                    filename,
                    format!("Product '{}' is declared more than once", product.name),
                ));
            },
            Ok(_) => {},
            Err(error) => errors.push((filename, error.to_string())),
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn default_and_unselected_libraries_are_empty() -> TestResult {
        assert!(ProductLibrary::default().is_empty());
        assert!(ProductLibrary::load(None)?.is_empty());
        let parent = tempfile::tempdir()?;
        let missing = parent.path().join("missing");
        assert!(matches!(
            ProductLibrary::load(Some(&missing)),
            Err(ProductLibraryError::ResolveDirectory { .. })
        ));
        Ok(())
    }

    #[test]
    fn explicitly_ordered_directories_allow_site_overrides() -> TestResult {
        let pack = tempfile::tempdir()?;
        let site = tempfile::tempdir()?;
        std::fs::write(
            pack.path().join("Device.json"),
            r#"{"name":"Device","M":[{"id":1,"name":"Pack value"}],"A":[],"P":[]}"#,
        )?;
        std::fs::write(
            site.path().join("Device.json"),
            r#"{"name":"Device","M":[{"id":1,"name":"Site value"}],"A":[],"P":[]}"#,
        )?;

        let library = ProductLibrary::load_directories(&[pack.path(), site.path()])?;

        assert_eq!(library.len(), 1);
        assert_eq!(
            library.get("Device").expect("Device").measurements[0].name,
            "Site value"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn selected_directory_rejects_json_symlinks() -> TestResult {
        use std::os::unix::fs::symlink;

        let selected = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let target = outside.path().join("Escaped.json");
        std::fs::write(&target, r#"{"name":"Escaped","M":[],"A":[],"P":[]}"#)?;
        symlink(target, selected.path().join("Escaped.json"))?;

        let error =
            ProductLibrary::load(Some(selected.path())).expect_err("JSON symlink must be rejected");
        assert!(matches!(error, ProductLibraryError::Symlink { .. }));
        assert_eq!(validate_product_directory(selected.path()).len(), 1);

        let link_parent = tempfile::tempdir()?;
        let directory_link = link_parent.path().join("models");
        symlink(selected.path(), &directory_link)?;
        assert!(matches!(
            ProductLibrary::load(Some(&directory_link)),
            Err(ProductLibraryError::DirectorySymlink { .. })
        ));
        Ok(())
    }

    #[test]
    fn selected_directory_rejects_non_regular_and_oversized_json() -> TestResult {
        let directory_entry = tempfile::tempdir()?;
        std::fs::create_dir(directory_entry.path().join("Directory.json"))?;
        assert!(matches!(
            ProductLibrary::load(Some(directory_entry.path())),
            Err(ProductLibraryError::NotRegularFile { .. })
        ));

        let oversized = tempfile::tempdir()?;
        std::fs::write(
            oversized.path().join("Huge.json"),
            vec![b'x'; MAX_PRODUCT_JSON_BYTES as usize + 1],
        )?;
        assert!(matches!(
            ProductLibrary::load(Some(oversized.path())),
            Err(ProductLibraryError::TooLarge { .. })
        ));
        assert_eq!(validate_product_directory(oversized.path()).len(), 1);
        Ok(())
    }

    #[test]
    fn duplicate_product_names_within_one_directory_fail_closed() -> TestResult {
        let selected = tempfile::tempdir()?;
        let product = r#"{"name":"Duplicate","M":[],"A":[],"P":[]}"#;
        std::fs::write(selected.path().join("First.json"), product)?;
        std::fs::write(selected.path().join("Second.json"), product)?;

        assert!(matches!(
            ProductLibrary::load(Some(selected.path())),
            Err(ProductLibraryError::DuplicateProduct { .. })
        ));
        assert_eq!(validate_product_directory(selected.path()).len(), 1);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_product_entry_fails_closed() -> TestResult {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let selected = tempfile::tempdir()?;
        let filename = OsString::from_vec(vec![0xff, b'.', b'j', b's', b'o', b'n']);
        std::fs::write(selected.path().join(filename), b"{}")?;

        assert!(matches!(
            ProductLibrary::load(Some(selected.path())),
            Err(ProductLibraryError::NonUtf8Filename { .. })
        ));
        assert_eq!(validate_product_directory(selected.path()).len(), 1);
        Ok(())
    }

    #[test]
    fn names_children_and_validation_come_only_from_selected_directory() -> TestResult {
        let selected = tempfile::tempdir()?;
        std::fs::write(
            selected.path().join("Root.json"),
            r#"{"name":"Root","M":[],"A":[],"P":[]}"#,
        )?;
        std::fs::write(
            selected.path().join("Device.json"),
            r#"{"name":"Device","pName":"Root","M":[],"A":[],"P":[]}"#,
        )?;

        let library = ProductLibrary::load(Some(selected.path()))?;
        assert_eq!(library.names(), vec!["Device", "Root"]);
        assert_eq!(library.children("Root")[0].name, "Device");
        assert!(validate_product_directory(selected.path()).is_empty());

        std::fs::write(selected.path().join("Bad.json"), "not json")?;
        assert_eq!(validate_product_directory(selected.path()).len(), 1);
        Ok(())
    }

    #[test]
    fn validation_rejects_empty_product_identity() -> TestResult {
        let selected = tempfile::tempdir()?;
        std::fs::write(
            selected.path().join("Empty.json"),
            r#"{"name":"","M":[],"A":[],"P":[]}"#,
        )?;

        assert!(matches!(
            ProductLibrary::load(Some(selected.path())),
            Err(ProductLibraryError::EmptyProductName { .. })
        ));
        let errors = validate_product_directory(selected.path());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].1.contains("empty"));
        Ok(())
    }
}
