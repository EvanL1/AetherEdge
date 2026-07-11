# Energy pack

The energy pack is the domain layer of the AetherEMS distribution. It is the
migration destination for energy product models, mappings, control strategies,
and operational knowledge; it is not a dependency of the Aether kernel.

The manifest currently points at their legacy locations so this architectural
change does not duplicate or silently fork production definitions. The
pack-owned configuration examples under `examples/config/` are deliberately
disabled: installing or inspecting this pack must not contact a device or run a
control rule. Commissioning must supply site-specific addresses and explicitly
enable each selected instance, channel, and rule.

Run the fail-safe distribution proof from the repository root:

```bash
cargo run -p aether-example-energy-gateway
cargo test -p aether-example-energy-gateway --test composition_contract
```

This validates and reports the bundled energy capabilities but does not start
the six production services, contact a field device, or enable a control rule.
The future standalone AetherEMS repository will consume a released Aether
version according to [ADR-0007](../../docs/adr/0007-aether-core-and-ems-distribution.md).
