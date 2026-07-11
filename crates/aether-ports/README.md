# aether-ports

Small, object-safe capability interfaces for Aether edge extensions.

The crate separates authoritative live reads, acquisition-owned writes,
device command dispatch, audit, history, mirroring, durable outbox, and uplink
publishing. It deliberately does not expose a generic database or cache API.
Hosts choose concrete adapters at the composition boundary.

Errors carry recovery semantics so callers can distinguish unavailable,
transient, rejected, invalid-data, and permanent failures.

```bash
cargo test -p aether-ports
```

Licensed under either MIT or Apache-2.0, at your option.
