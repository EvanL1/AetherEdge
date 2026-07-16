# AetherContracts

AetherContracts is the public, language-neutral interoperability authority for
AetherEdge, AetherCloud, and independent implementations.

Specifications define behavior, JSON Schema Draft 2020-12 defines structural
acceptance, fixtures pin observable examples, and the black-box TCK supplies
executable evidence. A product-local copy or language binding never becomes a
second source of truth.

## Current release

`v0.1.0-alpha.3` freezes an experimental CloudLink wire/profile/TCK surface and
provides TypeScript, Rust, C, and C++ fixture bindings. It is not a production
CloudLink cutover release.

The release preserves the historical AetherIot consumer name in signed and
digest-pinned artifacts. Those bytes remain immutable after the repository is
renamed to AetherEdge. Later releases may update product display names without
changing protocol identifiers.

Read the [AetherContracts repository](https://github.com/EvanL1/AetherContracts),
the [compatibility matrix](../compatibility/version-matrix.md), and the
[Edge to Contracts to Cloud tutorial](../tutorials/edge-contracts-cloud.md).
