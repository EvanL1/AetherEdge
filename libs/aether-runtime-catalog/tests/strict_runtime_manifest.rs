use std::fs;
use std::path::Path;

use aether_runtime_catalog::{
    MAX_RUNTIME_MANIFEST_BYTES, RuntimeManifestError, default_io_features,
    known_io_protocol_features, load_runtime_manifest_file,
    load_runtime_manifest_for_current_process, parse_runtime_manifest,
    shipped_distribution_manifest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn valid_document() -> Value {
    serde_json::from_str(
        &shipped_distribution_manifest("aarch64-unknown-linux-musl")
            .expect("shipped manifest")
            .to_pretty_json()
            .expect("manifest JSON"),
    )
    .expect("manifest value")
}

fn resign(document: &mut Value) {
    let object = document.as_object_mut().expect("manifest object");
    object.remove("checksum");
    let canonical = serde_json_canonicalizer::to_vec(&*object).expect("canonical payload");
    object.insert(
        "checksum".to_string(),
        json!({
            "algorithm": "sha256",
            "digest": format!("{:x}", Sha256::digest(canonical))
        }),
    );
}

#[test]
fn missing_and_unknown_fields_fail_closed() {
    let mut missing = valid_document();
    missing
        .as_object_mut()
        .expect("object")
        .remove("capabilities");
    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&missing).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::InvalidManifest { .. })
    ));

    let mut unknown = valid_document();
    unknown
        .as_object_mut()
        .expect("object")
        .insert("assume_all_adapters".to_string(), json!(true));
    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&unknown).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::InvalidManifest { .. })
    ));
}

#[test]
fn non_regular_and_oversized_manifest_files_fail_closed() {
    let root = tempfile::tempdir().expect("temporary runtime directory");
    assert!(matches!(
        load_runtime_manifest_file(root.path(), env!("CARGO_PKG_VERSION")),
        Err(RuntimeManifestError::UnsafeManifestFile { .. })
    ));

    let oversized = root.path().join("oversized.json");
    fs::write(
        &oversized,
        vec![b' '; usize::try_from(MAX_RUNTIME_MANIFEST_BYTES + 1).expect("bounded size")],
    )
    .expect("oversized runtime manifest");
    assert!(matches!(
        load_runtime_manifest_file(oversized, env!("CARGO_PKG_VERSION")),
        Err(RuntimeManifestError::UnsafeManifestFile { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_manifest_file_fails_closed() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("temporary runtime directory");
    let target = root.path().join("target.json");
    fs::write(&target, "{}").expect("target manifest");
    let link = root.path().join("runtime-manifest.json");
    symlink(target, &link).expect("manifest symlink");

    assert!(matches!(
        load_runtime_manifest_file(link, env!("CARGO_PKG_VERSION")),
        Err(RuntimeManifestError::UnsafeManifestFile { .. })
    ));
}

#[test]
fn unknown_capabilities_and_features_fail_closed_even_with_a_valid_checksum() {
    let mut capability = valid_document();
    capability["capabilities"]
        .as_array_mut()
        .expect("capabilities")
        .push(json!("vendor.magic"));
    capability["capabilities"]
        .as_array_mut()
        .expect("capabilities")
        .sort_by_key(|value| value.as_str().map(str::to_owned));
    resign(&mut capability);
    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&capability).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::UnknownCapability { ref id }) if id == "vendor.magic"
    ));

    let mut feature = valid_document();
    feature["cargo_features"]
        .as_array_mut()
        .expect("features")
        .push(json!("aether-io/all-protocols"));
    feature["cargo_features"]
        .as_array_mut()
        .expect("features")
        .sort_by_key(|value| value.as_str().map(str::to_owned));
    resign(&mut feature);
    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&feature).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::UnknownIoFeature { .. })
    ));
}

#[test]
fn standard_composition_cannot_omit_a_live_capability() {
    let mut document = valid_document();
    document["capabilities"]
        .as_array_mut()
        .expect("capabilities")
        .remove(0);
    resign(&mut document);

    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&document).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::CapabilityCatalogMismatch { .. })
    ));
}

