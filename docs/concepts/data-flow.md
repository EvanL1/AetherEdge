---
title: Data Flow
description: SHM-native uplink and downlink paths end to end, with latency budgets
updated: 2026-08-14
---

# Data Flow

Aether moves data along two independent paths. The **uplink** carries
measurement points — telemetry (T) and signal (S) values — from devices through
aether-io into shared memory, and from there to every consumer. The **downlink**
carries action points — control (C) and adjustment (A) commands — from the rule
engine or the HTTP API through aether-automation back to a device. Live point values and
point reads use the shared-memory segment as their source of truth. Command
values are mirrored to SHM, while a durable Unix-socket notification and a
local SQLite admission/outcome ledger govern dispatch. No default service
needs Redis or PostgreSQL for live data.

## Uplink (device → consumers)

1. A protocol frame arrives on a communication channel and the channel's
   protocol adapter in aether-io decodes it into point values.
2. Event-driven adapters publish decoded `DataEvent` values into one bounded,
   non-blocking ingress owned by their channel. Pending point values coalesce
   latest-wins by `(point type, point id)` and effective sample time;
   connection state and heartbeat each have one latest-wins slot, while errors
   remain a bounded FIFO. A batch that would exceed the remaining unique-point
   capacity is rejected atomically rather than being partially admitted.
3. aether-io commits each typed T/S batch through
   `ShmAcquisitionStateWriter` (`libs/aether-shm-bridge/src/acquisition_writer.rs`).
   The adapter validates the immutable manifest and writer generation before
   and after mutation; slot-indexed writes are private implementation detail.
4. **Event path (immediate).** After every slot write, the
   `PointWatchPublisher` (`libs/aether-shm-bridge/src/point_watch.rs`) checks the
   independent bitmap owned by each event consumer. On a hit, a bounded queue
   sends a `PointWatchEvent` to that consumer's UDS. aether-automation,
   aether-alarm, and aether-api cannot steal or overwrite one another's subscriptions. The event
   is a wake-up hint only; each consumer re-reads SHM, and polling repairs
   dropped events.
5. **Direct read path.** Consumers resolve channel/instance coordinates from
   one SQLite topology snapshot and re-read matching SHM slots. History and
   Uplink bind their exact configured points and needed routes to one committed
   point/health epoch, then pin that immutable generation for a whole
   collection/upload pass. Events do not silently change their cadence.

The `aether-uplink` production binary compiles one CloudLink topic tree and one
durable `CloudLinkSpool`; generic MQTT property/status/read/write/call-data
interfaces are not compiled. CloudLink converts the pinned generation into
`PointSample` facts, adds publication epoch plus topology digest, and persists
canonical business content. MQTT QoS 1/PUBACK advances only the
transport state; a matching cloud durable application ACK is the sole removal
authority. Disconnect keeps records replayable under the same stream
position/batch ID/digest and does not block any SHM consumer.

Runtime Manifest, telemetry, data-loss, and alarm facts all use lossless
pending admission and cannot be evicted by later traffic. Only alarm
transitions additionally reserve a protected post-ACK receipt because Alarm's
durable HTTP retry may outlive Cloud removal; ordinary periodic facts do not
consume that receipt ledger. A full alarm receipt ledger rejects new internal
alarm admission and makes Uplink health unready without preventing ordinary
CloudLink ACK progress.
```
Device ──frame──► aether-io protocol adapter (decode)
                        │ non-blocking bounded ingress
                        │
                        ▼  set_direct (~10 ns/point)
                  SHM T/S slot (authoritative)
                   │             │
      per-consumer │             │ periodic sampling
      bitmap + UDS │             ├─► aether-history
       ┌───────────┴────┐        └─► aether-uplink
       ▼                ▼
 aether-automation aether-alarm/aether-api
    event hint   event hint → SHM re-read
```

## Downlink (rule/API → device)

1. An external HTTP, CLI, or MCP control call becomes a transport-neutral
   `RequestContext` in aether-automation. `ControlApplication` checks the
   `device.control` permission and explicit confirmation, persists a mandatory
   attempted audit event in local SQLite, and only then calls the command
   dispatcher. An internal deterministic rule action enters the existing
   dispatcher path directly during the staged migration.
2. The dispatcher calls aether-automation's `execute_action`
   (`services/automation/src/instance_data.rs`), which resolves the instance action point to its channel command point
   **once**, from the in-memory routing cache (a mirror of the `route:m2c`
   table populated by `aether sync`). The resolved target is threaded through
   the rest of the call so a concurrent routing reload cannot change the
   decision mid-flight.
3. The offline gate reads the channel-health SHM segment. An offline channel
   rejects the write with `ChannelUnreachable` before anything is written.
4. After value validation, `ShmDeviceCommandSink`
   (`libs/aether-shm-bridge/src/command_sink.rs`) mirrors the C or A slot. The
   writer generation and canonical path are checked before and
   after the write; a mismatch means aether-io restarted and rebuilt the segment,
   so the write is discarded and the dispatch fails rather than landing in a
   stale layout.
5. The same command adapter sends a fixed 104-byte little-endian frame over the
   command socket. It contains a persistent 128-bit `CommandId`,
   a SHA-256 digest of the canonical 40-byte payload, routing coordinates,
   value bits, and issue/expiry timestamps. The endpoint defaults to
   `/tmp/aether-m2c.sock` and may be set with `AETHER_M2C_SOCKET`.
   Before accepting frames, aether-io sends a fixed 16-byte server hello that
   proves the endpoint speaks the durable command protocol. It then returns
   a fixed acknowledgement for durable admission, not for physical device
   completion.
