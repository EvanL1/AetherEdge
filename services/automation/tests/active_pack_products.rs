use std::fs;
use std::path::{Path, PathBuf};

use aether_automation::bootstrap::{
    load_pack_runtime_from_manifest, load_product_library, validate_instance_product_references,
};
use aether_pack::load_active_packs;

fn runtime() -> aether_pack::PackRuntime {
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["can", "gpio", "http", "modbus", "mqtt"],
    )
    .and_then(|manifest| manifest.pack_runtime())
    .expect("explicit Energy test composition")
}

fn repository_energy_pack() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/energy"))
        .expect("canonical repository energy pack")
}

fn write_global(config: &Path, packs: serde_json::Value) {
    fs::create_dir_all(config).expect("create config directory");
    let source = serde_json::to_string(&serde_json::json!({ "packs": packs }))
        .expect("serialize active pack config");
    fs::write(config.join("global.yaml"), source).expect("write global config");
}

fn write_model_pack(root: &Path, id: &str, declared: &str, actual: &str) {
    fs::create_dir_all(root.join("models")).expect("model directory");
    fs::write(
        root.join("models/model.json"),
        format!(r#"{{"name":"{actual}","M":[],"A":[],"P":[]}}"#),
    )
    .expect("model JSON");
    fs::write(
        root.join("pack.yaml"),
        format!(
            r#"schema_version: 1
id: {id}
name: Test Pack
version: 0.5.0
status: test
description: Active model contract test
distribution:
  id: test-distribution
  version: 0.5.0
  composition: test-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities: []
  required_protocols: []
assets:
  models: models
examples:
  commissioned: false
capabilities:
  models:
    - {declared}
"#
        ),
    )
    .expect("pack manifest");
}

#[test]
fn production_pack_loader_rejects_a_missing_runtime_manifest() {
    let config = tempfile::tempdir().expect("config directory");
    write_global(config.path(), serde_json::json!([]));

    let error = load_pack_runtime_from_manifest(config.path())
        .expect_err("automation startup must not use a static runtime fallback");

    assert!(error.to_string().contains("runtime-manifest.json"));
}

#[test]
fn fresh_site_without_an_active_pack_has_zero_products() {
    let config = tempfile::tempdir().expect("config directory");
    write_global(config.path(), serde_json::json!([]));
    let active = load_active_packs(config.path(), &runtime()).expect("empty active pack set");

    let library = load_product_library(&active, None).expect("empty product library");

    assert_eq!(library.len(), 0);
}

#[test]
fn explicitly_activated_energy_pack_supplies_its_thirteen_models() {
    let config = tempfile::tempdir().expect("config directory");
    write_global(
        config.path(),
        serde_json::json!([{ "id": "energy", "root": repository_energy_pack() }]),
    );
    let active = load_active_packs(config.path(), &runtime()).expect("validated energy pack");

    let library = load_product_library(&active, None).expect("pack product library");

    assert_eq!(library.len(), 13);
    assert!(library.exists("Battery"));
}

#[test]
fn explicit_site_product_directory_remains_available_without_a_pack() {
    let config = tempfile::tempdir().expect("config directory");
    let custom = tempfile::tempdir().expect("site products");
    write_global(config.path(), serde_json::json!([]));
    fs::write(
        custom.path().join("SiteDevice.json"),
        r#"{"name":"SiteDevice","M":[],"A":[],"P":[]}"#,
    )
    .expect("write site product");
    let active = load_active_packs(config.path(), &runtime()).expect("empty active pack set");

    let library = load_product_library(&active, Some(custom.path())).expect("site product library");

    assert_eq!(library.names(), vec!["SiteDevice"]);
}

#[test]
fn explicitly_configured_missing_or_non_directory_site_products_fail_closed() {
    let config = tempfile::tempdir().expect("config directory");
    let site = tempfile::tempdir().expect("site root");
    write_global(config.path(), serde_json::json!([]));
    let active = load_active_packs(config.path(), &runtime()).expect("empty active Pack set");
    let missing = site.path().join("missing-products");

    let missing_error = load_product_library(&active, Some(&missing))
        .expect_err("missing explicit site product path must fail");

    assert!(missing_error.to_string().contains("missing-products"));

    let file = site.path().join("products.json");
    fs::write(&file, "{}").expect("non-directory product path");
    let file_error = load_product_library(&active, Some(&file))
        .expect_err("file-valued explicit site product path must fail");
    assert!(file_error.to_string().contains("directory"));
}

#[test]
fn pack_model_files_must_exactly_match_manifest_model_capabilities() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let pack = site.path().join("pack");
    write_model_pack(&pack, "model-pack", "Declared", "Actual");
    write_global(
        &config,
        serde_json::json!([{ "id": "model-pack", "root": pack }]),
    );
    let active = load_active_packs(&config, &runtime()).expect("validated active Pack");

    let error = load_product_library(&active, None)
        .expect_err("manifest and actual model names must match exactly");

    assert!(error.to_string().contains("Declared"));
    assert!(error.to_string().contains("Actual"));
}

#[test]
fn two_active_packs_cannot_silently_override_the_same_product() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let first = site.path().join("first");
    let second = site.path().join("second");
    write_model_pack(&first, "first-pack", "Shared", "Shared");
    write_model_pack(&second, "second-pack", "Shared", "Shared");
    write_global(
        &config,
        serde_json::json!([
            { "id": "first-pack", "root": first },
            { "id": "second-pack", "root": second }
        ]),
    );
    let active = load_active_packs(&config, &runtime()).expect("validated active Packs");

    let error = load_product_library(&active, None)
        .expect_err("cross-Pack product collision must fail closed");

    assert!(error.to_string().contains("Shared"));
    assert!(error.to_string().contains("first-pack"));
    assert!(error.to_string().contains("second-pack"));
}

#[test]
fn explicit_site_directory_may_override_an_active_pack_product() {
    let config = tempfile::tempdir().expect("config directory");
    let custom = tempfile::tempdir().expect("site products");
    write_global(
        config.path(),
        serde_json::json!([{ "id": "energy", "root": repository_energy_pack() }]),
    );
    fs::write(
        custom.path().join("Battery.json"),
        r#"{"name":"Battery","pName":"ESS","M":[{"id":1,"name":"Site SOC"}],"A":[],"P":[]}"#,
    )
    .expect("site Battery override");
    let active = load_active_packs(config.path(), &runtime()).expect("validated energy Pack");

    let library =
        load_product_library(&active, Some(custom.path())).expect("site override library");

    assert_eq!(library.len(), 13);
    assert_eq!(
        library.get("Battery").expect("Battery").measurements[0].name,
        "Site SOC"
    );
}

#[tokio::test]
async fn existing_instance_with_an_inactive_product_fails_startup_validation() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("SQLite pool");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) VALUES (1, 'battery', 'Battery')",
    )
    .execute(&pool)
    .await
    .expect("existing instance");
    let library =
        aether_model::product_lib::ProductLibrary::load(None).expect("empty product library");

    let error = validate_instance_product_references(&pool, &library)
        .await
        .expect_err("dangling instance product must fail closed");

    assert!(error.to_string().contains("Battery"));
}

#[tokio::test]
async fn empty_site_accepts_an_empty_product_library() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("SQLite pool");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    let library =
        aether_model::product_lib::ProductLibrary::load(None).expect("empty product library");

    validate_instance_product_references(&pool, &library)
        .await
        .expect("empty site remains valid");
}
