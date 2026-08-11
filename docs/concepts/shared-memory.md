---
title: Shared Memory
description: The SHM data plane - slot layout, writer ownership, seqlock reads, generations, and the PointWatch event plane
updated: 2026-08-09
---

# Shared Memory

Live values in Aether do not travel through a broker or a database on the hot
path. io (the communication service) and automation (the model/rule service)
share an IO-owned point segment plus a separate channel-health segment and
exchange fixed-size notifications over Unix domain sockets. A small commit
witness proves that both segments came from the same physical topology
publication. A device reading lands in shared
memory in tens of nanoseconds. This page describes the segment itself and the
two socket-based signaling planes built on top of it. For the
services around it see [Architecture](architecture.md); for what the values
mean see [Data Model](data-model.md).

Source of truth: `crates/aether-dataplane/` (physical header, slots, locking),
`libs/aether-shm-bridge/` (typed manifests, point/health publication and
self-healing readers), and `services/io/src/core/channels/shm_listener.rs`
(the command listener).

## Layout

The live-state segment is a 64-byte header followed by an exact-sized array of
32-byte point slots. `calculate_file_size` in
`crates/aether-dataplane/src/core/header.rs` is exactly
`64 + 32 × slot_count`; spare capacity is not mapped into the file. The
composition still enforces its configured resource limit before creation, but
that safety limit is not part of the physical ABI. Header and slot sizes are
compile-time asserted. Channel health uses a second exact-sized segment with
one dense slot per configured channel.

The file path is resolved by `default_shm_path()`
(`crates/aether-dataplane/src/core/config.rs`) in this order:

1. `AETHER_SHM_PATH` environment variable, if set.
2. `/shm/aether/aether-live-state.shm`, if the `/shm/aether` directory exists (the
   Docker deployment mounts a shared tmpfs volume there).
3. `/dev/shm/aether-live-state.shm` on Linux (RAM-backed tmpfs).
4. `/tmp/aether-live-state.shm` otherwise (macOS development).

The v5 header (`ShmHeader`, `#[repr(C, align(64))]`) carries the `AETHER__`
magic, layout version, live `slot_count`, owner heartbeat, `layout_hash`,
`writer_generation`, and `publication_epoch`. There is no physical
`max_slots`, last-update timestamp, or ambiguously named routing field. All
multi-byte fields use native endianness, so readers and writers must run on the
same architecture.

Each `PointSlot` holds an engineering value (f64 bits), a raw value (f64
bits), a millisecond timestamp, a seqlock sequence counter, and the acquisition
quality code. There is no dirty flag or companion dirty bitmap in the write
path. A slot that has never been written holds a quiet-NaN sentinel in both
value fields — an unwritten slot is self-describing, never confusable with a
real device reading of zero. Downstream consumers reject non-finite values and
decode the stored quality; unknown quality codes fail closed.

Slots are addressed by flat index. `ChannelPointManifest` compiles the exact
configured physical addresses in deterministic channel/kind/point order;
sparse point IDs do not create implicit addresses or holes. Its only padding
rule aligns a channel's first C/A slot to a 64-byte cache-line boundary when
that channel also has T/S points, keeping the two writer authorities off one
cache line. `ChannelHealthManifest` independently compiles configured channel
IDs into dense health slots. Agreement is verified through each manifest's
`layout_hash`, exact slot count, committed publication epoch, and writer
generation. Logical measurement/action routing and protocol register mapping
do not participate in either physical layout.

Topology growth and shrink never resize a mapped live inode in place. io
builds an exact-sized staging generation, copies state only for identical
`(channel, kind, point)` addresses, and atomically publishes the new inode.
Added points start with the unwritten NaN sentinel; removed points disappear;
unchanged points retain value, raw value, timestamp, and quality even when
their slot index changes. Readers fence the retired generation and reconnect
through the canonical path.

The live mmap ABI is not the persistent snapshot format. Snapshot v1 uses its
own `AETHSNAP` header (`slot_count` plus `layout_hash`) and an explicit
absent/present record for every slot; present records contain value, raw value,
timestamp, and quality. It deliberately excludes heartbeat, writer generation,
publication epoch, and seqlock state. Live layouts before v5 and every earlier
snapshot representation are rejected; there is no decoder for older versions.

## Writer ownership is type-enforced

Channel points come in four slot types: telemetry (T) and signal (S) are the
measurement side; control (C) and adjustment (A) are the action side. The
ownership rule is:

- **io acquisition owns T/S slots.** `ShmAcquisitionStateWriter` accepts only
  typed `AcquiredPointSample` batches and rejects C/A addresses before any
  mutation.
- **governed command dispatch mirrors C/A slots.** `ShmDeviceCommandSink`
  resolves one typed physical target, checks the writer generation before and
  after the mirror, and sends the complete command frame to io. It cannot
  write T/S addresses.

