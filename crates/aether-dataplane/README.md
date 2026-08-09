# aether-dataplane

`aether-dataplane` is Aether's business-neutral shared-memory core. It can be
used by an embedded gateway without Redis, PostgreSQL, SQLx, routing models, or
any HTTP stack.

It owns:

- the v5 64-byte `ShmHeader` and 32-byte aligned `PointSlot` layout;
- seqlock-consistent reads and single-writer atomic updates;
- read-only and writable mmap owners with RAII cleanup;
- owner-controlled heartbeat and generation fencing;
- point-quality persistence and generation path helpers;
- ABI-independent, tear-resistant snapshot serialization with durable rename.

Mmap constructors require an exact physical length for the declared live slot
count (`64 + 32 × slot_count`) before exposing any header or slot reference.
`ShmHeader` carries only magic/version, `slot_count`, owner heartbeat,
`layout_hash`, writer generation, publication epoch, and reserved bytes. A
`PointSlot` carries value, raw value, timestamp, sequence, and quality. There
is no dirty state, physical spare-capacity field, or automatic heartbeat update
on a point write.

Production reads make one seqlock attempt. Contention returns `None` to the
adapter; the data plane does not spin through a retry loop. Public failures use
`DataplaneError`, allowing hosts to distinguish
invalid layout, invalid path, and operating-system I/O failures. Read-only
readers and the generic `SlotIo` trait expose header values as a
`HeaderSnapshot`, never as writable atomic cells. Logical manifest validation
remains the composition layer's job.

```bash
cargo test -p aether-dataplane
cargo tree -p aether-dataplane --edges normal
```

The canonical default file is `aether-live-state.shm`. The v5 layout is the
only accepted physical format. Snapshot v1 is a separate `AETHSNAP` format and
does not serialize heartbeat, writer generation, publication epoch, or seqlock
state; earlier mmap and snapshot representations are rejected. Industry-neutral
code depends on this crate directly; channel-aware composition belongs in
`aether-shm-bridge`.
