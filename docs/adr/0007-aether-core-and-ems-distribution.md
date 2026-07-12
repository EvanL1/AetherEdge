# ADR-0007: Separate the Aether kernel from the AetherEMS distribution

## Status

Accepted for staged implementation on 2026-07-11.

## Context

The repository began as AetherEMS, an energy-management product. It is now
also the integration workspace for Aether, an AI-native and industry-neutral
IoT edge kernel. The kernel must remain useful to building automation,
manufacturing, agriculture, transport, and other device domains without
compiling or commissioning energy-specific models.

Deleting or rewriting the existing Git history would not improve the runtime
boundary. It would instead invalidate commit links, tags, forks, and local
clones. Splitting repositories before the remaining release gates would create
a different problem. Production model and MCP knowledge loading are now
runtime-only and Pack-owned. Mappings, rules, and evaluations are also indexed
Pack artifacts, and the local Kernel CLI now installs a checksummed Pack-only
bundle. The remaining gates are independently published/signed Kernel and Pack
artifacts plus downstream CI consuming them. Two repositories would otherwise
still need synchronized source edits until those external gates are closed.

## Decision

1. Aether is the product and dependency identity of the industry-neutral edge
   kernel, SDK, six-process runtime, CLI/MCP interface, protocol adapters, and
   extension ports.
2. AetherEMS is an official energy distribution composed on a released Aether
   version. It owns energy models, mappings, rules, operational knowledge,
   commissioning policy, and any optional energy-specific client.
3. A domain pack is declarative data. It cannot add a Rust dependency to a
   core crate or become a default runtime dependency.
4. During migration, this repository remains the integration workspace for
   both deliverables. `examples/minimal-gateway` proves the Aether composition;
   `examples/energy-gateway` proves the fail-safe AetherEMS overlay.
5. Both examples must run without Redis, PostgreSQL, a broker, a field device,
   or a browser. The energy example may inspect configured devices and rules,
   but every bundled channel and control rule remains disabled until explicit
   site commissioning.
6. The existing Git history is retained. No force-push or history rewrite is
   part of the product split.
7. When the extraction criteria below are satisfied, create a new `Aether`
   repository from an identified integration-workspace commit. The initial
   public-kernel commit records that source SHA and links back to the retained
   history. This repository then becomes the thin `AetherEMS` distribution.
8. AetherEMS consumes versioned Aether artifacts or crates. It is not a fork
   and does not use a Git submodule for the kernel.

## Repository ownership after extraction

| Aether | AetherEMS |
|---|---|
| `crates/aether-*` kernel and SDK | `packs/energy` |
| six generic runtime services | energy product and instance definitions |
| CLI, MCP, capability policy, audit API | energy mappings and control rules |
| protocol and storage extension interfaces | EMS commissioning profiles |
| official generic adapters | energy-domain knowledge and evaluations |
| minimal and protocol examples | optional EMS deployment overlay/client |

Redis and PostgreSQL adapters may remain official Aether extensions. Their
presence in the source tree does not place them in the default dependency or
runtime graph.

## Version contract

Every extracted domain distribution declares:

- its pack schema version;
- its own release version;
- a compatible Aether release range;
- required capability and protocol identifiers;
- whether included examples are commissioned.

Pack validation fails closed on an unsupported schema, an incompatible Aether
version, an unknown required capability, or an unexpectedly enabled example.

Pack v1 is implemented by `crates/aether-pack` and exposed through
`aether_sdk::pack`. Its asset directories are relative to the pack root;
absolute paths, `..`, missing directories, and symlink escapes fail with typed
errors. Unknown manifest fields are rejected. Loading is read-only and does not
install or commission the pack.

## Migration gate status and removal criteria

`legacy_assets` is not part of Pack v1 and is rejected as an unknown field.
The staged gates now stand as follows:

1. **Complete:** the 13 Energy models and five knowledge pages are Pack-owned
   assets declared by `packs/energy/pack.yaml`; old JSON copies are absent.
2. **Complete:** `aether-model` embeds no domain products. Automation reads the
   shared `<AETHER_CONFIG_PATH>/global.yaml` `packs: [{ id, root }]` entry,
   validates every selected manifest through `aether-pack`, and then loads its
   model assets. `packs: []` yields zero products; an explicit site product
   directory remains a later, deliberate override layer.