The protection is primarily typed at the adapter port boundary; raw
slot-indexed writes stay inside the physical adapter. Runtime checks provide
defense in depth for manifest membership, slot bounds, stable generation, and
canonical-file identity.

Ordinary point and channel-health writes do not update liveness. Dedicated io
tasks publish the point-plane and health-plane heartbeats, so automation's C/A
mirror writes cannot impersonate acquisition health. Readers treat a missing
or stale owner heartbeat as unavailable.

`ShmReadTopologyGeneration` provides the production read view. It binds point
and health manifests to one commit witness and pins both writer generations;
debug tools may still open a single physical segment explicitly.

## Consistency: seqlock

Each slot is protected by a per-slot seqlock: the writer bumps the sequence
counter to an odd value, writes value, raw value, timestamp, and quality, then
bumps it back to even. Readers read the sequence, read the data, and re-read
the sequence; the snapshot is valid only if both reads returned the same even value. Memory
ordering uses paired Acquire fences on the read side and a Release fence plus
Release increment on the write side — the comments in
`crates/aether-dataplane/src/core/slot.rs` explain why single Acquire loads are
insufficient on AArch64.

The data plane performs one `try_load_consistent()` attempt per slot. An odd or
changed sequence returns contention rather than spinning on an async runtime
thread; callers retry on their next bounded read cycle. The production data
plane contains no spin retry loop.

`SlotSource::read_slots` makes a multi-point operation one batch read session.
`ReconnectingSlotSource` fixes one mmap reader and one local read lock for the
complete batch, checks heartbeat once, and validates writer generation and
publication identity before and after the batch. Individual slots are each
seqlock-consistent, but the batch is not an atomic cross-slot snapshot. Any
slot contention or fencing failure rejects the complete batch. History uses
this contract instead of repeating file identity, clock, header, and lock work
for every selected series.

## Generations and rebuilds

Three identities let readers detect that their view is stale:

- **`layout_hash`** is the fingerprint of the exact physical slot layout. io
  writes it at create time; every coordinated open path
  recomputes its own fingerprint from local configuration and refuses to open
  on mismatch — slot indices would silently point at the wrong points
  otherwise. The error message tells the operator to restart io to
  resynchronize.
- **`writer_generation`** identifies the writer incarnation. It is seeded at
  create time from wall-clock nanoseconds combined with a per-process nonce,
  forced even and nonzero: the invariant is "even at rest, odd while a
  reconfigure is in flight," so readers gate themselves out on odd values.
  command/read adapters compare the generation on every operation and detect an io
  restart or reconfigure it has not caught up with.
- **`publication_epoch` + commit witness** bind the point and health files to
  one completed IO transaction. The witness also records both hashes, counts,
  and writer generations. Missing, partial, corrupt, or mixed publications
  fail retryably; readers never guess from equal hashes.

Reconfiguration never mutates a live layout in place. `ShmWriterHandle` and
`ShmChannelHealthWriterHandle` build complete staging files and atomically
rename them over their canonical paths while holding one cross-plane
publication lease. The commit witness is renamed last and is the
linearization point. Retained mmaps are fenced by an odd writer generation;
self-healing readers may reopen only the epoch and writer generation pinned by
their service-level topology. History and Uplink replace their SQLite routes
and committed SHM read view as one `Arc`, so a collection pass cannot mix
logical and physical generations. Crash-orphaned staging files are bounded
and cleaned on recovery.

The physical contract is v5 only. An old v4 mmap, an old snapshot, or a mixed
v4/v5 process set is invalid input. Upgrade by stopping the six services,
removing the obsolete runtime files, and starting the complete v5 composition
so io publishes a new point/health pair and commit witness.

## Command notifications

When automation issues a command — a rule action or an HTTP control request (see
[Safe Operations for Applications and Agents](../guides/safe-operations.md) for what is
allowed to reach devices) — `ShmDeviceCommandSink` mirrors the C/A value into
the pinned writer generation and sends a notification over a Unix domain socket
(`/tmp/aether-m2c.sock`) so io reacts immediately instead of polling. In
measurement the notify path is sub-millisecond; ~1–2 ms is the design budget
the dispatch code documents for the happy path.

The notification (`DeviceCommandFrame`) is a fixed 56-byte frame carrying the
routing target (channel, point type, point), the command payload (value bits
plus issue and expiry timestamps), and producer ordering (`producer_id`, a per-incarnation ID
that changes on every automation restart, plus a monotonic `seq`). Because the
frame carries the full command, io never has to read the slot back — and
two rapid writes to the same point arrive as two events rather than collapsing
into one.

