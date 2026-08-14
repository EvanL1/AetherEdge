//! Offline JSON Schema validation for the checked-in runtime contract.
//!
//! Runtime consumers use the typed fail-closed loader in this crate. Release
//! and distribution checks additionally need to prove that the published JSON
//! Schema is itself valid and that checked-in instances conform to it. This
//! module is feature-gated so the validator is absent from the six-service
//! production dependency graph.

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use jsonschema::{Retrieve, Uri};
use serde_json::Value;
use thiserror::Error;

/// A checked-in runtime-manifest schema or instance failed offline validation.
#[derive(Debug, Error)]
pub enum RuntimeManifestSchemaError {
    /// Schema or instance JSON could not be read.
    #[error("cannot read JSON document {path}: {source}")]
    Read {
        /// Checked path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Schema or instance input was not valid JSON.
    #[error("invalid JSON document {path}: {source}")]
    Parse {
        /// Checked path.
        path: PathBuf,
        /// JSON decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// The schema does not conform to Draft 2020-12's bundled meta-schema.
    #[error("invalid Draft 2020-12 runtime-manifest schema: {message}")]
    InvalidMetaSchema {
        /// Validation detail.
        message: String,
    },
    /// The validated schema attempted external retrieval.
    #[error("runtime-manifest schema is not self-contained: {message}")]
    ExternalReference {
        /// Rejected reference detail.
        message: String,
    },
    /// An instance violates the checked-in runtime-manifest schema.
    #[error("runtime-manifest instance {path} violates its schema: {message}")]
    InvalidInstance {
        /// Invalid instance.
        path: PathBuf,
        /// Validation detail.
        message: String,
    },
}

#[derive(Debug)]
struct ExternalReferenceRejected(String);

impl Display for ExternalReferenceRejected {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "external schema reference {} is forbidden during offline validation",
            self.0
        )
    }
}

impl Error for ExternalReferenceRejected {}

#[derive(Debug)]
struct RejectExternalReferences;

impl Retrieve for RejectExternalReferences {
    fn retrieve(&self, uri: &Uri<String>) -> Result<Value, Box<dyn Error + Send + Sync>> {
        Err(Box::new(ExternalReferenceRejected(uri.as_str().to_owned())))
    }
}

/// Validate a Draft 2020-12 schema and every supplied instance without network
/// or filesystem reference retrieval.
pub fn validate_runtime_manifest_schema_files(
    schema_path: impl AsRef<Path>,
    instance_paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<(), RuntimeManifestSchemaError> {
    let schema_path = schema_path.as_ref();
    let schema = read_json(schema_path)?;
    let validator = build_runtime_manifest_schema_validator(&schema)?;

    for instance_path in instance_paths {
        let instance_path = instance_path.as_ref();
        let instance = read_json(instance_path)?;
        validator.validate(&instance).map_err(|error| {
            RuntimeManifestSchemaError::InvalidInstance {
                path: instance_path.to_path_buf(),
                message: error.to_string(),
            }
        })?;
    }
    Ok(())
}

/// Validate the schema document against the bundled Draft 2020-12
/// meta-schema, then compile it using a retriever that rejects external refs.
pub fn validate_runtime_manifest_schema(schema: &Value) -> Result<(), RuntimeManifestSchemaError> {
    build_runtime_manifest_schema_validator(schema).map(drop)
}

fn build_runtime_manifest_schema_validator(
    schema: &Value,
) -> Result<jsonschema::Validator, RuntimeManifestSchemaError> {
    jsonschema::draft202012::meta::validate(schema).map_err(|error| {
        RuntimeManifestSchemaError::InvalidMetaSchema {
            message: error.to_string(),
        }
    })?;
    jsonschema::draft202012::options()
        .with_retriever(RejectExternalReferences)
        .build(schema)
        .map_err(|error| RuntimeManifestSchemaError::ExternalReference {
            message: error.to_string(),
        })
}

fn read_json(path: &Path) -> Result<Value, RuntimeManifestSchemaError> {
    let source =
        std::fs::read_to_string(path).map_err(|source| RuntimeManifestSchemaError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    serde_json::from_str(&source).map_err(|source| RuntimeManifestSchemaError::Parse {
        path: path.to_path_buf(),
        source,
    })
}
