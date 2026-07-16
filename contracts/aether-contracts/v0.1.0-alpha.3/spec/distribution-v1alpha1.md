---
id: distribution-v1alpha1
status: experimental
version: 0.1.0-alpha.3
normative: true
---

# Consumer distribution v1 alpha 1

This profile lets AetherCloud, AetherIot, and independent implementations pin
one exact AetherContracts release while keeping their default verification path
offline. Distribution conformance proves release identity and byte integrity;
it does not prove that a consumer codec, state machine, authentication profile,
or Broker integration is conformant.

## Authority and trust

The reviewed `aether-contracts.lock.json` in a consumer repository is the local
trust decision. It pins the release version, annotated tag object, peeled
commit, exact release URL, bundle size and SHA-256, and external manifest
SHA-256. The tag object and commit provide reviewable provenance identifiers.
The bundle and manifest digests provide the enforced byte identity.

GitHub tags, releases, and co-hosted checksum files are mutable distribution
evidence, not a second trust root. This alpha does not yet require a Sigstore or
SLSA attestation that cryptographically binds the bundle to the commit. A cache
or CDN may serve only bytes already accepted by the lock digest. Consumers must
not follow `main`, `latest`, a version range, or an unpinned action revision.

## Lock behavior

The lock is closed by
`schemas/distribution/v1alpha1/consumer-lock.schema.json`. Its safety policy is
fixed for this experimental line:

- `conformance_claim` is `distribution-only`;
- `production_release` is `false`;
- `legacy_default` is `true`;
- `physical_control` is `false`.

An `import` binds one release artifact path, one consumer destination path, and
one SHA-256. A `pending_import` records a release artifact that the consumer has
not adopted and a non-empty reason. Imported and pending source sets are
disjoint. The lock's `adoption` section declares the scope, required modules,
and exact required release-source closure. The union of imported and pending
sources must equal that closure. A `partial-consumer` has at least one pending
source; a `complete-consumer` has none and imports the entire closure. Complete
distribution adoption still says nothing about behavioral conformance.

The consumer commits the exact release `contract-manifest.json` bytes at the
lock's `manifest.local_path`. Offline verification checks its digest, release
identity, safety declarations, artifact declarations, and every imported
consumer byte. It performs no download, repair, fallback, or write.

## Online verification

The optional online verifier downloads only the URL named by the lock. It
enforces the exact response size and SHA-256 before inspection. It parses gzip
and tar in-process under the lock's maximum compressed bytes, expanded bytes,
entry count, path bytes, per-file bytes, and total regular-file bytes. It
rejects absolute or escaping paths, links, devices, unsupported entry types,
duplicate normalized paths, invalid checksums, malformed terminators, and any
archive layout other than the one locked release root. Extraction uses private
directories and files and occurs only after validation. It then verifies the
manifest plus every imported and pending release-source byte. Failure is
terminal; there is no fallback to a sibling checkout or consumer-local
candidate.

Consumers must pin `.github/actions/verify-consumer` to the full 40-character
peeled commit of the locked release. The composite Action passes its actual
action commit to the verifier, which rejects a different revision with
`ACTION_COMMIT_MISMATCH`. The lock path is consumer-relative and must remain
inside the consumer repository. This check complements, but never replaces, the
consumer's native codec and state-machine conformance tests.
