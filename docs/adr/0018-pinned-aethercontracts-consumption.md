# ADR-0018: Pinned AetherContracts consumption

## Status

Accepted on 2026-07-15. AetherContracts `v0.1.0-alpha.3` is the sole public
contract authority. AetherIot and AetherCloud consume it through byte-identical
`aether-contracts.lock.json` files. The current claim is
`distribution-only`; both products are complete consumers with no pending
imports. This does not upgrade production conformance.

## Context

Copying a candidate between product repositories made Cloud and Edge appear to
be co-authorities. A Git submodule or a dependency on `main` would retain
branch drift and make default checks network-dependent. Downloading a public
release without proving which bytes the Rust codec actually consumes would
also be insufficient.

Alpha.3 closes strict Runtime Manifest SemVer validation, the valid-digest
conflict replay vector, and duplicate session cursor rejection in both product
suites. These are fixture-level behaviors, not a production conformance claim.

## Decision

1. Tagged AetherContracts specifications, Schemas, fixtures, profiles, and TCK
   are authoritative for shared interoperability semantics. AetherIot retains
   its local Rust domain, codec, spool, and transport implementations.
2. Both products commit the same closed lock. It pins the annotated tag object,
   peeled commit, exact release URL, bundle size and SHA-256, release manifest
   SHA-256, and imported and pending artifact sets.
3. The default check is offline and adds no runtime dependency. A versioned
   release manifest and reference verifier closure are committed locally; the
   Rust contract test hashes all imported destinations against the lock and
   manifest.
4. Optional CI downloads only the locked asset and validates its size and
   digest before extraction. It pins the reusable Action to the full release
   commit and has no branch, `latest`, sibling-checkout, or repair fallback.
5. Alpha.3 imports 53 exact artifacts: the required specification, profiles,
   gates, failure taxonomy, TCK manifest, Schemas, fixtures, and verifier
   closure, with `pending_imports: []`.
6. `contracts/cloudlink/v1` remains the product integration path. Its local
   fixture manifest, wire profile, authentication draft, gates, and scenarios
   are migration history or product proposals and cannot redefine the public
   core.
7. Distribution integrity alone does not pass behavior gates. Separate product
   tests now execute all 25 fixture outcomes and stable failure codes, and the
   opt-in real-Broker dual harness records alpha fault evidence. Shared-Broker
   production authentication and crash-durable ACK gates remain unpassed.

## Consequences

- Contract upgrades require a reviewed lock update. A CDN may cache already
  pinned bytes but is never authority; Git submodules are not the default.
- Later releases require a reviewed closure update and behavior tests in both
  languages.
- Alpha.3 freezes an experimental transcript; production key provisioning,
  rotation, revocation, and verifier ownership remain planned.
- Legacy MQTT remains the default. No physical-control operation is added.
