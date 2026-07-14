# ADR-0002: Extract the SHM data plane and add a local durable outbox

## Status

Accepted and implemented on 2026-07-10. Compatibility aggregate removal
completed on 2026-07-13 after ADR-0012 rolling conformance passed.

## Context

The physical shared-memory implementation lived inside `aether-rtdb-shm`, a
legacy aggregation crate that also depends on routing, SQLx, Tokio, and the
generated workspace dependency bundle. This prevented a minimal gateway from
using the production SHM slot layout without compiling unrelated databases.

The first local-store implementation exposed only an in-memory outbox. It
satisfied the port shape but could not meet the edge invariant that accepted
uplink data survives network outages and process restarts.

## Decision

1. `aether-dataplane` owns the business-neutral physical SHM implementation:
   header/layout math, atomic slots, mmap reader/writer, dirty bitmap, path
   helpers, and tear-resistant snapshots.
2. During the staged migration, `aether-rtdb-shm::core` became a compatibility
   re-export. The typed
   `ChannelPointManifest` in `aether-shm-bridge` is the sole implementation of
   deterministic T/S/C/A slot allocation, reverse attribution, and the layout
   hash. The legacy SQLite count loader stays at the composition boundary;
   `ChannelPointCounts`, `ChannelLayout`, `ChannelToSlotIndex`, and
   `ReverseSlotIndex` are temporary compatibility projections. Instance,
   routing, and action adapters remained in the legacy crate until migrated
   separately. That aggregate and all of these projections are now removed.
3. Public mmap constructors validate mapped length, declared capacity, and
   live slot count before any pointer dereference and return typed
   `DataplaneError` values. Read-only consumers receive a `HeaderSnapshot`,
   not writable atomic cells. Unsafe blocks retain local layout, alignment,
   bounds, lifetime, and writer-ownership explanations.
4. `FileOutbox` is the dependency-free deployment's durable queue. It uses a
   bounded, versioned binary append log with per-record checksums, synchronous
   durability before success, torn-final-record recovery, process-level file
   locking, monotonic identifiers, and atomic compaction. Corruption before a
   later valid record fails recovery rather than truncating committed data.
5. File I/O runs on one owned worker thread. Async callers exchange bounded
   requests and one-shot responses; no mutex guard is held across an await.
6. `UplinkPublisher` defines the transport boundary and `OutboxForwarder`
   implements transport-neutral FIFO delivery and acknowledgement.
7. The compatibility `aether-uplink` routes periodic telemetry, gateway metrics, and
   alarm broadcasts through the durable outbox before MQTT submission and
   compacts acknowledged records at startup and hourly.
8. `ShmAcquisitionStateWriter` implements the acquisition-owned write port over
   one `SlotWriter` and its matching `ChannelPointManifest`. It resolves and
   validates the complete typed T/S batch, including duplicate and generation
   mismatch checks, before the first slot mutation. Its public lifecycle
   surface is limited to heartbeat, generation/slot-count observation, dirty
   draining, and snapshot saving; it exposes no arbitrary slot write.

## Delivery semantics

`FileOutbox` itself commits enqueue and acknowledgement records to disk before
reporting success. A failed or ambiguous acknowledgement retains the entry,
so the application-level contract is at-least-once.

The current `aether-uplink` MQTT adapter treats acceptance by `rumqttc::AsyncClient`
of a QoS 1 publish request as its delivery boundary. This removes loss during
ordinary disconnection and restart while an item is still in the outbox, but
there remains a crash window between local acknowledgement and broker PUBACK.
A future MQTT extension must correlate outgoing packet identifiers with
PUBACK events before claiming broker-confirmed crash durability.

## Consequences

### Positive

- Production SHM mechanics are available in the default Cargo graph without
  Redis, PostgreSQL, SQLx, or `workspace-hack`.
- The edge SDK has a real offline queue without requiring SQLite or another
  database engine.
- Legacy services can migrate one data path at a time behind stable ports.
- Production IO publishes the typed acquisition writer with its formal
  manifest. It expands and deduplicates C2C routes before one typed batch
  commit, while PointWatch receives the committed domain address rather than
  consulting a stale reverse index after rebuild.
- Corruption, double writers, capacity exhaustion, and retryability produce
  explicit errors rather than silent fallback.

### Negative

- The journal format and recovery logic become code that the project must
  maintain and fuzz.
- `fs2` is required for portable advisory file locking.
- The staged migration temporarily carried a legacy aggregation crate and
  duplicate compatibility tests; that cost ended with its removal.
- MQTT broker-level acknowledgement remains follow-up work.

## Compatibility shim removal criteria

The channel-layout shims in `aether-rtdb-shm` were removed after all of the
following became true:

1. io and automation construct and consume `ChannelPointManifest` directly,
   and the SQLite manifest loader has moved to a composition/configuration
   adapter.
2. No production target imports `ChannelPointCounts`, `ChannelLayout`,
   `ChannelToSlotIndex`, or `ReverseSlotIndex`; test-only compatibility users
   have also migrated to typed addresses.
3. Golden layout-hash and slot/reverse-slot contract tests pass for the old
   and new process versions used during a rolling restart.
4. The legacy crate's dependency on `aether-shm-bridge` can be removed together
   with the shims without changing SHM header or slot semantics.
5. Production io already uses the typed writer published with each manifest
   generation, expands C2C before the atomic port call, and preserves
   PointWatch. Delete `write_channel_batch_direct` after its remaining legacy
   tests and benchmarks migrate to domain batches; retain the fixed-generation
   constructor only while testkit/in-process compositions require it.

Closure evidence is the ADR-0012 four-quadrant compatibility matrix, the
frozen `legacy_v4_reader_accepts_new_io_segment_during_io_first_rolling_upgrade`
fixture, the inverse epoch-zero rejection contract, typed manifest/acquisition
writer contracts, and the architecture gate that rejects restoration of the
retired crate. All legacy tests and benchmarks were either migrated to the
typed contracts or removed with the APIs they exclusively exercised.

## Verification

```bash
cargo test -p aether-dataplane
cargo test -p aether-shm-bridge --test channel_manifest_contract
cargo test -p aether-shm-bridge --test acquisition_writer_contract
cargo test -p aether-io --test test_shm_store
cargo test -p aether-io --test test_shm_listener
cargo test -p aether-store-local
cargo test -p aether-application --test outbox_forwarder
cargo check -p aether-uplink
./scripts/check-architecture.sh
```
