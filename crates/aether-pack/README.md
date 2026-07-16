# aether-pack

Versioned, industry-neutral domain-pack manifest loading for the Aether edge
kernel. The crate validates a pack's own release identity, distribution
identity, compatible Aether range, required capability and protocol IDs,
explicitly uncommissioned examples, and pack-root-confined asset directories.

Loading is read-only. It does not install a pack, enable a channel or rule, or
commission field hardware. Unsupported or unknown manifest fields, missing
requirements, and paths that are absolute, traverse `..`, or resolve outside
the pack root fail with typed errors.

```rust
use aether_pack::{PackRuntime, load_pack_manifest};

# fn inspect() -> Result<(), aether_pack::PackError> {
let runtime = PackRuntime::new("0.5.0")
    .with_capabilities(["device.read_point"])
    .with_protocols(["modbus_tcp"]);
let manifest = load_pack_manifest("packs/example", &runtime)?;
println!("{} {}", manifest.id(), manifest.version());
# Ok(())
# }
```

## Active Pack configuration

Automation and `aether mcp` consume one shared entry point:
`<AETHER_CONFIG_PATH>/global.yaml`. The safe default activates no domain Pack:

```yaml
packs: []
```

An operator activates an installed Pack by declaring both the expected identity
and its root. The root may be absolute or relative to the configuration
directory, but may not contain `..`:

```yaml
packs:
  - id: energy
    root: /opt/aether/packs/energy
```

The configured identity must match `pack.yaml`. Every selected manifest is
validated for Aether compatibility, capabilities, protocols, commissioning,
and asset confinement before its models or knowledge become visible.

Pack-owned `mappings`, `rules`, `evaluations`, and `data_processing` tasks are
formal indexed asset categories. Each directory contains `index.yaml` using
`aether.pack.asset-index.v1`; manifest capability IDs, index IDs, and actual
regular files must match exactly. Unknown fields/files, duplicate IDs or paths,
symlinks, path escapes, media/schema mismatches, and oversized files fail
closed. Pack v1 fixes each category to its corresponding v1 payload schema;
changing a payload contract requires a Pack contract version change. Only
explicitly active Packs contribute namespaced
`<pack>/<category>/<asset>` identities.

The machine-readable contracts are the
[`Pack manifest v1`](https://github.com/EvanL1/AetherEdge/blob/main/contracts/pack/pack-manifest.v1.schema.json)
and
[`Pack asset index v1`](https://github.com/EvanL1/AetherEdge/blob/main/contracts/pack/pack-asset-index.v1.schema.json)
schemas.

Licensed under either MIT or Apache-2.0, at your option.
