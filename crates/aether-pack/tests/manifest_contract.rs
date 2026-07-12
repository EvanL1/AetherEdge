use std::fs;
use std::path::{Path, PathBuf};

use aether_pack::{
    AssetPathErrorKind, PackError, PackRuntime, load_pack_manifest, parse_pack_manifest,
};
use tempfile::TempDir;

const VALID_MANIFEST: &str = r#"
schema_version: 1
id: demo
name: Demo Industry Pack
version: 1.2.3
status: stable
description: Industry-neutral conformance fixture
distribution:
  id: demo-distribution
  version: 4.5.6
  composition: demo-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities:
    - point.read
  required_protocols:
    - modbus_tcp
assets:
  config: examples/config
  data_processing: data-processing
examples:
  commissioned: false
capabilities:
  models:
    - DemoDevice
  rule_topics:
    - demo-control
  data_processing_tasks:
    - demo.forecast
"#;

fn pack_root() -> TempDir {
    let root = tempfile::tempdir().expect("temporary pack root");
    fs::create_dir_all(root.path().join("examples/config")).expect("config asset directory");
    fs::create_dir_all(root.path().join("data-processing"))
        .expect("data-processing asset directory");
    fs::write(
        root.path().join("data-processing/index.yaml"),
        r#"schema: aether.pack.asset-index.v1
category: data_processing
assets:
  - id: demo.forecast
    path: forecast.yaml
    schema: aether.data-processing-task.v1
    media_type: application/yaml
"#,
    )
    .expect("data-processing asset index");
    fs::write(
        root.path().join("data-processing/forecast.yaml"),
        "schema: aether.data-processing-task.v1\nid: demo.forecast\nenabled: false\n",
    )
    .expect("data-processing task asset");
    root
}

fn runtime() -> PackRuntime {
    PackRuntime::new("0.5.0")
        .with_capabilities(["point.read"])
        .with_protocols(["modbus_tcp"])
}

#[test]
fn valid_v1_manifest_loads_from_its_pack_root() {
    let root = pack_root();
    fs::write(root.path().join("pack.yaml"), VALID_MANIFEST).expect("write fixture manifest");

    let manifest = load_pack_manifest(root.path(), &runtime()).expect("valid pack loads");

    assert_eq!(manifest.schema_version(), 1);
    assert_eq!(manifest.id(), "demo");
    assert_eq!(manifest.name(), "Demo Industry Pack");
    assert_eq!(manifest.version().to_string(), "1.2.3");
    assert_eq!(manifest.status(), "stable");
    assert_eq!(
        manifest.description(),
        "Industry-neutral conformance fixture"
    );
    assert_eq!(manifest.distribution().id(), "demo-distribution");
    assert_eq!(manifest.distribution().version().to_string(), "4.5.6");
    assert_eq!(manifest.distribution().composition(), "demo-gateway");
    assert_eq!(manifest.aether_requirement().to_string(), ">=0.5.0, <0.6.0");
    assert_eq!(
        manifest.asset_directory("config"),
        Some(Path::new("examples/config"))
    );
    assert_eq!(
        manifest.asset_directory("data_processing"),
        Some(Path::new("data-processing"))
    );
    assert_eq!(
        manifest.capability_ids("models"),
        Some(&["DemoDevice".to_string()][..])
    );
    assert!(!manifest.examples_commissioned());
    assert_eq!(manifest.required_capabilities(), &["point.read"]);
    assert_eq!(manifest.required_protocols(), &["modbus_tcp"]);
}

#[test]
fn loading_requires_a_readable_manifest_file() {
    let root = pack_root();

    let error = load_pack_manifest(root.path(), &runtime()).expect_err("pack.yaml is mandatory");

    assert!(matches!(error, PackError::ManifestRead { .. }));
}

#[test]
fn unknown_manifest_fields_fail_closed() {
    let root = pack_root();
    let source = VALID_MANIFEST.replace(
        "description: Industry-neutral conformance fixture",
        "description: Industry-neutral conformance fixture\nunknown_contract: true",
    );

    let error = parse_pack_manifest(&source, root.path(), &runtime())
        .expect_err("unknown fields must fail closed");

    assert!(matches!(error, PackError::InvalidManifest { .. }));
}

#[test]
fn legacy_asset_shim_is_not_part_of_the_v1_contract() {
    let root = pack_root();
    let source = VALID_MANIFEST.replace(
        "assets:\n",
        "legacy_assets:\n  models: ../../legacy-models\nassets:\n",
    );

    let error = parse_pack_manifest(&source, root.path(), &runtime())
        .expect_err("legacy repository paths must not enter Pack v1");

    assert!(matches!(error, PackError::InvalidManifest { .. }));
}

