use std::fs;
use std::path::Path;

use aether_pack::{
    ActivePackError, PackError, PackRuntime, load_active_packs, parse_active_packs_config,
};

const MANIFEST: &str = r#"schema_version: 1
id: energy
name: Energy
version: 0.5.0
status: stable
description: Test energy pack
distribution:
  id: aether-ems
  version: 0.5.0
  composition: energy-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities:
    - device.read_point
  required_protocols:
    - modbus_tcp
assets:
  models: models
  knowledge: knowledge
examples:
  commissioned: false
capabilities:
  models:
    - Battery
"#;

fn runtime() -> PackRuntime {
    PackRuntime::new("0.5.0")
        .with_capabilities(["device.read_point"])
        .with_protocols(["modbus_tcp"])
}

fn write_pack(root: &Path, manifest: &str) {
    fs::create_dir_all(root.join("models")).expect("create model asset directory");
    fs::create_dir_all(root.join("knowledge")).expect("create knowledge asset directory");
    fs::write(root.join("pack.yaml"), manifest).expect("write pack manifest");
}

fn write_global(config: &Path, packs: serde_json::Value) {
    fs::create_dir_all(config).expect("create config directory");
    let source = serde_yml::to_string(&serde_json::json!({ "packs": packs }))
        .expect("serialize active pack config");
    fs::write(config.join("global.yaml"), source).expect("write global config");
}

#[test]
fn empty_active_pack_list_is_a_valid_fail_safe_default() {
    let config = tempfile::tempdir().expect("config directory");
    write_global(config.path(), serde_json::json!([]));

    let active = load_active_packs(config.path(), &runtime()).expect("load empty active pack set");

    assert!(active.is_empty());
    assert_eq!(active.len(), 0);
}

#[test]
fn configured_identity_and_root_produce_a_validated_active_pack() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let root = site.path().join("packs/energy");
    write_pack(&root, MANIFEST);
    write_global(
        &config,
        serde_json::json!([{ "id": "energy", "root": root }] ),
    );

    let active = load_active_packs(&config, &runtime()).expect("load active energy pack");
    let pack = active.get("energy").expect("validated energy identity");

    assert_eq!(pack.id(), "energy");
    assert_eq!(
        pack.root(),
        fs::canonicalize(root).expect("canonical pack root")
    );
    assert_eq!(
        pack.manifest().asset_directory("models"),
        Some(Path::new("models"))
    );
}

#[test]
fn relative_roots_are_resolved_from_the_shared_config_directory() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let root = config.join("installed-packs/energy");
    write_pack(&root, MANIFEST);
    write_global(
        &config,
        serde_json::json!([{ "id": "energy", "root": "installed-packs/energy" }] ),
    );

    let active = load_active_packs(&config, &runtime()).expect("load relative active pack");

    assert_eq!(
        active.get("energy").expect("energy pack").root(),
        fs::canonicalize(root).expect("canonical root")
    );
}

#[test]
fn candidate_source_is_validated_without_replacing_global_config() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let root = config.join("installed-packs/energy");
    write_pack(&root, MANIFEST);
    write_global(&config, serde_json::json!([]));
    let before = fs::read(config.join("global.yaml")).expect("current config");
    let candidate = serde_yml::to_string(&serde_json::json!({
        "site_name": "preserved",
        "packs": [{ "id": "energy", "root": "installed-packs/energy" }]
    }))
    .expect("candidate config");

    let active = parse_active_packs_config(&candidate, &config, &runtime())
        .expect("validate candidate active Pack set");

    assert_eq!(
        active.get("energy").expect("candidate Pack").root(),
        fs::canonicalize(root).expect("canonical candidate Pack root")
    );
    assert_eq!(
        fs::read(config.join("global.yaml")).expect("unchanged current config"),
        before
    );
}

#[test]
fn identity_mismatch_and_parent_traversal_fail_closed() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let root = site.path().join("energy");
    write_pack(&root, MANIFEST);
    write_global(
        &config,
        serde_json::json!([{ "id": "not-energy", "root": root }] ),
    );

    assert!(matches!(
        load_active_packs(&config, &runtime()),
        Err(ActivePackError::IdentityMismatch { configured, manifest })
            if configured == "not-energy" && manifest == "energy"
    ));

    write_global(
        &config,
        serde_json::json!([{ "id": "energy", "root": "../energy" }] ),
    );
    assert!(matches!(
        load_active_packs(&config, &runtime()),
        Err(ActivePackError::InvalidRoot { .. })
    ));
}

#[test]
fn incompatible_or_unknown_requirements_are_not_activated() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let root = site.path().join("energy");
    write_global(
        &config,
        serde_json::json!([{ "id": "energy", "root": root }] ),
    );

    write_pack(&root, &MANIFEST.replace(">=0.5.0,<0.6.0", ">=0.6.0,<0.7.0"));
    assert!(matches!(
        load_active_packs(&config, &runtime()),
        Err(ActivePackError::InvalidPack { source, .. })
            if matches!(source.as_ref(), PackError::IncompatibleAether { .. })
    ));

    write_pack(
        &root,
        &MANIFEST.replace("device.read_point", "device.unknown"),
    );
    assert!(matches!(
        load_active_packs(&config, &runtime()),
        Err(ActivePackError::InvalidPack { source, .. })
            if matches!(source.as_ref(), PackError::UnknownCapability { .. })
    ));
}

#[test]
fn duplicate_identity_and_root_fail_closed() {
    let site = tempfile::tempdir().expect("site root");
    let config = site.path().join("config");
    let first = site.path().join("first");
    let second = site.path().join("second");
    write_pack(&first, MANIFEST);
    write_pack(&second, MANIFEST);
    write_global(
        &config,
        serde_json::json!([
            { "id": "energy", "root": first },
            { "id": "energy", "root": second }
        ]),
    );

    assert!(matches!(
        load_active_packs(&config, &runtime()),
        Err(ActivePackError::DuplicateIdentity { id }) if id == "energy"
    ));

    write_global(
        &config,
        serde_json::json!([
            { "id": "energy", "root": first },
            { "id": "energy-alias", "root": first }
        ]),
    );
    assert!(matches!(
        load_active_packs(&config, &runtime()),
        Err(ActivePackError::DuplicateRoot { .. })
    ));
}

#[test]
fn malformed_or_missing_global_config_fails_closed() {
    let config = tempfile::tempdir().expect("config directory");
    fs::write(
        config.path().join("global.yaml"),
        "packs:\n  - id: energy\n",
    )
    .expect("malformed packs shape");

    assert!(matches!(
        load_active_packs(config.path(), &runtime()),
        Err(ActivePackError::InvalidConfig { .. })
    ));

    fs::remove_file(config.path().join("global.yaml")).expect("remove global config");
    assert!(matches!(
        load_active_packs(config.path(), &runtime()),
        Err(ActivePackError::ConfigRead { .. })
    ));
}
