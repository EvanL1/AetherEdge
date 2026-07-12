# ADR-0010: Separate physical acquisition addresses from logical application points

## Status

Implemented for the default production graphs on 2026-07-12. The legacy
aggregate remains available only to explicit compatibility and conformance
tests.

## Context

`PointAddress` identifies an instance point exposed through the application
query and command API. The legacy SHM and IO code also used that shape for a
different identity: a physical `(channel, point kind, point id)` tuple. In a
few paths a `channel_id` was therefore placed in an `InstanceId`, making an
authority error type-correct and blocking a clean `LiveStateWriter` adapter.

The distinction matters because IO alone owns telemetry/status acquisition,
while automation owns validated command/action dispatch. HTTP, CLI, MCP, and
AI clients may read logical live state and submit governed commands; they may
not manufacture physical acquisition samples.

## Decision

1. `PointAddress` remains the logical instance address consumed by
   `LiveState`, `EdgeApplication`, and `ControlCommand`.
2. `ChannelPointAddress` is the physical acquisition address. Its constructor
   accepts only telemetry and status kinds and carries a strongly typed
   `ChannelId`.
3. `AcquiredPointSample` contains finite engineering and raw values, source
   timestamp, and quality. NaN remains an internal unwritten-slot sentinel and
   is never a valid acquired sample.
4. `AcquisitionStateWriter` is the batch-oriented port granted only to the IO
   composition root. A batch must be rejected before its first write if any
   item is unknown or belongs to another writer.
5. `aether-shm-bridge` owns channel manifests, typed channel readers, the IO
   writer lifecycle, and per-consumer PointWatch publication. Application
   interfaces never receive the acquisition writer port.
6. The existing logical `LiveStateWriter` remains a compatibility surface for
   embedded test compositions only during the staged move. It is removed once
   the production SHM acquisition adapter implements `AcquisitionStateWriter`,
   the minimal example uses an explicit fixture seed, and no normal dependency
   graph references it.
7. Canonical SHM replacement is a linearized transaction. Acquisition batches
   and automation commands hold a shared OS lease on the stable
   `<canonical>.authority.lock` sidecar until their result or command receipt
   is formed. IO holds the exclusive lease from before staging begins through
   rename, canonical reopen, and local `ArcSwap` publication. An in-process
   read/write gate applies the same ordering between IO acquisition and rebuild.
   Mapped `(device, inode)` identity and header generation checks remain a
   fail-closed second layer for crashes or a replacement that bypasses the
   composition-root protocol.

## Consequences

- A physical channel can no longer be passed to new acquisition code as an
  instance by convention.
- IO/SHM adapters gain a small translation change before their implementation
  can move out of `libs/`.
- Simulation injection requires a separately declared application capability;
  it cannot obtain `AcquisitionStateWriter` from an HTTP, CLI, or MCP state
  object.
- A canonical SHM directory now contains one persistent authority-lock
  sidecar. Commands may time out rather than overlap a long generation publish;
  advisory locks are released automatically if a process exits.
- The six service binaries, rules engine, and CLI have no normal dependency on
  `aether-rtdb-shm`. Compatibility fixtures may keep it as an explicit
  development dependency until their wire-conformance coverage is replaced.

## Verification

```bash
cargo test -p aether-domain --test domain_contract
cargo test -p aether-ports --test port_contract
cargo test -p aether-shm-bridge --test acquisition_writer_contract \
  --test runtime_generation_contract --test point_watch_publisher_contract \
  --test device_command_sink_contract
cargo test -p aether-dataplane core::authority
./scripts/check-architecture.sh
```
