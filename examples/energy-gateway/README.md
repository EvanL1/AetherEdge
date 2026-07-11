# AetherEMS energy gateway composition

This example proves that the AetherEMS distribution layers energy knowledge
over the industry-neutral Aether SDK without commissioning a site.

It builds the same local-only application API as `minimal-gateway`, parses the
bundled energy pack manifest and safe example configuration, and fails if a
device channel, instance auto-loading, or control rule is enabled.

```bash
cargo run -p aether-example-energy-gateway
cargo test -p aether-example-energy-gateway --test composition_contract
```

This is a composition and conformance proof, not the six-process production
deployment. It does not connect to hardware, start a broker, or require Redis
or PostgreSQL.
