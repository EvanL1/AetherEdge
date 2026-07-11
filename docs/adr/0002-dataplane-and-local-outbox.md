# ADR-0002: Extract the SHM data plane and add a local durable outbox

## Status

Accepted and implemented on 2026-07-10.

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
2. `aether-rtdb-shm::core` becomes a compatibility re-export. Channel,
   instance, routing, and action adapters remain in the legacy crate until
   migrated separately.
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
- Corruption, double writers, capacity exhaustion, and retryability produce
  explicit errors rather than silent fallback.

### Negative

- The journal format and recovery logic become code that the project must
  maintain and fuzz.
- `fs2` is required for portable advisory file locking.
- The legacy SHM aggregation crate temporarily re-exports the new crate while
  business adapters are still split out.
- MQTT broker-level acknowledgement remains follow-up work.

## Verification

```bash
cargo test -p aether-dataplane
cargo test -p aether-store-local
cargo test -p aether-application --test outbox_forwarder
cargo check -p aether-uplink
./scripts/check-architecture.sh
```
