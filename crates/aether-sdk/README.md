# aether-edge-sdk

Versioned beta facade for embedding the Aether AI-native IoT edge kernel.

The API is release-gated for packaging and SemVer compatibility, but the first
independent registry release has not yet been completed. Until then, consume it
from a pinned repository revision rather than assuming crates.io availability.

The Rust library target is imported as `aether_sdk`. `AetherBuilder` has no
concrete infrastructure defaults. A host explicitly
provides authoritative live state, a device-command dispatcher, and the
mandatory audit sink. This keeps Redis, PostgreSQL, SQLx, web frameworks, and
protocol drivers out of the SDK's default dependency graph.

The `aether_sdk::pack` facade exposes the versioned, fail-closed domain-pack
manifest loader. Loading a pack validates compatibility and confined asset
directories; it never installs or commissions the pack.

The optional `local-runtime` feature exposes zero-external-service adapters
under `aether_sdk::local`. Downstream applications depend only on this facade;
the workspace's domain, port, application, and adapter crates are source
modules and do not define independent registry products.

```toml
[dependencies]
aether-sdk = { package = "aether-edge-sdk", git = "https://github.com/EvanL1/AetherEdge.git", tag = "v0.5.0", features = ["local-runtime"] }
```

For a runnable zero-external-service composition, see the repository's
[`examples/minimal-gateway`](https://github.com/EvanL1/AetherEdge/tree/main/examples/minimal-gateway).

```bash
cargo test -p aether-edge-sdk
cargo test -p aether-edge-sdk --features local-runtime
cargo run -p aether-example-minimal-gateway
```

Licensed under either MIT or Apache-2.0, at your option.
