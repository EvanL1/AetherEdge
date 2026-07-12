use std::fs;
use std::path::Path;

use aether_pack::{
    MAX_PACK_ASSET_BYTES, MAX_PACK_ASSET_INDEX_BYTES, PackError, PackRuntime, load_active_packs,
    load_pack_manifest,
};

const MANIFEST: &str = r#"schema_version: 1
id: demo
name: Demo
version: 1.0.0
status: stable
description: Indexed asset fixture
distribution:
  id: demo-distribution
  version: 1.0.0
  composition: demo-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities: []
  required_protocols: []
assets:
  rules: rules
examples:
  commissioned: false
capabilities:
  rules:
    - demo.safe-rule
"#;

const INDEX: &str = r#"schema: aether.pack.asset-index.v1
category: rules
assets:
  - id: demo.safe-rule
    path: safe-rule.json
    schema: aether.pack.rule.v1
    media_type: application/json
"#;

const RULE: &str = r#"{
  "schema": "aether.pack.rule.v1",
  "id": "demo.safe-rule",
  "enabled": false
}"#;

const DATA_PROCESSING_MANIFEST: &str = r#"schema_version: 1
id: demo
name: Demo
version: 1.0.0
status: stable
description: Data Processing task index fixture
distribution:
  id: demo-distribution
  version: 1.0.0
  composition: demo-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities: []
  required_protocols: []
assets:
  data_processing: data-processing/tasks
examples:
  commissioned: false
capabilities:
  data_processing_tasks:
    - demo.forecast
"#;

const DATA_PROCESSING_INDEX: &str = r#"schema: aether.pack.asset-index.v1
category: data_processing
assets:
  - id: demo.forecast
    path: forecast.yaml
    schema: aether.data-processing-task.v1
    media_type: application/yaml
"#;

const DATA_PROCESSING_TASK: &str = r#"schema: aether.data-processing-task.v1
id: demo.forecast
revision: 1
enabled: false
"#;

fn runtime() -> PackRuntime {
    PackRuntime::new("0.5.0")
}

fn write_valid_pack(root: &Path) {
    fs::create_dir_all(root.join("rules")).expect("rule directory");
    fs::write(root.join("pack.yaml"), MANIFEST).expect("manifest");
    fs::write(root.join("rules/index.yaml"), INDEX).expect("index");
    fs::write(root.join("rules/safe-rule.json"), RULE).expect("rule");
}

fn write_data_processing_pack(root: &Path) {
    fs::create_dir_all(root.join("data-processing/tasks")).expect("task directory");
    fs::write(root.join("pack.yaml"), DATA_PROCESSING_MANIFEST).expect("manifest");
    fs::write(
        root.join("data-processing/tasks/index.yaml"),
        DATA_PROCESSING_INDEX,
    )
    .expect("task index");
    fs::write(
        root.join("data-processing/tasks/forecast.yaml"),
        DATA_PROCESSING_TASK,
    )
    .expect("task");
}

#[test]
fn data_processing_tasks_are_a_formal_exact_pack_index() {
    let root = tempfile::tempdir().expect("pack root");
    write_data_processing_pack(root.path());

    let manifest = load_pack_manifest(root.path(), &runtime()).expect("task pack");
    let index = manifest
        .asset_index("data_processing")
        .expect("Data Processing task assets require a validated index");
    assert_eq!(index.assets()[0].id(), "demo.forecast");

    fs::write(
        root.path()
            .join("data-processing/tasks/unindexed-task.yaml"),
        DATA_PROCESSING_TASK.replace("demo.forecast", "demo.other"),
    )
    .expect("unindexed task");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetInventoryMismatch { category, .. })
            if category == "data_processing"
    ));
}

#[test]
fn indexed_assets_require_exact_manifest_index_and_directory_inventory() {
    let root = tempfile::tempdir().expect("pack root");
    write_valid_pack(root.path());

    let manifest = load_pack_manifest(root.path(), &runtime()).expect("valid indexed pack");
    let index = manifest.asset_index("rules").expect("validated rule index");

    assert_eq!(index.category(), "rules");
    assert_eq!(index.assets().len(), 1);
    assert_eq!(index.assets()[0].id(), "demo.safe-rule");
    assert_eq!(index.assets()[0].path(), Path::new("safe-rule.json"));
    assert_eq!(index.assets()[0].schema(), "aether.pack.rule.v1");

    fs::write(root.path().join("rules/unindexed.json"), RULE).expect("unknown file");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetInventoryMismatch { category, .. }) if category == "rules"
    ));

    fs::remove_file(root.path().join("rules/unindexed.json")).expect("remove unknown file");
    fs::write(
        root.path().join("pack.yaml"),
        MANIFEST.replace("    - demo.safe-rule", "    - demo.other-rule"),
    )
    .expect("mismatched manifest IDs");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetInventoryMismatch { category, .. }) if category == "rules"
    ));
}

#[test]
fn duplicate_ids_paths_and_unknown_index_fields_fail_closed() {
    let root = tempfile::tempdir().expect("pack root");
    write_valid_pack(root.path());
    let duplicate_id = format!(
        "{INDEX}  - id: demo.safe-rule\n    path: second.json\n    schema: aether.pack.rule.v1\n    media_type: application/json\n"
    );
    fs::write(root.path().join("rules/index.yaml"), duplicate_id).expect("duplicate id index");
    fs::write(root.path().join("rules/second.json"), RULE).expect("second rule");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::DuplicateAssetId { category, id })
            if category == "rules" && id == "demo.safe-rule"
    ));

    let duplicate_path = format!(
        "{INDEX}  - id: demo.other-rule\n    path: safe-rule.json\n    schema: aether.pack.rule.v1\n    media_type: application/json\n"
    );
    fs::write(root.path().join("rules/index.yaml"), duplicate_path).expect("duplicate path index");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::DuplicateAssetPath { category, .. }) if category == "rules"
    ));

    fs::write(
        root.path().join("rules/index.yaml"),
        INDEX.replace("category: rules", "category: rules\nunknown: rejected"),
    )
    .expect("unknown field index");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::InvalidAssetIndex { category, .. }) if category == "rules"
    ));
}

