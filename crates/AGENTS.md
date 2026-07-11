# Core Crate Instructions

These instructions apply to all crates in this directory.

- Core crates expose stable, industry-neutral concepts.
- No Redis, PostgreSQL, SQLx, Axum, MQTT client, or hardware-driver dependency.
- `aether-domain` owns entities, value objects, commands, and domain events.
- `aether-ports` owns small object-safe capability traits.
- Port errors must expose recovery semantics such as transient, unavailable,
  rejected, or permanent failure.
- Do not add a generic `Database`, `Cache`, or `Rtdb` trait.
- Public APIs require rustdoc and behavior tests.
