# Experimental CloudLink MQTT v1 edge contract

## Status and scope

This document describes the experimental CloudLink candidate. The public
AetherContracts `v0.1.0-alpha.3` release is the sole authority, and AetherEdge and
AetherCloud commit the same digest-pinned consumer lock. The current claim is
complete distribution integrity, full fixture execution, and an opt-in real
dual-product alpha harness, not production interoperability conformance.
The public AetherContracts release and compatibility guide define the shared
protocol; this page only explains the AetherEdge integration.

The lock imports the exact alpha.3 adoption closure. Strict Runtime Manifest
SemVer and replay/digest/cursor context vectors execute in both products. Local wire,
authentication, gate, scenario, and fixture-manifest files are migration or
product integration material and cannot override AetherContracts.

Implemented by this slice:

- strict transport-neutral JSON values and canonical business digests;
- session/version/epoch validation;
- Runtime Manifest and real `PointSample` business mappings;
- memory and crash-recoverable file spools removed only by application ACK;
- MQTT v3.1.1/QoS 1 binding for a user-selected broker;
- a default-off `aether-io` Home Assistant composition that uses the real MQTT
  transport, independent file spools, restart replay, and application-ACK
  writeback after an explicit Runtime Manifest and cloud-consumer gate;
- deterministic fake-transport tests, an Edge-only Broker harness, and an
  opt-in real Mosquitto AetherEdge/AetherCloud dual harness.

Present in the imported experimental subset:

- every wire field, topic, receipt, and replay/data-loss exchange;
- the exact alpha.3 challenge, Gateway-signature, and trusted-connector origin
  proposal shapes; production key lifecycle remains unresolved;
- edge-native telemetry without a mandatory Thing Model revision.

The incompatible repository-local AetherEdge and AetherCloud vocabularies are
superseded by public alpha.3; migration findings are retained in
`contracts/cloudlink/v1/MIGRATION.md`.

Planned outside this slice:

- production AetherCloud authentication and durable stores;
- production enrollment/CA/KMS lifecycle;
- MQTT 5 enhancements and private-broker bridge/site connector;
- jointly published ACL templates and production enablement;
- alarms and operational telemetry on dedicated CloudLink streams;
- an explicit expiry-to-data-loss lifecycle for records that expire before a
  cloud receipt; expired content already fails closed and is never offered.

Deprecated but retained:

- the unversioned `property/{productSN}/{deviceSN}` and related legacy namespace;
- removal after MQTT client acceptance through the generic outbox;
- legacy MQTT device-control topics.

CloudLink v1 exposes no physical control or arbitrary RPC.

## Experimental Integration extension

`aether.cloudlink.integration.v1alpha1` carries Integration topology snapshots
and observation batches over CloudLink. It is experimental and disabled by
default. Activation is Cloud-first: AetherCloud must explicitly enable a
compatible consumer before AetherEdge enables publication, and the current
Runtime Manifest must explicitly declare the exact extension identifier. If
either condition is absent, the Edge rejects activation. Enabling an Integration
source by itself does not enable Cloud publication.

The extension has two independent durable streams and MQTT routes:

- topology snapshots use `up/integration/topology`;
- observation batches use `up/integration/observations`.

A topology snapshot is one atomic delivery record and is never fragmented. If
its complete encoded MQTT payload exceeds 256 KiB, publication fails closed.
An observation batch is retained unchanged when it fits; otherwise it is split
only at observation boundaries, so an individual observation is never divided.
Every resulting batch is independently valid, and every complete MQTT payload,
including the CloudLink delivery envelope, must fit within 256 KiB.

Both streams reuse the existing application-level durable ACK, replay, stable
delivery identity, and digest rules. MQTT PUBACK remains transport evidence and
cannot remove a record. The extension carries read-only Integration projection
facts and provides no command, arbitrary RPC, or physical-control capability.

## Compatibility matrix

| Concern | AetherEdge before this slice | AetherCloud reference | Candidate resolution |
|---|---|---|---|
| Wire package | Unversioned legacy JSON | Strict TypeScript codec/ingress | Byte-identical alpha.3 schemas/fixtures and matching Rust/TypeScript vocabulary |
| Delivery removal | After local `AsyncClient::publish` acceptance | Durable application ACK required; memory foundations only | Dedicated spool removes only after validated durable ACK |
| Session | No CloudLink session/epoch | Domain/application memory implementation | Candidate hello/accepted and monotonic epoch binding |
| Authentication | MQTT username/password or mTLS; product/device topic identity | Session verifier consumes alpha structural evidence | Hello carries the exact challenge and Gateway signature object, or trusted adapter metadata is required outside payload; production key lifecycle remains proposed |
| Resume | Broker reconnect only | Server cursor intended as authority | Server cursor drives stable identity/digest replay |
| Telemetry identity | Legacy logical map loses timestamp/quality/address | Cloud codec now accepts the Edge fields and optional model | Preserve edge `PointAddress`, timestamp, quality and batch position without fabricating a model; Cloud multi-sample internal indexing remains open |
| Topology | Coherent SHM publication epoch and snapshot digest | No equivalent batch field | Carry publication epoch and topology digest per batch |
| Manifest | Closed v1, JCS SHA-256, implemented | Matching runtime-manifest domain shape | Embed the exact verified manifest and checksum |
| Broker | Configurable legacy endpoint | Alpha MQTT ingress implemented | User-selected shared MQTT v3.1.1 broker is exercised by the dual harness; production authentication remains blocked |
| Control | Legacy write/call topics reach governed application boundary | CloudLink commands planned separately | No CloudLink v1 command topic or payload |

## Topic policy