#[test]
fn indexed_asset_path_escape_and_oversize_content_fail_closed() {
    let root = tempfile::tempdir().expect("pack root");
    write_valid_pack(root.path());
    fs::write(root.path().join("outside.json"), RULE).expect("outside rule");
    fs::write(
        root.path().join("rules/index.yaml"),
        INDEX.replace("safe-rule.json", "../outside.json"),
    )
    .expect("escaping index");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::InvalidAssetFilePath { category, .. }) if category == "rules"
    ));

    fs::write(
        root.path().join("rules/index.yaml"),
        INDEX.replace("safe-rule.json", "..\\outside.json"),
    )
    .expect("portable escaping index");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::InvalidAssetFilePath { category, .. }) if category == "rules"
    ));

    write_valid_pack(root.path());
    fs::write(
        root.path().join("rules/safe-rule.json"),
        vec![b' '; MAX_PACK_ASSET_BYTES + 1],
    )
    .expect("oversize rule");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetFileTooLarge { category, .. }) if category == "rules"
    ));
}

#[test]
fn oversized_index_and_mismatched_asset_metadata_fail_closed() {
    let root = tempfile::tempdir().expect("pack root");
    write_valid_pack(root.path());
    fs::write(
        root.path().join("rules/index.yaml"),
        vec![b' '; MAX_PACK_ASSET_INDEX_BYTES + 1],
    )
    .expect("oversize index");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetFileTooLarge { category, .. }) if category == "rules"
    ));

    write_valid_pack(root.path());
    fs::write(
        root.path().join("rules/safe-rule.json"),
        RULE.replace("aether.pack.rule.v1", "aether.pack.rule.v2"),
    )
    .expect("mismatched rule schema");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetInventoryMismatch { category, .. }) if category == "rules"
    ));

    fs::write(
        root.path().join("rules/index.yaml"),
        INDEX.replace("aether.pack.rule.v1", "aether.pack.rule.v2"),
    )
    .expect("unsupported rule schema index");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::InvalidAssetIndex { category, .. }) if category == "rules"
    ));

    write_valid_pack(root.path());
    fs::write(
        root.path().join("rules/index.yaml"),
        INDEX.replace("application/json", "application/octet-stream"),
    )
    .expect("unsupported media type");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::InvalidAssetIndex { category, .. }) if category == "rules"
    ));
}

#[cfg(unix)]
#[test]
fn indexed_asset_symlinks_are_rejected_even_when_the_target_is_inside_the_pack() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("pack root");
    write_valid_pack(root.path());
    fs::rename(
        root.path().join("rules/safe-rule.json"),
        root.path().join("rules/real-rule.json"),
    )
    .expect("rename real asset");
    symlink("real-rule.json", root.path().join("rules/safe-rule.json")).expect("asset symlink");

    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetFileSymlink { category, .. }) if category == "rules"
    ));
}

#[cfg(unix)]
#[test]
fn indexed_directory_and_index_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("pack root");
    write_valid_pack(root.path());
    fs::rename(root.path().join("rules"), root.path().join("real-rules"))
        .expect("rename asset directory");
    symlink("real-rules", root.path().join("rules")).expect("asset directory symlink");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetFileSymlink { category, .. }) if category == "rules"
    ));

    fs::remove_file(root.path().join("rules")).expect("remove directory symlink");
    fs::rename(root.path().join("real-rules"), root.path().join("rules"))
        .expect("restore asset directory");
    fs::rename(
        root.path().join("rules/index.yaml"),
        root.path().join("rules/real-index.yaml"),
    )
    .expect("rename index");
    symlink("real-index.yaml", root.path().join("rules/index.yaml")).expect("index symlink");
    assert!(matches!(
        load_pack_manifest(root.path(), &runtime()),
        Err(PackError::AssetFileSymlink { category, .. }) if category == "rules"
    ));
}

#[test]
fn active_pack_asset_namespaces_follow_only_the_explicit_active_set() {
    let site = tempfile::tempdir().expect("site");
    let config = site.path().join("config");
    let pack = site.path().join("packs/demo");
    fs::create_dir_all(&config).expect("config");
    write_valid_pack(&pack);
    fs::write(config.join("global.yaml"), "packs: []\n").expect("empty config");

    let empty = load_active_packs(&config, &runtime()).expect("empty active set");
    assert!(empty.namespaced_asset_ids("rules").is_empty());

    fs::write(
        config.join("global.yaml"),
        format!("packs:\n  - id: demo\n    root: {}\n", pack.display()),
    )
    .expect("active config");
    let active = load_active_packs(&config, &runtime()).expect("active demo pack");
    let canonical_pack = fs::canonicalize(&pack).expect("canonical demo Pack root");
    assert_eq!(
        active.namespaced_asset_ids("rules"),
        vec!["demo/rules/demo.safe-rule"]
    );
    assert!(active.namespaced_asset_ids("evaluations").is_empty());
    assert_eq!(
        active
            .get("demo")
            .and_then(|pack| pack.asset_file("rules", "demo.safe-rule")),
        Some(canonical_pack.join("rules/safe-rule.json"))
    );
    assert!(
        active
            .get("demo")
            .and_then(|pack| pack.asset_file("rules", "demo.unknown"))
            .is_none()
    );
}
