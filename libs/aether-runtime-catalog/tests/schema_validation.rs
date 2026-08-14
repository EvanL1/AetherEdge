#![cfg(feature = "schema-validation")]

use std::fs;
use std::path::{Path, PathBuf};

use aether_runtime_catalog::schema_validation::{
    RuntimeManifestSchemaError, validate_runtime_manifest_schema,
    validate_runtime_manifest_schema_files,
};
use serde_json::{Value, json};

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn checked_schema() -> PathBuf {
    repository_path("contracts/runtime/runtime-manifest.schema.json")
}

fn read_value(relative: &str) -> Value {
    serde_json::from_str(
        &fs::read_to_string(repository_path(relative)).expect("read checked-in JSON fixture"),
    )
    .expect("parse checked-in JSON fixture")
}

#[test]
fn checked_schema_and_both_distribution_instances_validate_offline() {
    validate_runtime_manifest_schema_files(
        checked_schema(),
        [
            repository_path("config.template/runtime-manifest.json"),
            repository_path("config.e2e/runtime-manifest.json"),
        ],
    )
    .expect("checked-in schema and instances are valid");
}

#[test]
fn invalid_schema_keyword_value_is_rejected_by_the_bundled_meta_schema() {
    let invalid = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "not-a-json-schema-type"
    });

    assert!(matches!(
        validate_runtime_manifest_schema(&invalid),
        Err(RuntimeManifestSchemaError::InvalidMetaSchema { .. })
    ));
}

#[test]
fn external_schema_references_fail_closed_instead_of_touching_the_network() {
    let external = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://schemas.example.invalid/runtime.json"
    });

    let error = validate_runtime_manifest_schema(&external)
        .expect_err("external references must be rejected offline");
    assert!(matches!(
        error,
        RuntimeManifestSchemaError::ExternalReference { .. }
    ));
    assert!(error.to_string().contains("schemas.example.invalid"));
}

#[test]
fn malformed_template_instance_is_rejected_by_the_json_schema() {
    let root = tempfile::tempdir().expect("temporary validation directory");
    let invalid_path = root.path().join("invalid-template.json");
    let mut invalid = read_value("config.template/runtime-manifest.json");
    invalid["composition"] = json!(1);
    fs::write(
        &invalid_path,
        serde_json::to_vec(&invalid).expect("serialize bad instance"),
    )
    .expect("write bad instance");

    assert!(matches!(
        validate_runtime_manifest_schema_files(checked_schema(), [&invalid_path]),
        Err(RuntimeManifestSchemaError::InvalidInstance { path, .. }) if path == invalid_path
    ));
}

#[test]
fn malformed_e2e_instance_is_rejected_by_the_json_schema() {
    let root = tempfile::tempdir().expect("temporary validation directory");
    let invalid_path = root.path().join("invalid-e2e.json");
    let mut invalid = read_value("config.e2e/runtime-manifest.json");
    invalid
        .as_object_mut()
        .expect("runtime manifest object")
        .remove("checksum");
    fs::write(
        &invalid_path,
        serde_json::to_vec(&invalid).expect("serialize bad instance"),
    )
    .expect("write bad instance");

    assert!(matches!(
        validate_runtime_manifest_schema_files(checked_schema(), [&invalid_path]),
        Err(RuntimeManifestSchemaError::InvalidInstance { path, .. }) if path == invalid_path
    ));
}
