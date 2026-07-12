# Energy pack

The energy pack is the domain layer of the AetherEMS distribution. It is the
migration destination for energy product models, mappings, control strategies,
and operational knowledge; it is not a dependency of the Aether kernel.

The v1 manifest is validated by the industry-neutral `aether-pack` boundary and
contains only pack-root-relative asset directories. Product models under
`models/`, operational knowledge under `knowledge/`, and the indexed
`mappings/`, `rules/`, `evaluations/`, and `data-processing/tasks/`
directories are Pack-owned assets.
Each formal directory has a closed v1 index whose IDs exactly match
`pack.yaml` and its actual regular files.
The pack-owned configuration examples under `examples/config/` are deliberately
disabled: installing or inspecting this pack must not contact a device or run a
control rule. Commissioning must supply site-specific addresses and explicitly
enable each selected instance, channel, and rule.

The legacy energy-dashboard calculated-point preset is retained at
[`examples/config/api/calculated_points.sql`](examples/config/api/calculated_points.sql)
only as an AetherEMS distribution migration and commissioning asset. The Kernel
does not compile or auto-import it; Pack installation and activation do not
execute it, and the generic homepage reset always returns to zero points.

Runtime activation is a single shared `global.yaml` entry consumed by both
automation and MCP:

```yaml
packs:
  - id: energy
    root: /opt/aether/packs/energy
```

The adjacent checksummed `runtime-manifest.json` must also satisfy every
protocol in `pack.yaml`. The generic Kernel default intentionally does not
compile or advertise MQTT/HTTP; an AetherEMS composition enables its explicit
`can,gpio,http,modbus,mqtt` IO feature set and generates matching metadata.
Adding the Pack entry to an incompatible trimmed Kernel therefore fails closed
instead of pretending an adapter exists.

Without that validated identity/root pair, Energy models and
`aether://packs/energy/knowledge/*` resources are absent, and the formal asset
catalog exposes no `energy/<category>/<id>` entries.

Energy product-name aliases and the pre-v5 instance-property conversion now
live in `mappings/` with `removed_from_kernel: 0.5.0`. The generic CLI/schema
does not know Energy product names and refuses to discard non-empty legacy
domain properties; apply the Pack-owned migration before upgrading such a
database.

Run the fail-safe distribution proof from the repository root:

```bash
cargo run -p aether-example-energy-gateway
cargo test -p aether-example-energy-gateway --test pack_artifact_contract
cargo test -p aether-example-energy-gateway --test data_processing_composition
```

This validates and reports the bundled energy capabilities but does not start
the six production services, contact a field device, or enable a control rule.
For a target-specific local artifact, first generate the manifest from the
same IO feature selection used to build that Kernel, then build the data-only
bundle:

```bash
./scripts/build-installer.sh v0.5.0 arm64 \
  --io-features=can,gpio,http,modbus,mqtt \
  --manifest-only=build/energy-runtime/runtime-manifest.json
./scripts/build-pack-artifact.sh \
  packs/energy \
  build/energy-runtime/runtime-manifest.json \
  release/aether-energy-arm64-0.5.0.bundle
```

This local command is not evidence of a published or signed release. The
future standalone AetherEMS repository must consume an independently released
Aether Kernel artifact and publish its own Pack artifact and downstream CI
evidence according to
[ADR-0007](../../docs/adr/0007-aether-core-and-ems-distribution.md).

Load and PV forecasting are the first Aether Data Processing tasks in this
pack. Their complete disabled-by-default declarations, synthetic binding, and
contract fixtures live under [data-processing](data-processing/README.md).
Their semantic inputs, request-driven processor boundary, and migration from
the existing service are documented in
[Power Forecasting](knowledge/power-forecasting.md). Installing this
pack never starts a model or contacts a remote processor by itself. The
opt-in implementations are the bounded
[HTTP adapter](../../extensions/http-data-processor/README.md) and
[Load-Forecasting processor](../../integrations/load-forecasting/README.md).