#[test]
fn unsupported_schema_version_has_a_typed_error() {
    let root = pack_root();
    let source = VALID_MANIFEST.replacen("schema_version: 1", "schema_version: 2", 1);

    let error = parse_pack_manifest(&source, root.path(), &runtime())
        .expect_err("unsupported schema must fail");

    assert!(matches!(
        error,
        PackError::UnsupportedSchema {
            found: 2,
            supported: 1
        }
    ));
}

#[test]
fn pack_and_distribution_versions_are_validated_independently() {
    let root = pack_root();
    let invalid_pack = VALID_MANIFEST.replacen("version: 1.2.3", "version: latest", 1);
    let invalid_distribution = VALID_MANIFEST.replacen("version: 4.5.6", "version: latest", 1);

    assert!(matches!(
        parse_pack_manifest(&invalid_pack, root.path(), &runtime()),
        Err(PackError::InvalidPackVersion { .. })
    ));
    assert!(matches!(
        parse_pack_manifest(&invalid_distribution, root.path(), &runtime()),
        Err(PackError::InvalidDistributionVersion { .. })
    ));
}

#[test]
fn incompatible_aether_release_fails_closed() {
    let root = pack_root();
    let incompatible = PackRuntime::new("0.6.0")
        .with_capabilities(["point.read"])
        .with_protocols(["modbus_tcp"]);

    let error = parse_pack_manifest(VALID_MANIFEST, root.path(), &incompatible)
        .expect_err("incompatible Aether release must fail");

    assert!(matches!(error, PackError::IncompatibleAether { .. }));
}

#[test]
fn malformed_aether_version_and_requirement_have_distinct_errors() {
    let root = pack_root();
    let invalid_requirement = VALID_MANIFEST.replacen(">=0.5.0,<0.6.0", "not-semver", 1);

    assert!(matches!(
        parse_pack_manifest(VALID_MANIFEST, root.path(), &PackRuntime::new("latest")),
        Err(PackError::InvalidAetherVersion { .. })
    ));
    assert!(matches!(
        parse_pack_manifest(&invalid_requirement, root.path(), &runtime()),
        Err(PackError::InvalidAetherRequirement { .. })
    ));
}

#[test]
fn missing_required_capability_and_protocol_have_typed_errors() {
    let root = pack_root();
    let no_capabilities = PackRuntime::new("0.5.0").with_protocols(["modbus_tcp"]);
    let no_protocols = PackRuntime::new("0.5.0").with_capabilities(["point.read"]);

    assert!(matches!(
        parse_pack_manifest(VALID_MANIFEST, root.path(), &no_capabilities),
        Err(PackError::UnknownCapability { ref id }) if id == "point.read"
    ));
    assert!(matches!(
        parse_pack_manifest(VALID_MANIFEST, root.path(), &no_protocols),
        Err(PackError::UnknownProtocol { ref id }) if id == "modbus_tcp"
    ));
}

#[test]
fn packs_without_runtime_requirements_or_optional_capability_groups_are_valid() {
    let root = pack_root();
    let source = VALID_MANIFEST
        .replace("    - point.read", "    []")
        .replace("    - modbus_tcp", "    []")
        .replace("  data_processing: data-processing\n", "")
        .replace(
            "capabilities:\n  models:\n    - DemoDevice\n  rule_topics:\n    - demo-control\n  data_processing_tasks:\n    - demo.forecast",
            "capabilities: {}",
        );

    let manifest = parse_pack_manifest(&source, root.path(), &PackRuntime::new("0.5.0"))
        .expect("a knowledge-only pack may require no executable capability");

    assert!(manifest.required_capabilities().is_empty());
    assert!(manifest.required_protocols().is_empty());
}

#[test]
fn malformed_and_duplicate_identifiers_fail_closed() {
    let root = pack_root();
    let malformed = VALID_MANIFEST.replacen("id: demo", "id: .demo", 1);
    let duplicate_requirement =
        VALID_MANIFEST.replacen("    - point.read", "    - point.read\n    - point.read", 1);
    let duplicate_capability =
        VALID_MANIFEST.replacen("    - DemoDevice", "    - DemoDevice\n    - DemoDevice", 1);

    assert!(matches!(
        parse_pack_manifest(&malformed, root.path(), &runtime()),
        Err(PackError::InvalidIdentifier { .. })
    ));
    assert!(matches!(
        parse_pack_manifest(&duplicate_requirement, root.path(), &runtime()),
        Err(PackError::DuplicateIdentifier { .. })
    ));
    assert!(matches!(
        parse_pack_manifest(&duplicate_capability, root.path(), &runtime()),
        Err(PackError::DuplicateIdentifier { .. })
    ));
}

#[test]
fn asset_directories_reject_absolute_and_parent_paths() {
    let root = pack_root();
    let absolute = VALID_MANIFEST.replacen("examples/config", "/etc/aether", 1);
    let traversal = VALID_MANIFEST.replacen("examples/config", "../outside", 1);

    assert!(matches!(
        parse_pack_manifest(&absolute, root.path(), &runtime()),
        Err(PackError::InvalidAssetPath {
            kind: AssetPathErrorKind::Absolute,
            ..
        })
    ));
    assert!(matches!(
        parse_pack_manifest(&traversal, root.path(), &runtime()),
        Err(PackError::InvalidAssetPath {
            kind: AssetPathErrorKind::ParentTraversal,
            ..
        })
    ));
}

