# aether-edge-sdk

Stable public facade for embedding the Aether AI-native IoT edge kernel.

The Rust library target is imported as `aether_sdk`. `AetherBuilder` has no
concrete infrastructure defaults. A host explicitly
provides authoritative live state, a device-command dispatcher, and the
mandatory audit sink. This keeps Redis, PostgreSQL, SQLx, web frameworks, and
protocol drivers out of the SDK's default dependency graph.

For a runnable zero-external-service composition, see the repository's
[`examples/minimal-gateway`](https://github.com/EvanL1/Aether/tree/main/examples/minimal-gateway).

```bash
cargo test -p aether-edge-sdk
cargo run -p aether-example-minimal-gateway
```

Licensed under either MIT or Apache-2.0, at your option.
