use std::collections::BTreeSet;

use aether_io::protocols::get_protocol_registry;
use aether_runtime_catalog::KernelRuntimeManifest;

fn compiled_io_protocol_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    for (enabled, feature) in [
        (cfg!(feature = "modbus"), "modbus"),
        (cfg!(feature = "iec104"), "iec104"),
        (cfg!(feature = "opcua"), "opcua"),
        (cfg!(feature = "can"), "can"),
        (cfg!(feature = "j1939"), "j1939"),
        (cfg!(feature = "gpio"), "gpio"),
        (cfg!(feature = "dl645"), "dl645"),
        (cfg!(feature = "aether_485"), "aether_485"),
        (cfg!(feature = "mqtt"), "mqtt"),
        (cfg!(feature = "http"), "http"),
        (cfg!(feature = "ble"), "ble"),
        (cfg!(feature = "zigbee"), "zigbee"),
        (cfg!(feature = "iec61850"), "iec61850"),
    ] {
        if enabled {
            features.push(feature);
        }
    }
    features
}

fn native_target_triple() -> &'static str {
    if cfg!(target_os = "linux") {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else if cfg!(target_os = "windows") {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported-test-target"
    }
}

#[test]
fn manifest_protocols_match_the_io_binary_feature_set() {
    let manifest = KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        native_target_triple(),
        compiled_io_protocol_features(),
    )
    .expect("compiled IO feature set must be understood");
    let protocols = manifest
        .protocols()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    assert_eq!(protocols.contains("mqtt"), cfg!(feature = "mqtt"));
    assert_eq!(protocols.contains("http"), cfg!(feature = "http"));
    assert_eq!(protocols.contains("iec104"), cfg!(feature = "iec104"));
    assert!(!protocols.contains("sunspec_tcp"));
    assert!(!protocols.contains("sunspec_rtu"));
    assert_eq!(protocols.contains("opcua"), cfg!(feature = "opcua"));
    assert_eq!(protocols.contains("dl645"), cfg!(feature = "dl645"));
    assert_eq!(
        protocols.contains("aether_485"),
        cfg!(feature = "aether_485")
    );
    assert_eq!(protocols.contains("iec61850"), cfg!(feature = "iec61850"));
    assert!(!protocols.contains("aether.cloudlink.integration.v1alpha1"));
    assert!(!protocols.contains("aether.cloudlink.integration-control.v1alpha1"));
    assert_eq!(
        protocols.contains("di_do"),
        cfg!(all(target_os = "linux", feature = "gpio"))
    );
    assert_eq!(
        protocols.contains("can"),
        cfg!(all(target_os = "linux", feature = "can"))
    );
    assert!(!protocols.contains("matter"));
    assert!(!protocols.contains("virtual"));
}

#[test]
fn manifest_protocols_are_exactly_the_discoverable_production_protocols() {
    let manifest = KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        native_target_triple(),
        compiled_io_protocol_features(),
    )
    .expect("compiled IO feature set must be understood");
    let advertised = manifest
        .protocols()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let registry = get_protocol_registry();
    let discoverable = registry
        .protocols()
        .map(|protocol| protocol.protocol_type)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        discoverable.len(),
        registry.protocols().len(),
        "protocol discovery contains duplicate canonical protocol types"
    );
    assert_eq!(
        discoverable, advertised,
        "protocol discovery and the signed runtime manifest must describe the same production adapters"
    );
}

#[test]
fn registry_resolves_runtime_aliases_to_one_canonical_factory() {
    let registry = get_protocol_registry();

    #[cfg(feature = "modbus")]
    {
        assert_eq!(
            registry
                .resolve("modbus")
                .expect("Modbus alias must resolve")
                .protocol_type,
            "modbus_tcp"
        );
        assert_eq!(
            registry
                .resolve("MODBUS-RTU")
                .expect("normalized Modbus RTU alias must resolve")
                .protocol_type,
            "modbus_rtu"
        );
    }

    #[cfg(all(target_os = "linux", feature = "gpio"))]
    assert_eq!(
        registry
            .resolve("gpio")
            .expect("GPIO alias must resolve")
            .protocol_type,
        "di_do"
    );

    assert!(
        registry.resolve("unregistered-protocol").is_none(),
        "unknown protocols must fail closed"
    );
}
