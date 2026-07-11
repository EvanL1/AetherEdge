---
title: Shared Memory
description: The SHM data plane - slot layout, writer ownership, seqlock reads, generations, and the PointWatch event plane
updated: 2026-07-10
---

# Shared Memory

Live values in Aether do not travel through a broker or a database on the hot
path. io (the communication service) and automation (the model/rule service)
share one memory-mapped file — the SHM segment — and exchange fixed-size
notifications over Unix domain sockets. A device reading lands in shared
memory in tens of nanoseconds. This page describes the segment itself and the
two socket-based signaling planes built on top of it. For the
services around it see [Architecture](architecture.md); for what the values
mean see [Data Model](data-model.md).

Source of truth: `libs/aether-rtdb-shm/` (layout, writers, readers, sockets)
and `services/io/src/core/channels/shm_listener.rs` (the command
listener).

## Layout

The segment is a single file: a 64-byte header followed by a fixed-size array
of 32-byte point slots (`calculate_file_size` in
`libs/aether-rtdb-shm/src/core/header.rs` is exactly
`64 + 32 × max_slots`; the default capacity is 100,000 slots). Both struct
sizes are compile-time asserted.

The file path is resolved by `default_shm_path()`
(`libs/aether-rtdb-shm/src/core/config.rs`) in this order:

1. `AETHER_SHM_PATH` environment variable, if set.
2. `/shm/rtdb/aether-rtdb.shm`, if the `/shm/rtdb` directory exists (the
   Docker deployment mounts a shared tmpfs volume there).
3. `/dev/shm/aether-rtdb.shm` on Linux (RAM-backed tmpfs).
4. `/tmp/aether-rtdb.shm` otherwise (macOS development).

The header (`UnifiedHeader`, `#[repr(C, align(64))]`) carries: a magic number,
a layout version, `max_slots`, the live `slot_count`, a last-update timestamp,
a writer heartbeat, `routing_hash` (a fingerprint of the channel/point layout),
and `writer_generation` (an incarnation counter). All multi-byte fields use
native endianness, so readers and writers must run on the same architecture.

Each `PointSlot` holds an engineering value (f64 bits), a raw value (f64
bits), a millisecond timestamp, a seqlock sequence counter, and a dirty flag.
A slot that has never been written holds a quiet-NaN sentinel in both value
fields — an unwritten slot is self-describing, never confusable with a real
device reading of zero. Downstream consumers filter on `is_finite`.

Slots are addressed by flat index. Each process independently derives the same
`(channel_id, point_type, point_id) → slot` mapping from the same channel
point counts (`allocate_layouts` in `libs/aether-rtdb-shm/src/layout.rs`);
the mapping itself never crosses the process boundary — agreement is verified
through `routing_hash` instead. The allocator also inserts padding so that
slots owned by different writers never share a 64-byte cache line: without
that padding, false sharing between io and automation cores was measured at a
5.7× per-write penalty on the Cortex-A55 deployment target.

## Writer ownership is type-enforced

Channel points come in four slot types: telemetry (T) and signal (S) are the
measurement side; control (C) and adjustment (A) are the action side. The
ownership rule is:

- **io owns T/S slots.** It creates the segment at startup through
  `UnifiedWriter::create`, which truncates the file, initializes every slot to
  the NaN sentinel, and stamps the header.
- **automation owns C/A slots.** It opens the existing segment through
  `ActionWriter::open` — a newtype wrapping the full writer that exposes only
  `set_action` (which writes C/A slots), `generation()`, and a read-only
  channel-to-slot index. There is no general `set()` on the type.

The protection is primarily compile-time: a automation write to a telemetry slot
is unrepresentable because `ActionWriter` has no method for it, and the only
way to obtain a full writer over an existing file
(`UnifiedWriter::open_existing`) is crate-private. As defense in depth,
`set_action` also carries a runtime guard that refuses point types other than
control (2) and adjustment (3) and returns `false`.

`UnifiedReader` provides the read-only view for other consumers (automation rule
evaluation, the `aether` CLI); it validates the same header chain on open, and
a raw variant (`open_raw`) exists for debug tools that lack layout data.

## Consistency: seqlock

Each slot is protected by a per-slot seqlock: the writer bumps the sequence
counter to an odd value, writes the three data fields, then bumps it back to
even. Readers read the sequence, read the data, and re-read the sequence; the
snapshot is valid only if both reads returned the same even value. Memory
ordering uses paired Acquire fences on the read side and a Release fence plus
Release increment on the write side — the comments in
`libs/aether-rtdb-shm/src/core/slot.rs` explain why single Acquire loads are
insufficient on AArch64.

Two read entry points exist, and choosing the right one matters:

- `try_load_consistent()` — a single attempt that returns `None` on any
  contention (odd sequence or sequence change). This is the variant for tasks
  running on async runtime worker threads: never spin on a tokio worker.
- `load_consistent()` — retries `try_load_consistent` up to 32,768 times with
  a spin hint, bounding worst-case spinning to roughly 3–16 ms under extreme
  contention. It is intended for dedicated threads. When retries are
  exhausted it logs a warning and returns `None` — it never returns torn
  data.