All candidate publications and subscriptions use QoS 1 and `retain = false`.
The prefix is configurable. Prefix
and gateway segments reject empty values, `+`, `#`, NUL/control characters, and
path traversal-like empty segments. A gateway receives only its own
`down/session`, `down/ack`, and `down/replay` topics.

The gateway ID is not authorization evidence. A Cloud ingress must compare it to
verified publisher identity. With a generic shared broker, a separate Cloud
subscriber normally cannot see the original publisher's authenticated Broker
principal. Alpha.3 freezes experimental challenge/session/uplink signing
objects. Production generic Broker mode still requires key provisioning,
rotation, revocation, and verifier ownership. The alternate origin model is
configured trusted-adapter evidence outside the payload for every publish.

## Time, integers, bounds, and digest

- Protocol `uint64` values are canonical decimal strings: `0` or a non-zero digit
  followed by digits, with no sign, whitespace, exponent, or leading zero.
- Protocol timestamps are Unix milliseconds encoded as canonical decimal strings.
  The embedded Runtime Manifest retains its existing field formats unchanged.
- One encoded message is at most 256 KiB and one point batch contains at most 256
  samples.
- Identifiers and metadata are length bounded; unknown object fields fail closed.
- Delivery digest is `sha256:<64 lowercase hex>` over RFC 8785 canonical JSON of
  the versioned business content only. Session data, trace context, retry counts,
  MQTT packet identifiers/properties, and transport timestamps are excluded.

Equal `(gateway_id, stream_id, stream_epoch, position)` with the same stable
`batch_id` and digest binding is a replay. Changing either binding at that
identity is a security conflict.

## Spool and resume behavior

The spool persists stream epoch, next position, records, delivery state, the last
durable ACK, and data-loss evidence. Client or broker delivery cannot remove a
record. A durable ACK validates all of:

- current session ID and session epoch;
- stream ID and stream epoch;
- contiguous acknowledged position;
- terminal batch identity and digest;
- non-empty receipt identity.

Duplicate ACKs return the prior idempotent result. Older sessions, positions past a
gap, wrong batch/digest values, and conflicts fail closed. Reconnect offers the
same stored content under the new session envelope; it never allocates another
position, batch ID, or digest.

Configured capacity is bounded to 1–65,536 retained records; each record payload
is independently bounded to 256 KiB.

At capacity, the adapter records the exact evicted position range. If the cloud's
authoritative cursor requests an unavailable position, the edge sends that
evidence and resumes at the earliest retained record only after the cloud handles
the gap. A torn final journal record is truncated during recovery. Corruption
in any complete journal mutation prevents opening the spool. The file adapter
uses payload-once incremental mutations and atomically compacts live records and
cursor metadata before accepting work after 256 mutations.

## Telemetry mapping

| Edge value | Candidate field | Notes |
|---|---|---|
| `PointAddress::instance_id` | `instance_id` | Canonical decimal identity |
| `PointAddress::kind` | `point_kind` | `telemetry`, `status`, `command`, or `action`; business collection uses acquisition-owned kinds only |
| `PointAddress::point_id` | `point_id` | Canonical decimal identity |
| `PointSample::value` | `value` | Must be finite |
| `PointSample::timestamp` | `source_timestamp_ms` | Source Unix milliseconds |
| `PointSample::quality` | `quality` | Preserved by the contract |
| SHM publication epoch | `topology.publication_epoch` | Coherent point/health generation witness |
| topology snapshot digest | `topology.snapshot_digest` | Identifies the exact published routing snapshot |

The current SHM slot does not encode acquisition quality, so its read adapter
reports accepted finite values as `good`. This is an implementation limitation,
not proof that the source supplied `good` quality.

No Thing Model revision is fabricated. The optional `model` binding is accepted
only when it originates from commissioned, verified configuration. Cloud
enrichment can map edge point addresses after ingestion.

## Runtime Manifest mapping

The report embeds the exact result of the current Runtime Manifest generator. Its
`checksum.algorithm` remains `sha256`; its digest is computed over RFC 8785
canonical JSON of every manifest field except `checksum`. CloudLink validates and
transports that checksum rather than inventing another composition model.

## Shared-broker harnesses

The Edge-only integration is disabled unless explicitly enabled. It requires an
MQTT v3.1.1 broker and deliberately uses a fake Cloud peer, so it is not joint
interop evidence:

```bash
AETHER_CLOUDLINK_RUN_INTEGRATION=1 \
AETHER_CLOUDLINK_BROKER_HOST=127.0.0.1 \
AETHER_CLOUDLINK_BROKER_PORT=1883 \
cargo test -p aether-cloudlink-mqtt --test shared_broker -- --nocapture
```

Optional credentials use `AETHER_CLOUDLINK_BROKER_USERNAME` and
`AETHER_CLOUDLINK_BROKER_PASSWORD`. TLS uses
`AETHER_CLOUDLINK_BROKER_CA`, with optional client certificate/key variables.
Tests and errors never print credential values.

The final alpha evidence uses real Mosquitto, this repository's rumqttc
transport and `FileCloudLinkSpool`, plus AetherCloud's real MQTT adapter and
application use cases. Run it from AetherCloud:

```bash
pnpm test:cloudlink-dual
```

It writes `AetherCloud/evidence/cloudlink-alpha3-dual-harness.json` and a
compatibility copy under `artifacts/cloudlink-alpha/evidence.json`, including
the fault matrix. The harness implements Broker reconnect, ACK loss, a second
Edge process recovering the file spool, the Cloud-owned `manifest/1/1` resume
cursor, a second Cloud ingress process, duplicate/idempotent replay, conflict,
expiry, out-of-order, a non-durable partial outcome, and explicit data loss.
Telemetry still replays after its lost ACK. PostgreSQL process-crash durability
remains blocked, and the Cloud restart result is honestly `unknown-reaccepted`.