3. **Complete:** the CLI binary embeds only generic kernel documentation. MCP
   discovers knowledge from the same validated active Pack set at startup and
   publishes identity-bound URIs such as
   `aether://packs/energy/knowledge/ess-primer`. An inactive Pack contributes no
   URI. Product and knowledge candidates reject symlinks, non-regular files,
   path escapes, invalid names, and oversized content.
4. **Complete:** Energy mappings, control rules, evaluations, and Data
   Processing task declarations live in formal Pack directories. `pack.yaml`,
   each closed v1 `index.yaml`, and actual files must match exactly. Loading
   rejects unknown fields/files, duplicates, symlinks, path escapes,
   schema/media mismatches, and oversized content. Isolated-copy conformance
   covers models, knowledge, mappings, rules, evaluations, and task indexes;
   `packs: []` exports none of their namespaced identities.
   Energy product aliases and legacy property conversion metadata are Pack-
   owned compatibility assets with kernel-removal version `0.5.0`; the generic
   CLI/schema contains no Energy product-name rewrite and fails closed before
   discarding unresolved domain properties.
5. **Complete:** every concrete runtime composition carries a closed v1
   `runtime-manifest.json` with its Aether version, target triple, services,
   exact protocol-affecting Cargo features, derived adapters, live application
   capability catalog, and canonical SHA-256 checksum. Automation and MCP load
   it from the shared configuration directory before validating active Packs;
   missing, unknown, tampered, release-mismatched, target-mismatched, or
   feature-inconsistent metadata fails closed. The installer generator and
   `aether-io` build consume the same feature source, including trimmed builds.
6. **Local implementation complete; external release gate blocked:** the
   Kernel CLI builds and installs a closed Pack-only artifact containing no
   Kernel executable or core crate. Its metadata declares the exact Kernel
   version, target triple, runtime-manifest digest, and per-file checksums.
   Installation re-verifies the Pack against the installed runtime, publishes
   it below the site's `packs/<id>/<version>` directory, and atomically updates
   `global.yaml`; failure preserves the previous active set and rolls back a
   newly published directory. What is not yet complete is release evidence:
   there is no independently published, signed Aether artifact, signed Energy
   Pack artifact, second repository, or downstream CI consuming both. Those
   external facts must not be inferred from this workspace implementation.

The short pointers under `docs/domain/` and
`libs/aether-model/src/products/README.md` may be removed after supported
hosted/offline routes and downstream links use Pack-owned locations. Empty
legacy `get_builtin_*` APIs may be removed once downstream callers compile
against `ProductLibrary` exclusively.

## Extraction criteria

The repositories split only after all of the following are true:

1. The pack manifest and loader have a versioned, tested contract.
2. Energy models no longer resolve through `legacy_assets` paths.
3. Core manifests and source contain no energy product constants or default
   site configuration.
4. Aether crates and runtime artifacts are released with compatible version
   metadata.
5. AetherEMS CI consumes those released artifacts and passes its pack,
   configuration, safety, and composition conformance suites.
6. The complete Aether runtime can install and start with an empty,
   industry-neutral site.
7. The AetherEMS distribution can install without modifying kernel source.

## Consequences

### Positive

- Library users see a small, neutral Aether surface instead of an EMS product
  that happens to contain reusable crates.
- EMS remains a first-class maintained product and a realistic conformance
  scenario for the kernel.
- The migration can proceed without force-pushing history or maintaining two
  unstable copies of the same runtime.
- Other official or third-party industry packs follow the same dependency and
  safety contract.

### Negative

- The integration workspace temporarily contains both identities.
- README and release automation must distinguish kernel, runtime, and energy
  distribution commands precisely.
- Extraction is deferred until the pack boundary is real rather than merely a
  directory convention.

## Verification

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p aether-example-energy-gateway
cargo test -p aether-example-minimal-gateway --test composition_contract
cargo test -p aether-example-energy-gateway --test composition_contract
cargo test -p aether-example-energy-gateway --test pack_artifact_contract
cargo test -p aether-pack --test asset_index_contract
./scripts/check-energy-pack-boundary.sh
./scripts/check-safe-default-config.sh
./scripts/check-architecture.sh
```
