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
clones. Splitting repositories immediately would create a different problem:
the pack loader and several compatibility service paths still point at legacy
energy assets, so two repositories would need synchronized edits while their
contract is still changing.

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
./scripts/check-architecture.sh
```
