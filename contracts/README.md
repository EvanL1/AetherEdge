# Machine-readable contracts

This directory is reserved for stable configuration, command, event, and MCP
schemas. Generated files must carry a generated header and are updated through
the repository generation task rather than edited manually.

During migration, Rust application types are the source of truth. Schema drift
will be checked before legacy HTTP payloads are removed.

- [`pack/pack-manifest.v1.schema.json`](pack/pack-manifest.v1.schema.json) defines
  the fail-closed, industry-neutral domain-pack manifest accepted by
  `aether-pack`.
- [`pack/pack-artifact.v1.schema.json`](pack/pack-artifact.v1.schema.json)
  defines the closed metadata, exact Kernel/runtime binding, and checksummed
  file inventory for a data-only Pack bundle.
- [`runtime/runtime-manifest.v1.schema.json`](runtime/runtime-manifest.v1.schema.json)
  defines the feature-exact, checksummed composition metadata consumed by
  Automation, MCP, and Pack installation tooling before Pack activation.
- [`pack/pack-asset-index.v1.schema.json`](pack/pack-asset-index.v1.schema.json)
  defines the exact inventory for Pack-owned mappings, rules, evaluations, and
  Data Processing tasks.
- The mapping, rule, evaluation, and Data Processing task schemas in
  [`pack/`](pack/) version the corresponding asset payloads. `aether-pack`
  verifies index identity, confinement, size, media type, and exact file
  inventory before activation. Each category is bound to its corresponding v1
  payload schema; distribution conformance validates each schema-specific
  identity and fail-safe invariant.
