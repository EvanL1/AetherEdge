use std::collections::BTreeSet;
use std::fs;

use aether_runtime_catalog::{
    KernelRuntimeManifest, RUNTIME_MANIFEST_FILE_NAME, RuntimeManifestError, default_io_features,
    load_runtime_manifest,
};

fn linux_manifest(features: &[&str]) -> KernelRuntimeManifest {
    KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        features.iter().copied(),
    )
    .expect("valid Linux runtime manifest")
}

#[test]
fn default_manifest_is_feature_exact_and_uses_the_live_capability_catalog() {
    let manifest = linux_manifest(default_io_features());
    let expected_capabilities = aether_application::capability_catalog()
        .iter()
        .map(|descriptor| descriptor.name())
        .collect::<BTreeSet<_>>();
    let expected_protocols = BTreeSet::from([
        "aether_485",
        "can",
        "di_do",
        "iec61850",
        "modbus_rtu",
        "modbus_tcp",
        "sunspec_rtu",
        "sunspec_tcp",
        "virtual",
    ]);

    assert_eq!(
        manifest
            .capabilities()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_capabilities
    );
    assert_eq!(
        manifest
            .protocols()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_protocols
    );
    assert!(!manifest.protocols().any(|protocol| protocol == "mqtt"));
    assert!(!manifest.protocols().any(|protocol| protocol == "http"));
}

#[test]
fn trimmed_manifest_does_not_advertise_uncompiled_protocols() {
    let manifest = linux_manifest(&["modbus"]);

    assert_eq!(
        manifest.protocols().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "modbus_rtu",
            "modbus_tcp",
            "sunspec_rtu",
            "sunspec_tcp",
            "virtual",
        ]
    );
    assert_eq!(
        manifest
            .cargo_features()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["aether-io/modbus"]
    );
}

#[test]
fn integration_cloudlink_protocol_requires_its_explicit_io_feature() {
    let home_assistant_only = linux_manifest(&["home-assistant"]);
    assert!(
        !home_assistant_only
            .protocols()
            .any(|protocol| protocol == "aether.cloudlink.integration.v1alpha1")
    );

    let enabled = linux_manifest(&["home-assistant-cloudlink"]);
    assert!(
        enabled
            .protocols()
            .any(|protocol| protocol == "aether.cloudlink.integration.v1alpha1")
    );
    assert!(
        enabled
            .cargo_features()
            .any(|feature| feature == "aether-io/home-assistant-cloudlink")
    );
}

#[test]
fn integration_control_protocol_requires_its_explicit_default_off_io_feature() {
    let read_only = linux_manifest(&["home-assistant-cloudlink"]);
    assert!(
        !read_only
            .protocols()
            .any(|protocol| protocol == "aether.cloudlink.integration-control.v1alpha1")
    );

    let enabled = linux_manifest(&["home-assistant-integration-control"]);
    assert!(
        enabled
            .protocols()
            .any(|protocol| protocol == "aether.cloudlink.integration-control.v1alpha1")
    );
    assert!(
        enabled
            .protocols()
            .any(|protocol| protocol == "aether.cloudlink.integration.v1alpha1")
    );
    assert!(
        enabled
            .cargo_features()
            .any(|feature| feature == "aether-io/home-assistant-integration-control")
    );
}

#[test]
fn manifest_round_trip_validates_schema_version_release_and_checksum() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let manifest = linux_manifest(&["modbus", "mqtt"]);
    manifest
        .write_to_config_directory(config.path())
        .expect("write manifest");

    let loaded = load_runtime_manifest(config.path(), env!("CARGO_PKG_VERSION"))
        .expect("validated manifest");

    assert_eq!(loaded, manifest);
    assert_eq!(loaded.schema_version(), 1);
    assert_eq!(loaded.aether_version(), env!("CARGO_PKG_VERSION"));
    assert_eq!(loaded.checksum().algorithm(), "sha256");
    assert_eq!(loaded.checksum().digest().len(), 64);
    assert!(loaded.pack_runtime().is_ok());
}

#[test]
fn missing_tampered_or_wrong_release_manifest_fails_closed() {
    let config = tempfile::tempdir().expect("temporary config directory");
    assert!(matches!(
        load_runtime_manifest(config.path(), env!("CARGO_PKG_VERSION")),
        Err(RuntimeManifestError::Read { .. })
    ));

    let manifest = linux_manifest(&["modbus"]);
    manifest
        .write_to_config_directory(config.path())
        .expect("write manifest");
    assert!(matches!(
        load_runtime_manifest(config.path(), "0.6.0"),
        Err(RuntimeManifestError::AetherVersionMismatch { .. })
    ));

    let path = config.path().join(RUNTIME_MANIFEST_FILE_NAME);
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read manifest")).expect("manifest JSON");
    document["protocols"]
        .as_array_mut()
        .expect("protocol array")
        .push(serde_json::json!("mqtt"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&document).expect("tampered JSON"),
    )
    .expect("write tampered manifest");

    assert!(matches!(
        load_runtime_manifest(config.path(), env!("CARGO_PKG_VERSION")),
        Err(RuntimeManifestError::ChecksumMismatch { .. })
    ));
}

#[test]
fn unknown_io_feature_is_rejected_instead_of_being_silently_ignored() {
    let error = KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["modbus", "not-a-real-adapter"],
    )
    .expect_err("unknown feature must fail closed");

    assert!(matches!(
        error,
        RuntimeManifestError::UnknownIoFeature { .. }
    ));
}
