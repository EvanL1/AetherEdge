# ADR-0019: Establish AetherIoT as the product family and rename the edge product to AetherEdge

Status: Accepted on 2026-07-16.

## Context

The name AetherIot simultaneously identified the umbrella project and the edge
runtime repository. The product already used AetherEdge in installer names, the
Rust SDK package, and public architecture diagrams. AetherCloud and
AetherContracts had distinct product identities, making the remaining overlap
ambiguous for users, documentation, and release automation.

The public GitHub account names `AetherIoT` and `aether-iot` are unavailable.
The repositories can remain under `EvanL1` while a future organization uses a
different address and the display name AetherIoT.

## Decision

AetherIoT is the umbrella project and public platform identity.

- AetherEdge is the edge runtime, Kernel, CLI, and SDK product and repository.
- AetherCloud is the cloud fusion and governed control plane.
- AetherContracts is the public interoperability authority.
- AetherEMS is an industry solution built on the platform, not a fourth core
  platform product.

The GitHub repository `EvanL1/AetherIot` is renamed to `EvanL1/AetherEdge` only
after documentation, compatibility, CI, release, and rollback material is ready.

Existing `aether-*` crates and binaries, the `aether` CLI, `aether-edge-sdk`,
configuration identifiers, installer names, and protocol identifiers remain
stable. Published artifacts and digest-pinned contract releases are immutable
and may retain the historical product name.

The documentation information architecture is:

```text
Overview / AetherEdge / AetherCloud / AetherContracts /
Tutorials / Compatibility / Roadmap
```

## Consequences

The platform relationship becomes explicit and the edge repository matches its
existing installer and SDK identity. New users get one documentation entry
point and a version compatibility matrix.

Repository URLs, badges, source archive names, attestation examples, Git
dependencies, and CI allowlists require a coordinated update. Historical ADRs,
release assets, evidence, and contract bundles will continue to contain the old
name where changing bytes would falsify history or break a digest.

For a transition period, searches for AetherIot may refer either to the former
edge repository name or to the AetherIoT platform. Migration documents must make
that distinction explicit.

## Rollout and rollback

The rollout order is documentation, source-reference preparation, repository
rename, remote metadata update, then post-rename verification. The detailed
checklist and rollback procedure live in
`docs/migration/aetheriot-to-aetheredge.md`.

Rollback may restore the GitHub repository name and repository-facing URLs. It
must not rewrite published artifacts, change protocol identifiers, or collapse
the product-family model.

## Alternatives considered

- Keep AetherIot for both platform and edge product: rejected because the
  ambiguity already affects website, docs, SDK, and installer language.
- Rename every `aether-*` software identifier: rejected as high-cost churn with
  little user value and unnecessary compatibility risk.
- Wait for a matching GitHub organization name: rejected because repository
  ownership and display-name architecture are independent decisions.