In production the retry path almost never iterates: protocol I/O between
writes means a reader rarely collides with a write in progress.

## Generations and rebuilds

Two header fields let readers detect that their view of the segment is stale:

- **`routing_hash`** is the fingerprint of the channel point layout. io
  writes it at create time; every open path (reader or `ActionWriter`)
  recomputes its own fingerprint from local configuration and refuses to open
  on mismatch — slot indices would silently point at the wrong points
  otherwise. The error message tells the operator to restart io to
  resynchronize.
- **`writer_generation`** identifies the writer incarnation. It is seeded at
  create time from wall-clock nanoseconds combined with a per-process nonce,
  forced even and nonzero: the invariant is "even at rest, odd while a
  reconfigure is in flight," so readers gate themselves out on odd values.
  automation compares the generation on every dispatch and detects a io
  restart or reconfigure it has not caught up with.

Reconfiguration (a routing reload that changes the slot layout) never mutates
the live mapping in place. `ShmHandle::rebuild_via_swap`
(`libs/aether-rtdb-shm/src/shm_handle.rs`) creates a complete fresh file at a
staging path (`aether-rtdb-<nanos>.shm`), flushes it, and then publishes it
with a POSIX `rename(2)` over the canonical path — atomic on the same
filesystem. Readers still holding a mmap of the old file keep reading the old
inode (the kernel keeps it alive) until they reopen; there is no window in
which anyone can observe a torn header or partially cleared slots. automation
notices the swap through an inode watcher on the canonical path
(`services/automation/src/main.rs`): when the inode changes it fires the rebuild
trigger, and a background task reopens the `ActionWriter` against the new
file with exponential backoff. Staging files orphaned by a crash between
create and rename are cleaned up at the next io startup.

## Command notifications

When automation issues a command — a rule action or an HTTP control request (see
[Safe Operations for AI Agents](../domain/safe-operations.md) for what is
allowed to reach devices) — it writes the C/A slot through its `ActionWriter`
and then sends a notification over a Unix domain socket
(`/tmp/aether-m2c.sock`) so io reacts immediately instead of polling. In
measurement the notify path is sub-millisecond (the SHM write plus UDS send
benches at microseconds; see `libs/aether-rtdb-shm/benches/BASELINE.md`);
~1–2 ms is the design budget the dispatch code documents for the happy path.

The notification (`ShmNotification`) is a fixed 56-byte frame carrying the
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
what makes the rule engine event-driven (see [Rule Engine](rule-engine.md)). After every T/S
slot write, io consults a **subscription bitmap** — a separate 12,504-byte
mmap file (`aether-rtdb-point-watch-subs.shm`, next to the main segment) of
atomic u64 words covering all slots. io creates it zero-filled at
startup; automation sets bits when it loads or reloads rules. The hot-path check
is a single relaxed atomic load and bit test, about 1–2 ns, and the common
case (slot not subscribed) returns immediately.

On a hit, io builds a 56-byte `PointWatchEvent` — channel, point, point
type, value bits, raw bits, slot index, timestamp, producer ID — and pushes it
to a bounded in-process channel (capacity 2048) drained by a background task
that batches up to 64 events per write onto a dedicated socket
(`/tmp/aether-point-watch-automation.sock`, aether-automation listens, aether-io connects, same
1–5 s reconnect backoff as the command plane). Because the event carries the
value itself, automation evaluates deadband directly from the event with no
read-back; duplicate events are harmless (at worst an extra
deadband check), which is why the frame has no sequence field.

On the automation side the pipeline stays bounded end to end: the listener
forwards frames into a 1024-capacity channel, and the dispatcher
(`PointWatchDispatcher` in `libs/aether-rules/`) maps
`(channel, point) → rule IDs` and forwards wake-up events into the
scheduler's own 1024-capacity channel. Every stage uses a non-blocking
`try_send`; on overflow the event is dropped and a `dropped_count` counter is
incremented rather than ever blocking io's write path. Dropped events are
recovered by the rule engine's periodic tick, so overload degrades to the old
polling latency instead of losing correctness.

The payoff, measured on production hardware (Cortex-A55 @ 1.4 GHz, ECU-1170)
for the initial PointWatch benchmark: point-change-to-event-delivery latency of 206 µs at
P50 and 526 µs at P99 (rule evaluation brings the cumulative figure to
~215 µs P50 — see [Data Flow](data-flow.md)), versus 50–150 ms under the
previous Redis-tick model — roughly a 500× improvement at the median.

## Related pages

- [Architecture](architecture.md) — the services that share this segment
- [Data Model](data-model.md) — what T/S/C/A values mean, and the NaN sentinel
- [Data Flow](data-flow.md) — uplink/downlink paths and the latency budget
- [Rule Engine](rule-engine.md) — the consumer of PointWatch events
- [Safe Operations for AI Agents](../domain/safe-operations.md) — which writes reach devices