6. aether-io's `ShmCommandListener`
   (`services/io/src/core/channels/shm_listener.rs`) receives the
   notification, rejects malformed or expired frames, and binds each command
   identity to a semantic digest of channel, point kind, point ID, and value in
   a bounded SQLite ledger before queueing. The frame-level digest still
   integrity-protects issuance and expiry, but those server-generated times do
   not turn an otherwise identical HTTP retry into a different command.
   An exact retry after durable queue admission is idempotent while the terminal
   identity remains retained; reusing an ID with another digest is a conflict.
   The default protected window extends through command expiry plus at least 24
   hours, after which reuse is a new operation rather than a retry. Queue
   admission is marked durably before the accepted ACK is returned. Immediately
   before protocol dispatch,
   `CommandGuard` verifies
   that the writable point exists and that the value satisfies its
   min/max/step policy; only then can the protocol adapter write it to the
   field bus. The ledger then advances through received, queued, dispatching,
   and a terminal outcome.

The SQLite ledger stores command identity, admission, and outcome metadata; it
does not replace SHM as the live-value authority and it is never used to replay
a device command. A crash after queue admission or after dispatch begins can
leave the physical result unknowable, so recovery records `possibly_applied`
and requires reconciliation from feedback telemetry rather than automatic
resubmission. Query a retained result at the authenticated service-local
`GET /api/commands/{command_id}/outcome` route, or through the gateway at
`GET /api/io/api/commands/{command_id}/outcome`; capability
`device.command_outcome.read` requires `device.read`. The accepted timestamp
proves durable queue admission, not device success.

There is no alternate write-only command transport. A connection, write,
acknowledgement, timeout, malformed acknowledgement, or negative
acknowledgement failure is returned to the caller and never retried through a
weaker protocol.

The channel-owned DataEvent ingress exposes accepted, coalesced, full/closed/
contention/oversized drops, shutdown discards, pending capacity, and high-water
marks in io status and channel diagnostics. Current closed or saturated ingress
state fails io health; historical drop counters remain visible without making
readiness permanently unhealthy.

## Data-processing path (source data → derived data)

Aether Data Processing introduces a third, non-authoritative path for
request-driven computation. It is neither an uplink mirror nor a downlink
command path:

```text
caller
  │ typed data-processing task
  ▼
DataProcessingApplication
  ├─ HistoryQuery ───────────── historical observations
  ├─ LiveState ──────────────── current read-only tail
  └─ task/request context ───── future or external covariates
             │
             ▼ complete, bounded ProcessingFrame
         DataProcessor
             │
             ▼ schema-validated, expiring ProcessingResult
       authenticated HTTP DerivedData response
```

The application resolves semantic bindings, aligns and aggregates timestamps,
requires commissioned unit/sign metadata to match exactly, checks missing and
stale inputs, and sends the values in the processor request. The runtime performs
no runtime unit/sign conversion.
The processor never receives credentials for SHM, SQLite, or internal service
APIs and never resolves a site identifier by reaching back into Aether.

The result records its input watermark, input digest, processor
provenance, quality, status, and expiry. It is derived evidence rather than a
measurement: it is not written to the IO-owned T/S segment. If automation uses
the result, a separate planning/control use case validates freshness and safety
before the existing audited command path can act. Processor loss therefore
removes an optional advisory input without interrupting acquisition or local
safety rules. See [Data Processing Flow](data-processing-flow.md) for the
complete contract.

An event-time `as_of` is not by itself a historical knowledge cut. The current
historian has no ingestion/source epoch and artifact provenance has no
training/availability cut, so point-in-time evaluation uses frozen history and
artifact inputs rather than querying today's mutable sources for an old frame.

## Latency budget

The microsecond figures are historical measurements on production hardware
(Cortex-A55 @ 1.4 GHz, ECU-1170 / EdgeLinux 22.04) recorded in the README and
CHANGELOG. The nanosecond figure is the README's stated order of magnitude for
the hot-path write; release qualification must rerun current stress gates.

| Stage | Latency | Source label |
|-------|---------|--------------|
| aether-io shared-memory write (`set_direct`) | ~10 ns/point | README |
| Data change → aether-automation event received (PointWatch delivery) | P50 206 µs, P99 526 µs | README/CHANGELOG, measured |
| + rule evaluation + control SHM write + UDS notify to aether-io | ~215 µs P50, ~540 µs P99 (cumulative) | README, measured |
| + device protocol write (Modbus / IEC 104 field bus) | +5–10 ms | README |
| aether-alarm → aether-api/aether-uplink, service HTTP hops | local HTTP | — |

The CHANGELOG also records P99.9 at 1.4–2.2 ms for the event path, and notes
that PointWatch replaced the previous 100 ms Redis-tick polling model
(50–150 ms end to end) — roughly a 500× improvement on the critical path. The
software-internal control path is sub-millisecond; the field-bus write
dominates the physical control loop.

## Optional state mirrors

A downstream state mirror is not a participant in the control path. A custom
composition may implement the `StateMirror` port and publish an eventually
consistent remote view, but no kernel service reads that mirror and its
failure cannot affect acquisition, rules, alarms, history, API reads, uplink,
or command delivery.

Custom stores stay outside this repository, consume read-only state through
published contracts, remain non-authoritative, and cannot become core service
startup dependencies.

## Related pages

- [Architecture](architecture.md) — the services these paths connect
- [Shared Memory](shared-memory.md) — slot layout, seqlock, write ownership
- [Data Model](data-model.md) — points, instances, and NaN/absence semantics
- [Data Processing](data-processing.md) — the optional industry-neutral processing boundary
- [Data Processing Flow](data-processing-flow.md) — processor-request data flow and failure semantics
- [CloudLink MQTT](../reference/cloudlink-mqtt.md) — application-ACK/replay edge path
- [Rule Engine](rule-engine.md) — what happens after a PointWatch event arrives
