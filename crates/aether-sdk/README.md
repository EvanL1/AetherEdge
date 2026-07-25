# aether-edge-sdk

Versioned beta facade for embedding the Aether AI-native IoT edge kernel.

This is the only supported public Rust API for AetherEdge and the only package
carrying a SemVer compatibility promise. The other `aether-*` packages it pulls
in are published to satisfy Cargo's transitive dependency requirement; they are
implementation detail, and depending on them directly is unsupported. See
[ADR-0022](https://github.com/EvanL1/AetherEdge/blob/main/docs/adr/0022-crates-io-sdk-registry-release.md).

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
the workspace's domain, port, application, and adapter crates are reached
through it rather than named directly.

```bash
cargo add aether-edge-sdk --features local-runtime
```

```toml
[dependencies]
aether-sdk = { package = "aether-edge-sdk", version = "0.0.1", features = ["local-runtime"] }
```

To build against an exact signed release commit instead of the registry, the
pinned-source flow from
[ADR-0013](https://github.com/EvanL1/AetherEdge/blob/main/docs/adr/0013-single-sdk-source-release.md)
still applies:

```toml
[dependencies]
aether-sdk = { package = "aether-edge-sdk", git = "https://github.com/EvanL1/AetherEdge.git", tag = "v0.0.1", features = ["local-runtime"] }
```

For a runnable zero-external-service composition, see the repository's
[`examples/minimal-gateway`](https://github.com/EvanL1/AetherEdge/tree/main/examples/minimal-gateway).

```bash
cargo test -p aether-edge-sdk
cargo test -p aether-edge-sdk --features local-runtime
cargo run -p aether-example-minimal-gateway
```

Licensed under either MIT or Apache-2.0, at your option.
