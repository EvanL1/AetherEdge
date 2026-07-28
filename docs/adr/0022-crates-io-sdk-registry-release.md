# ADR-0022: Publish the SDK facade and its closure to crates.io

## Status

Accepted on 2026-07-25. Supersedes ADR-0013 clauses 3 and 7. All other
ADR-0013 clauses remain in force. ADR-0026 later removes in-tree integrations
from the active workspace; previously reserved registry names remain
historical, while the SDK-facade and dependency-closure rule remains active.
The 2026-07-28 storage-agnostic routing convergence adds `aether-routing` to
that closure through the optional local-store routing adapter.

## Context

[ADR-0013](./0013-single-sdk-source-release.md) made `aether-edge-sdk` the only
supported Rust application facade, set `publish = false` on every workspace
package, and forbade `cargo publish` in release automation. Its reasoning was
that publishing every package would "expose implementation boundaries as public
acquisition choices and create separate SemVer promises for units that are not
intended to be consumed alone".

Two facts have since become clear.

First, the facade already prevents that exposure at the API level.
`crates/aether-sdk/src/lib.rs` is a 121-line re-export shell: downstream code
writes `aether_sdk::domain::EntityId` and `aether_sdk::ports::HistorySink`, and
never names `aether-domain` or `aether-ports`. An implementation package
appearing in the registry index is a Cargo packaging requirement, not a public
acquisition choice. The Rust ecosystem separates these routinely —
`serde_derive`, `tokio-macros`, and `hyper-util` are all published, all
resolvable, and none are independently supported products.

Second, the source-release-only decision has a measured distribution cost.
Cargo cannot fetch a dependency that is not in a registry, so the documented
way to consume the SDK is a Git dependency on a signed release commit. That
also removes AetherEdge from crates.io search, from docs.rs, and from lib.rs —
the three surfaces where Rust developers actually discover libraries. In the
fourteen days after the repository went public it drew seven unique visitors.

ADR-0013 clause 7 anticipated this and set the bar for revisiting it: a new ADR
and "a genuinely standalone public package". Reviewing the workspace against
that bar:

- `aether-dataplane` has zero internal dependencies and four external ones. It
  is a general mmap slot store with atomic generation swap, subscription
  bitmaps, and an authority lock. It is standalone in the strict sense and
  useful outside this project.
- `aether-domain` has zero dependencies of any kind. `aether-pack` has none
  that are internal.
- `aether-edge-sdk` is what downstream users actually want to add, and it is
  not standalone: it re-exports four internal packages plus one optional
  adapter.

The workspace layout is still not a registry product map. But "standalone" is
the wrong test for a facade whose entire purpose is to be the single name a
consumer types.

## Decision

1. `aether-edge-sdk` is the only supported public Rust API and the only package
   carrying a SemVer compatibility promise. ADR-0013 clauses 1 and 2 are
   unchanged.
2. The registry release set is `aether-edge-sdk` and the transitive closure of
   its normal and optional dependencies, plus `aether-testkit` so that
   adapter authors can run the port conformance suites. Every other workspace
   package keeps `publish = false`.
3. Packages in that closure other than `aether-edge-sdk` and `aether-testkit`
   are published to satisfy Cargo, are documented as implementation detail, and
   carry no independent compatibility promise. Depending on them directly is
   unsupported.
4. Local dev-dependencies inside the release set are declared path-only. They
   are excluded from published manifests, so they neither reach the registry
   nor constrain publish ordering.
5. Release automation publishes with `cargo publish --workspace --locked` after
   the GitHub Release job succeeds, using a `CARGO_REGISTRY_TOKEN` with publish
   permission. Cargo derives the order from normal dependency edges.
6. `scripts/check-open-source-readiness.sh` enforces the release set as an
   explicit allowlist: every package in it must be publishable, and every other
   Rust package in the workspace must still set `publish = false`. Adding a
   package to the registry requires editing that allowlist.
7. The signed source release, checksums, provenance bundle, and pinned-commit
   downstream flow from ADR-0013 clauses 4 and 5 continue unchanged. The
   registry release is an additional distribution channel, not a replacement.
8. The `0.5.0` versions yanked under ADR-0013 clause 6 stay yanked. The
   registry line restarts at the current workspace version.

## Consequences

- `cargo add aether-edge-sdk` works, and every package in the release set gets
  a docs.rs page and a lib.rs entry.
- Fourteen names become permanently reserved on crates.io. Renaming an internal
  package now leaves a stale registry name behind.
- Publishing is irreversible per version. Re-tagging an already-published
  version fails the publish job; a botched release needs a new version, not a
  retry.
- The support boundary is now documentation and lint policy rather than a
  registry-level impossibility. A downstream project can depend on
  `aether-domain` directly against advice, and nothing mechanically stops it.
- Internal package refactors still do not imply independently supported
  products, but they now have a registry cost that pure source releases did
  not carry.
