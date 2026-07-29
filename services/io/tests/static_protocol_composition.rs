use std::collections::BTreeSet;

use aether_io::core::channels::compiled_protocol_registry;

#[test]
fn compiled_registry_is_the_static_protocol_composition_authority() {
    let protocols = compiled_protocol_registry()
        .expect("compiled protocol registry")
        .protocol_ids()
        .into_iter()
        .collect::<BTreeSet<_>>();

    assert_eq!(protocols.contains("modbus_tcp"), cfg!(feature = "modbus"));
    assert_eq!(protocols.contains("modbus_rtu"), cfg!(feature = "modbus"));
    assert_eq!(protocols.contains("mqtt"), cfg!(feature = "mqtt"));
    assert_eq!(protocols.contains("http"), cfg!(feature = "http"));
    assert_eq!(
        protocols.contains("aether_485"),
        cfg!(feature = "aether_485")
    );
    assert_eq!(protocols.contains("iec61850"), cfg!(feature = "iec61850"));
    assert_eq!(
        protocols.contains("gpio"),
        cfg!(all(target_os = "linux", feature = "gpio"))
    );
    assert_eq!(
        protocols.contains("can"),
        cfg!(all(target_os = "linux", feature = "can"))
    );

    for unavailable in [
        "ble",
        "dl645",
        "iec104",
        "j1939",
        "matter",
        "opcua",
        "sunspec_tcp",
        "virtual",
        "zigbee",
    ] {
        assert!(!protocols.contains(unavailable));
    }
}
