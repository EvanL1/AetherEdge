# Domain packs

Domain packs contain declarative models, protocol mappings, rules, knowledge,
and AI evaluations for one industry. They cannot add Rust dependencies to the
edge kernel.

An extension adds executable capability; a pack adds domain knowledge. For
example, a Modbus driver is an extension, while a PCS register mapping and SOC
control rule belong to the energy pack.

Official distributions may package one or more packs over a compatible Aether
release. AetherEMS is the reference energy distribution; see
[ADR-0007](../docs/adr/0007-aether-core-and-ems-distribution.md).

Pack v1 manifests are loaded by the industry-neutral `aether-pack` crate and
the `aether_sdk::pack` facade. The machine-readable contract is
[`contracts/pack/pack-manifest.v1.schema.json`](../contracts/pack/pack-manifest.v1.schema.json).
Loading validates compatibility and fail-safe examples but never installs or
commissions a pack.

Compatibility is evaluated against the composition-provided, checksummed
`runtime-manifest.json`, not a hard-coded "full" catalog. Its v1 schema is
[`contracts/runtime/runtime-manifest.v1.schema.json`](../contracts/runtime/runtime-manifest.v1.schema.json);
missing adapters or capabilities reject activation before Pack assets load.

## Pack-only artifacts

The release bundle is a directory containing only closed metadata and Pack
data:

```text
example.bundle/
├── pack-artifact.json
└── pack/
    └── pack.yaml
```

Build it against the exact `runtime-manifest.json` produced for the target
Kernel artifact, not against a guessed version or a digest copied from another
target:

```bash
./scripts/build-pack-artifact.sh \
  packs/example \
  build/installer/runtime/runtime-manifest.json \
  release/example.bundle
```

On an already installed edge host, use its Kernel CLI:

```bash
aether packs install --artifact ./example.bundle
```

The CLI verifies the Kernel version, target triple, complete runtime-manifest
digest, closed metadata, exact file inventory, and Pack compatibility. It then
publishes to `<data-dir>/packs/<id>/<version>` and atomically activates that
absolute root in `global.yaml`. It does not copy a Kernel binary, start or
restart services, or commission any Pack example.

Tagged releases are configured to package the Energy Pack separately for each
target Kernel runtime as
`aetherems-energy-pack-<target>-<version>.tar.gz`. The archive and its
`.sha256` sidecar are release assets; GitHub build provenance is attached to
the archive and can be checked with:

```bash
sha256sum -c aetherems-energy-pack-<target>-<version>.tar.gz.sha256
gh attestation verify aetherems-energy-pack-<target>-<version>.tar.gz \
  --repo EvanL1/Aether
```

This documents the workflow contract, not proof that a particular tag or a
separate AetherEMS repository has already been published. ADR-0007 requires
real release digests and successful downstream CI evidence before extraction.
