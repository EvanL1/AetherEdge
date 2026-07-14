# aether-dataplane

`aether-dataplane` is Aether's business-neutral shared-memory core. It can be
used by an embedded gateway without Redis, PostgreSQL, SQLx, routing models, or
any HTTP stack.

It owns:

- the stable 64-byte header and 32-byte aligned `PointSlot` layout;
- seqlock-consistent reads and single-writer atomic updates;
- read-only and writable mmap owners with RAII cleanup;
- process-local dirty-slot tracking;
- slot allocation bitmaps and generation path helpers;
- tear-resistant snapshot serialization.

Mmap constructors reject a mapping that cannot cover its declared capacity or
whose live slot count exceeds that capacity before exposing any header or slot
reference. Public failures use `DataplaneError`, allowing hosts to distinguish
invalid layout, invalid path, and operating-system I/O failures. Read-only
readers and the generic `SlotIo` trait expose header values as a
`HeaderSnapshot`, never as writable atomic cells. Logical manifest validation
remains the composition layer's job.

```bash
cargo test -p aether-dataplane
cargo tree -p aether-dataplane --edges normal
```

The former `aether-rtdb-shm` aggregation crate was retired after its rolling
v4 compatibility contracts passed. Industry-neutral code depends on this
crate directly; channel-aware composition belongs in `aether-shm-bridge`.
