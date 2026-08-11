# aether-shm-bridge

Typed composition bridge between Aether's authoritative shared-memory data
plane, its owner-only write ports, and read-only service consumers.

The bridge compiles exact configured `PhysicalPointAddress` values into a
deterministic `ChannelPointManifest`; sparse point IDs never become implicit
slots. The only point-plane padding aligns a channel's first C/A slot away from
its T/S cache line. `ChannelHealthManifest` maps configured channel IDs to
dense health slots. Both manifests contribute their `layout_hash`, exact slot
count, writer generation, and nonzero publication epoch to one coordinated
point/health commit witness.

Topology changes publish a new exact-sized inode rather than resizing a live
mapping. The bridge migrates only exact typed-address intersections; added
points start unwritten and removed points cannot leak state through a reused
slot index.

`ShmAcquisitionStateWriter` owns T/S writes and preserves value, raw value,
timestamp, and `PointQuality`. Governed command dispatch mirrors only C/A.
Ordinary writes do not refresh liveness; dedicated io tasks own point and
health heartbeats. Read-only generations reject stale/mixed publications and
reconnect only to the physical identity pinned by the service topology.

`SlotSource::read_slots` fixes one reader for a complete batch, validates
heartbeat once, and fences generation/publication before and after the batch.
Each returned slot is seqlock-consistent, but the batch is not an atomic
cross-slot snapshot.

PointWatch uses independent consumer bitmaps and 16-byte UDS wake-up frames
containing only channel ID, point ID, slot index, and kind. Automation, alarm,
and API validate the address/slot against their pinned manifest and re-read
SHM; no event payload is authoritative. Bitmap capacity is explicit and
versioned, selected from the same deployment resource cap used to compile the
manifests rather than from a fixed library constant.

`ShmObserver` provides direct read-only observability without HTTP or a
daemon. It grades the committed point/health pair as healthy, degraded, or
unhealthy from physical identity, heartbeat age, and an optional aggregate
slot scan; it never repairs or writes either plane.

The default point path is `aether-live-state.shm`. This bridge accepts only the
v5 mmap and snapshot v1 contracts exposed by `aether-dataplane`; it contains no
v4 or older-snapshot decoder. It does not depend on Redis or PostgreSQL and
never grants a read consumer acquisition-writer authority.

```bash
cargo test -p aether-shm-bridge
```

Licensed under either MIT or Apache-2.0, at your option.
