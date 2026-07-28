# aether-ports

Small, object-safe capability interfaces for kernel and downstream adapters.

The crate separates authoritative live reads, device command dispatch, audit,
history sinks, mirroring, durable outbox, uplink publishing, alarm operations,
automation mutations, and I/O channel commissioning. The owner-only physical
writer remains isolated in `aether-acquisition-port`.

It deliberately exposes no generic database, cache, model, processor, or script
runtime. Hosts choose concrete adapters at composition boundaries. Port errors
retain recovery semantics so callers can distinguish unavailable, timeout,
conflict, rejected, invalid-data, and permanent failures.

```bash
cargo test -p aether-ports
```

Licensed under either MIT or Apache-2.0, at your option.
