# aether-domain

Industry-neutral, `no_std` domain types for the Aether edge kernel.

This crate defines point addresses and samples, strongly typed identifiers,
quality states, timestamps, and validated control commands. It has no async
runtime, database, network, service, or hardware dependency.

Use it when implementing an Aether host, extension, protocol adapter, or
firmware component that needs to exchange stable edge-domain values.

```bash
cargo test -p aether-domain
cargo tree -p aether-domain --edges normal
```

Licensed under either MIT or Apache-2.0, at your option.
