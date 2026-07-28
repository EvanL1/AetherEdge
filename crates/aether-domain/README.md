# aether-domain

Industry-neutral, `no_std` domain types for the Aether edge kernel.

This crate defines typed physical and logical point addresses, samples,
identifiers, quality states, timestamps, alarm policy, and validated device
commands. It has no async runtime, database, network, service, model framework,
or hardware dependency.

```bash
cargo test -p aether-domain
cargo tree -p aether-domain --edges normal
```

Licensed under either MIT or Apache-2.0, at your option.
