# ADR-0007: Separate the Aether kernel from the AetherEMS distribution

## Status

Accepted. Repository-local crate, artifact release, and extraction gates were
implemented on 2026-07-13. The physical AetherEMS repository split, downstream
bootstrap CI, and EMS Console ownership are complete. ADR-0013 defines the
single-facade source release needed to replace the bootstrap pin; the first
independent signed publication and replacement of that pin remain incomplete.

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
8. AetherEMS consumes the exact commit of a signed AetherIot source release
   through the single `aether-edge-sdk` facade. It is not a fork and does not
   use a Git submodule for the kernel. Internal workspace crates are not
   independently published products.

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
   The empty `get_builtin_*`, `get_product_names`, `get_child`,
   `product_exists`, and `builtin_only` compatibility entry points have been
   removed from `aether-model`; in-workspace callers now construct an explicit
   `ProductLibrary` instead of entering a hidden built-in catalog path.
5. **Complete:** every concrete runtime composition carries a closed v1
   `runtime-manifest.json` with its Aether version, target triple, services,
   exact protocol-affecting Cargo features, derived adapters, live application
   capability catalog, and canonical SHA-256 checksum. Automation and MCP load
   it from the shared configuration directory before validating active Packs;
   missing, unknown, tampered, release-mismatched, target-mismatched, or
   feature-inconsistent metadata fails closed. The installer generator and
   `aether-io` build consume the same feature source, including trimmed builds.
6. **Local artifact and crate implementation complete; external publication
   blocked:** the Kernel CLI builds and installs a closed Pack-only artifact
   containing no Kernel executable or core crate. Its metadata declares the
   exact Kernel version, target triple, runtime-manifest digest, and per-file
   checksums.
   Installation re-verifies the Pack against the installed runtime, publishes
   it below the site's `packs/<id>/<version>` directory, and atomically updates
   `global.yaml`; failure preserves the previous active set and rolls back a
   newly published directory. A single dependency-ordered catalog covers all
   15 publishable Rust crates. The release gate packages each exact archive,
   compiles those archives together in an isolated consumer, rejects a public
   dependency on a private or later-published workspace package, and applies
   SemVer checks whenever crates.io provides a prior baseline. The tag workflow
   publishes the catalog in order and attests the resulting `.crate` archives.
   A resumed partial publication skips an existing version only after its
   downloaded archive digest exactly matches the locally rebuilt archive.
   The independent AetherEMS repository now exists, owns the EMS Console, and
   runs downstream bootstrap CI against a pinned Kernel commit. What is not yet
   complete is release evidence: there is no independently published, signed
   Aether artifact, public crate set, or signed Energy Pack artifact, and the
   downstream pin does not yet consume those artifacts. Those external facts
   must not be inferred from the repository split alone.
7. **Local release and extraction gates complete; external evidence pending:**
   the tag workflow is configured to publish the six-process Kernel runtime,
   standalone CLI, and target-bound Energy Pack as separate artifacts. Each
   release payload has a SHA-256 sidecar. The workflow uses GitHub's official
   `actions/attest@v4` mechanism with `id-token: write` and
   `attestations: write` plus `artifact-metadata: write` to create signed build
   provenance for the Kernel, CLI, public crate, and Pack archives. This is a
   locally reviewable workflow property; it is not evidence that a tag workflow
   has run or that an external artifact exists. The target-bound runtime and
   Pack build share the single
   AetherEMS composition feature authority at
   `distributions/aetherems/runtime-io-features.txt`; the Pack build therefore
   cannot silently target a runtime that omits one of its required protocol
   adapters.
8. **Local extraction-readiness proof complete; external extraction blocked:**
   `scripts/check-extraction-readiness.sh --local-only` deterministically
   checks the neutral Kernel boundary, safe no-external-database defaults,
   unignored runtime-manifest binary source, generated runtime metadata, both
   composition examples, and an isolated Pack-only artifact. The default full
   mode additionally requires explicit released-version, Kernel/Pack digest,
   and successful downstream-repository CI evidence. It fails closed when any
   input is absent, malformed, repository-inconsistent, or unsuccessful. The
   checker validates supplied evidence identifiers; it does not create or
   query external releases, repositories, attestations, or CI runs.

The short pointers under `docs/domain/` and
`libs/aether-model/src/products/README.md` may be removed after supported
hosted/offline routes and downstream links use Pack-owned locations. This
documentation-pointer condition is not satisfied by the local release workflow
alone; downstream link evidence must come from the extracted distribution
repository. The model compatibility entry points themselves are already gone,
but downstream compilation remains part of the external CI extraction gate.

## Extraction criteria

The temporary bootstrap split becomes a stable independent release boundary
only after all of the following are true:

1. The pack manifest and loader have a versioned, tested contract.
2. Energy models no longer resolve through `legacy_assets` paths.
3. Core manifests and source contain no energy product constants or default
   site configuration.
4. Aether crates and runtime artifacts are released with compatible version
   metadata, SHA-256 digests, and verifiable build provenance. The workflow is
   locally gated; an actual released version and artifact digests are still
   required external evidence.
5. AetherEMS CI in the extracted downstream repository consumes those released
   artifacts and passes its pack, configuration, safety, and composition
   conformance suites. A repository-qualified successful CI run and commit are
   still required external evidence.
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
./scripts/test-release-integrity.sh
./scripts/test-public-crate-release.sh
./scripts/check-public-crate-release.sh
./scripts/publish-public-crates.sh
./scripts/test-extraction-readiness.sh
./scripts/check-extraction-readiness.sh --local-only
./scripts/check-architecture.sh
```

The full extraction gate intentionally fails until real external evidence is
provided:

```bash
./scripts/check-extraction-readiness.sh \
  --released-version 0.5.0 \
  --kernel-artifact-sha256 <64-hex-sha256> \
  --energy-pack-artifact-sha256 <different-64-hex-sha256> \
  --downstream-repository <owner/aether-ems> \
  --downstream-ci-run-url <https://github.com/owner/aether-ems/actions/runs/id> \
  --downstream-ci-commit <full-git-commit> \
  --downstream-ci-conclusion success
```
