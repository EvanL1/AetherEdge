# ADR-0013: Release one SDK facade instead of workspace crates

## Status

Accepted on 2026-07-14. Clauses 3 and 7 are superseded by
[ADR-0022](./0022-crates-io-sdk-registry-release.md), which adds a crates.io
release for the SDK facade and its dependency closure. Clauses 1, 2, 4, 5, and
6 remain in force: `aether-edge-sdk` is still the only supported facade, and
the signed source release still stands alongside the registry release.

## Context

AetherIot uses multiple Cargo packages to enforce dependency direction inside
the repository. Those packages are not independent products: useful edge
applications compose the domain, ports, application policy, and concrete
adapters behind the SDK. Publishing every package to crates.io would expose
implementation boundaries as public acquisition choices and create separate
SemVer promises for units that are not intended to be consumed alone.

Cargo registries cannot keep a published package's transitive workspace
dependencies private. Therefore a registry release of the existing facade
would still require publishing the implementation packages. A signed Git
source release can preserve the internal Cargo boundaries while giving
downstream builds one exact, reviewable source identity.

## Decision

1. `aether-edge-sdk`, imported as `aether_sdk`, is the only supported Rust
   application facade.
2. The `local-runtime` feature exposes zero-external-service adapters through
   `aether_sdk::local`; downstream applications do not depend directly on
   `aether-store-local`.
3. Workspace domain, port, application, testkit, data-plane, and extension
   packages set `publish = false`. Release automation holds no registry token
   and runs no `cargo publish` command.
4. AetherIot v0.5 publishes a versioned source archive, runtime installers,
   runtime manifests, and CLI archives. Every asset has an adjacent SHA-256,
   and the source archive and executable artifacts share one GitHub/Sigstore
   provenance bundle.
5. A downstream release pins the exact Git commit behind the release tag. Its
   distribution authority records and verifies the tag, commit, source
   archive, runtime/CLI assets, checksums, and provenance.
6. Any crate versions accidentally uploaded before this decision are yanked
   and are not supported AetherIot releases.
7. A future crates.io release requires a new ADR and a genuinely standalone
   public package. The workspace layout alone is not a registry product map.

## Consequences

- Users see one SDK dependency and one compatibility contract.
- Internal package refactors do not imply independently supported products.
- AetherEMS can prove the exact upstream source and binary release without
  copying Kernel source or relying on an arbitrary bootstrap commit.
- Cargo must fetch the signed release commit from GitHub until a standalone
  registry package exists.
- Yanking an accidental crate version prevents new resolution but cannot erase
  the immutable registry record or break builds that already locked it.
