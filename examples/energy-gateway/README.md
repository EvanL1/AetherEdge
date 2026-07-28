# AetherEMS energy gateway compatibility composition

This example proves that an energy Pack can layer models, mappings, rules,
evaluations, and knowledge over the industry-neutral Aether SDK without
commissioning a site.

It builds the same local-only application API as `minimal-gateway`, validates
the bundled Pack and disabled examples, and fails if a device channel,
instance auto-loading, or control rule is enabled.

```bash
cargo run -p aether-example-energy-gateway
cargo test -p aether-example-energy-gateway --test composition_contract
cargo test -p aether-example-energy-gateway --test pack_artifact_contract
```

Forecasting and other derived-data applications are downstream AetherEMS
capabilities, not part of this kernel composition proof.