io's `ShmCommandListener` binds the socket, immediately restricts it to
mode 0600 (refusing to listen if that fails — anyone who can write this socket
can inject device commands), and dedupes incoming events per point: a
different `producer_id` always resets state (a automation restart), while within
the same producer a frame is dropped as stale or duplicate using wrapping
sequence comparison (`seq.wrapping_sub(last_seq) > u64::MAX / 2`). Expired
frames are dropped before queueing. The unified channel task then checks the
value again against the configured writable point, inclusive min/max, and
step immediately before calling the protocol adapter. Unknown points, invalid
point constraints, NaN/infinity, and a rejected member of a batch all fail the
whole command without touching hardware. On the
sending side, `ShmNotifier` retries a failed write three times, then marks
itself disconnected and reconnects with exponential backoff (1 s doubling to a
5 s cap). There is no polling fallback: if the socket stays down, the notify
result reports degraded delivery and the caller decides what to surface.

## The PointWatch event plane

Commands flow automation → io; PointWatch is the reverse direction, and it is
what makes the rule engine event-driven (see [Rule Engine](rule-engine.md)).
After every T/S slot write, io consults each consumer's **subscription bitmap**
— a separate versioned mmap of atomic u64 words beside the main segment. Its
capacity comes from the deployment's `shared_memory.max_slots` resource cap;
its exact length is a 32-byte self-describing header plus
`ceil(max_slots / 64) × 8` bytes. It therefore has no compiled-in 100,000-slot
ceiling. Paths are derived from the resolved live-state path, for example
`aether-live-state-point-watch-subs-automation.shm`; automation, alarm, and API
own independent bitmaps and sockets. The common unwatched path is one relaxed
atomic load and bit test per consumer.

Bitmap creation is serialized by an atomically published authority sidecar.
The sidecar is read-only to non-owners because advisory locking never needs to
mutate its contents; a newly published bitmap is mode `0666` so the root-owned
io process and an explicitly unprivileged consumer can map the same atomic
words. Reopening a valid bitmap never changes its ownership or permissions.
The header binds magic, format version, capacity, and word count to the exact
file length. Obsolete, malformed, or capacity-mismatched files are never
decoded; the composition owner publishes a clean bitmap generation instead.

On a hit, io builds a 16-byte little-endian `PointWatchEvent`: `channel_id`
(u32), `point_id` (u32), `slot_index` (u32), point kind (u8), and a three-byte
v1 frame marker. Construction rejects a slot that cannot fit the u32 wire field
and never truncates. Decoding rejects an unversioned or malformed frame, so an
older sender cannot be misinterpreted. The frame contains no value, raw value, timestamp, quality,
producer ID, or sequence. It is only a wake-up hint and cannot compete with
SHM authority. A background task drains the bounded in-process channel in
batches of up to 64 frames onto each consumer's isolated socket.

On the automation side the pipeline stays bounded end to end: the listener
forwards frames into a 1024-capacity channel, and the dispatcher
(`PointWatchDispatcher` in `libs/aether-rules/`) maps
`(channel, kind, point) → rule IDs` and forwards wake-up events into the
scheduler's own 1024-capacity channel. Before dispatch, automation validates
the typed address/slot against its pinned manifest and re-reads the sample from
that same SHM generation. API and alarm perform the same manifest validation
and authoritative SHM re-read. Every stage uses non-blocking `try_send`; on
overflow the hint is dropped and `dropped_count` is incremented rather than
ever blocking io's write path. Periodic polling remains the repair path for a
dropped hint.

## Local observability without mandatory HTTP

`ShmObserver` in `aether-shm-bridge` opens the point plane, channel-health
plane, and topology commit through read-only paths. It validates exact mmap
layouts, stable generations, the common publication epoch, commit identity,
and dedicated heartbeat age. Its optional O(N) scan reports present,
unwritten, quality, online/offline, invalid, and contended slot counts. It
does not take writer authority, refresh heartbeat, repair files, or trigger a
topology publication.

Operators use the existing local CLI surfaces:

```bash
aether shm info             # one human-readable observation
aether --json shm info      # script/agent observation
aether shm info --no-scan   # O(1) header and commit validation
aether shm top              # continuously refreshed terminal UI
aether shm serve            # optional loopback browser UI
```

`aether doctor` uses the same observer, so system diagnostics and the SHM
dashboards cannot disagree by checking different planes. HTTP is optional and
no permanent observer process is required. `aether shm serve` embeds a
self-contained page and same-origin JSON endpoint, binds only to loopback, and
exists only for the CLI process lifetime. It has no write route and cannot
become SHM authority.

## Related pages

- [Architecture](architecture.md) — the services that share this segment
- [Data Model](data-model.md) — what T/S/C/A values mean, and the NaN sentinel
- [Data Flow](data-flow.md) — uplink/downlink paths and the latency budget
- [Rule Engine](rule-engine.md) — the consumer of PointWatch events
- [Safe Operations for Applications and Agents](../guides/safe-operations.md) — which writes reach devices
