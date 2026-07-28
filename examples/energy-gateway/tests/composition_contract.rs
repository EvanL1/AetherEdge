use aether_example_energy_gateway::EnergyGateway;

#[test]
fn bundled_energy_pack_layers_over_the_generic_gateway() {
    let gateway = EnergyGateway::bundled().expect("bundled energy pack must be valid");
    let summary = gateway.pack_summary();

    assert_eq!(summary.id, "energy");
    assert_eq!(summary.name, "Aether Energy");
    assert!(summary.capabilities.iter().any(|model| model == "Battery"));
    assert!(summary.example_channel_count > 0);
    let _ = gateway.application();
}

#[test]
fn bundled_energy_composition_starts_uncommissioned_and_fail_safe() {
    let gateway = EnergyGateway::bundled().expect("bundled energy pack must be valid");
    let summary = gateway.pack_summary();

    assert_eq!(summary.enabled_channel_count, 0);
    assert_eq!(summary.enabled_rule_count, 0);
    assert!(!summary.auto_load_instances);
}
