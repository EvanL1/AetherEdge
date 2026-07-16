# Migrate from AetherIot to AetherEdge

The edge product and repository are renamed from AetherIot to AetherEdge.
AetherIoT becomes the umbrella project name for AetherEdge, AetherCloud, and
AetherContracts. This is a product-identity change, not a protocol or package
namespace rewrite.

## What changes

- Repository URL: `https://github.com/EvanL1/AetherEdge`.
- Source checkout directory and new source archive prefix: `AetherEdge`.
- Product-facing documentation, badges, release links, CI examples, and website
  navigation use AetherEdge.
- AetherCloud and future AetherContracts documentation refer to the edge
  product as AetherEdge.

## What stays stable

- The `aether` CLI and `aether-*` binaries.
- Rust crate and import names, including `aether-edge-sdk` and `aether_sdk`.
- Configuration keys, environment variables, service identities, and on-disk
  paths unless a separate compatibility decision changes them.
- Installer name `AetherEdge-<arch>-<version>.run`.
- CloudLink, Thing Model, Schema, TCK, and failure-code identifiers.
- Published tags, release assets, attestations, and digest-pinned
  AetherContracts alpha.3 artifacts.

## Update an existing clone

After the GitHub repository rename:

```bash
git remote set-url origin https://github.com/EvanL1/AetherEdge.git
git remote -v
```

Existing GitHub redirects are a transition aid, not the permanent
configuration. Update Git dependencies, submodules, badges, release automation,
attestation commands, and allowlists to the new URL.

## Maintainer rollout checklist

1. Publish the product-family overview, unified documentation structure, ADR,
   compatibility matrix, status page, and this migration guide.
2. Update product-facing references while excluding immutable releases,
   evidence, provenance records, and digest-pinned contract imports.
3. Verify AetherEdge docs and release workflows, AetherCloud docs, the website,
   and AetherContracts migration notice.
4. Rename `EvanL1/AetherIot` to `EvanL1/AetherEdge` on GitHub.
5. Update local remotes, repository descriptions, website links, default-branch
   references, release badges, and attestation examples.
6. Re-run checks and inspect open pull requests and release links through the
   new address.
7. Announce a compatibility window and retain the old-name explanation until
   downstream references no longer depend on redirects.

## Rollback

If a release, CI, package consumer, or documentation deployment cannot resolve
the new repository identity, pause the rollout and restore the former GitHub
repository name. Keep the AetherIoT product-family documentation and the stable
software identifiers; revert only repository-facing URLs and source archive
examples. Never rewrite published artifacts to simulate a rollback.