#[test]
fn standard_composition_cannot_omit_a_service() {
    let mut document = valid_document();
    document["services"]
        .as_array_mut()
        .expect("services")
        .remove(0);
    resign(&mut document);

    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&document).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::StandardServiceMismatch { .. })
    ));
}

#[test]
fn feature_protocol_mismatch_fails_even_when_the_document_is_resigned() {
    let mut document = valid_document();
    document["protocols"]
        .as_array_mut()
        .expect("protocols")
        .push(json!("mqtt"));
    document["protocols"]
        .as_array_mut()
        .expect("protocols")
        .sort_by_key(|value| value.as_str().map(str::to_owned));
    resign(&mut document);

    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&document).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::ProtocolFeatureMismatch { .. })
    ));
}

#[test]
fn target_os_must_match_the_artifact_target_triple() {
    let mut document = valid_document();
    document["target_os"] = json!("macos");
    resign(&mut document);

    assert!(matches!(
        parse_runtime_manifest(
            &serde_json::to_string(&document).expect("JSON"),
            env!("CARGO_PKG_VERSION")
        ),
        Err(RuntimeManifestError::TargetMismatch { .. })
    ));
}

#[test]
fn running_process_rejects_a_manifest_for_another_architecture() {
    let other_architecture = if std::env::consts::ARCH == "x86_64" {
        "aarch64"
    } else {
        "x86_64"
    };
    let target = match std::env::consts::OS {
        "macos" => format!("{other_architecture}-apple-darwin"),
        "windows" => format!("{other_architecture}-pc-windows-msvc"),
        "freebsd" => format!("{other_architecture}-unknown-freebsd"),
        _ => format!("{other_architecture}-unknown-linux-gnu"),
    };
    let root = tempfile::tempdir().expect("temporary runtime directory");
    shipped_distribution_manifest(target)
        .expect("other-architecture manifest")
        .write_to_config_directory(root.path())
        .expect("write other-architecture manifest");

    assert!(matches!(
        load_runtime_manifest_for_current_process(root.path(), env!("CARGO_PKG_VERSION")),
        Err(RuntimeManifestError::TargetArchitectureMismatch { .. })
    ));
}

#[test]
fn checked_in_default_manifest_has_generator_parity() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.template/runtime-manifest.json");
    let source = fs::read_to_string(path).expect("checked-in runtime manifest");
    let loaded = parse_runtime_manifest(&source, env!("CARGO_PKG_VERSION"))
        .expect("valid checked-in runtime manifest");
    let generated = shipped_distribution_manifest("aarch64-unknown-linux-musl")
        .expect("generated runtime manifest");

    assert_eq!(loaded, generated);
    assert_eq!(
        loaded
            .cargo_features()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        default_io_features()
            .iter()
            .map(|feature| format!("aether-io/{feature}"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn e2e_linux_manifest_has_its_target_specific_generator_parity() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.e2e/runtime-manifest.json");
    let source = fs::read_to_string(path).expect("checked-in E2E runtime manifest");
    let loaded = parse_runtime_manifest(&source, env!("CARGO_PKG_VERSION"))
        .expect("valid E2E runtime manifest");
    let generated = shipped_distribution_manifest("x86_64-unknown-linux-gnu")
        .expect("generated E2E runtime manifest");

    assert_eq!(loaded, generated);
}

#[test]
fn catalog_default_features_match_aether_io_cargo_defaults() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../services/io/Cargo.toml");
    let source = fs::read_to_string(path).expect("aether-io Cargo manifest");
    let default_line = source
        .lines()
        .find(|line| line.trim_start().starts_with("default = ["))
        .expect("single-line aether-io default feature list");
    let mut declared = default_line
        .split_once('[')
        .and_then(|(_, tail)| tail.rsplit_once(']'))
        .map(|(features, _)| features)
        .expect("default feature brackets")
        .split(',')
        .map(str::trim)
        .map(|feature| feature.trim_matches('"'))
        .filter(|feature| known_io_protocol_features().contains(feature))
        .collect::<Vec<_>>();
    declared.sort_unstable();

    assert_eq!(declared, default_io_features());
}