#[test]
fn asset_directories_reject_portable_windows_absolute_paths() {
    let root = pack_root();
    let drive = VALID_MANIFEST.replacen("examples/config", "C:\\aether\\config", 1);
    let unc = VALID_MANIFEST.replacen("examples/config", "\\\\server\\share", 1);

    assert!(matches!(
        parse_pack_manifest(&drive, root.path(), &runtime()),
        Err(PackError::InvalidAssetPath {
            kind: AssetPathErrorKind::Absolute,
            ..
        })
    ));
    assert!(matches!(
        parse_pack_manifest(&unc, root.path(), &runtime()),
        Err(PackError::InvalidAssetPath {
            kind: AssetPathErrorKind::Absolute,
            ..
        })
    ));
}

#[test]
fn missing_asset_directory_and_commissioned_examples_fail_closed() {
    let root = pack_root();
    let missing = VALID_MANIFEST.replacen("examples/config", "missing", 1);
    let commissioned = VALID_MANIFEST.replacen("commissioned: false", "commissioned: true", 1);

    assert!(matches!(
        parse_pack_manifest(&missing, root.path(), &runtime()),
        Err(PackError::AssetDirectoryUnavailable { .. })
    ));
    assert!(matches!(
        parse_pack_manifest(&commissioned, root.path(), &runtime()),
        Err(PackError::CommissionedExamples)
    ));
}

#[test]
fn pack_root_and_asset_kind_are_validated() {
    let root = pack_root();
    fs::write(root.path().join("asset-file"), "not a directory").expect("asset file");
    let file_asset = VALID_MANIFEST.replacen("examples/config", "asset-file", 1);
    let missing_root = root.path().join("missing-root");

    assert!(matches!(
        parse_pack_manifest(&file_asset, root.path(), &runtime()),
        Err(PackError::AssetDirectoryUnavailable { .. })
    ));
    assert!(matches!(
        parse_pack_manifest(VALID_MANIFEST, &missing_root, &runtime()),
        Err(PackError::PackRootUnavailable { .. })
    ));
}

#[cfg(unix)]
#[test]
fn asset_directory_symlink_cannot_escape_the_pack_root() {
    use std::os::unix::fs::symlink;

    let root = pack_root();
    let outside = tempfile::tempdir().expect("outside directory");
    symlink(outside.path(), root.path().join("escape")).expect("create escape symlink");
    let source = VALID_MANIFEST.replacen("examples/config", "escape", 1);

    let error = parse_pack_manifest(&source, root.path(), &runtime())
        .expect_err("symlink escape must fail");

    assert!(matches!(
        error,
        PackError::InvalidAssetPath {
            kind: AssetPathErrorKind::EscapesRoot,
            ..
        }
    ));
}

#[test]
fn v1_json_schema_declares_the_same_fail_closed_surface() {
    let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/pack/pack-manifest.v1.schema.json");
    if !schema_path.is_file() {
        // The JSON Schema is a repository/release contract, not duplicated in
        // the Cargo crate tarball. Workspace CI validates it separately.
        return;
    }
    let schema: serde_json::Value = serde_json::from_slice(
        &fs::read(&schema_path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", schema_path.display())),
    )
    .expect("schema must be JSON");

    assert_eq!(
        schema["$id"],
        "https://aether.dev/schemas/pack-manifest.v1.json"
    );
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["distribution"]["additionalProperties"],
        false
    );
    assert_eq!(
        schema["properties"]["compatibility"]["additionalProperties"],
        false
    );
    assert_eq!(
        schema["properties"]["examples"]["properties"]["commissioned"]["const"],
        false
    );
    assert!(
        schema["required"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "version"))
    );
    for category in ["mappings", "rules", "evaluations"] {
        assert!(
            schema["properties"]["capabilities"]["properties"]
                .get(category)
                .is_some(),
            "Pack manifest schema omits {category} capability IDs"
        );
    }

    let index_schema_path = schema_path.with_file_name("pack-asset-index.v1.schema.json");
    let index_schema: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_schema_path).unwrap_or_else(|error| {
            panic!("failed to read {}: {error}", index_schema_path.display())
        }))
        .expect("asset index schema must be JSON");
    assert_eq!(
        index_schema["properties"]["schema"]["const"],
        aether_pack::PACK_ASSET_INDEX_SCHEMA
    );
    assert!(
        index_schema["properties"]["category"]["enum"]
            .as_array()
            .is_some_and(|categories| categories
                .iter()
                .any(|category| category == "data_processing"))
    );
}
