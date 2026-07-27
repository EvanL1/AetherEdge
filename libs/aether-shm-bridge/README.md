# aether-shm-bridge

Read-only capability bridge from Aether's authoritative shared-memory data
plane to the public `LiveState` port.

The bridge validates the logical channel manifest, supports per-consumer
PointWatch bitmaps and UDS hints, reports channel health, and reconnects after
writer restart or atomic SHM file replacement. It does not depend on Redis or
PostgreSQL and never grants a consumer acquisition-writer authority.

```bash
cargo test -p aether-shm-bridge
```

Licensed under either MIT or Apache-2.0, at your option.
