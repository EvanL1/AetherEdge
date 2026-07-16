# ADR-0017: Experimental CloudLink MQTT edge foundation

## Status

Accepted for an **experimental edge implementation** on 2026-07-15 and amended
the same day for initial repository coordination. ADR-0018 supersedes the
co-authority language with one pinned public AetherContracts authority.

Both products now pin AetherContracts `v0.1.0-alpha.3` and consume the same
complete alpha.3 adoption closure with no pending imports. This proves
distribution integrity and fixture execution. A real Mosquitto dual-product
alpha run now exists, but it is not production conformance. The legacy MQTT adapter remains the default
until the exit criteria in this ADR are met.

## Context

`aether-uplink` currently publishes unversioned `property`, `status`, `write`,
`call-data`, `call-alarm`, and `inst-sync` topics scoped by `productSN/deviceSN`.
It places messages in `FileOutbox`, calls `rumqttc::AsyncClient::publish`, and
removes the entry when that client call succeeds. That boundary means only that
the local MQTT request queue accepted the publish. It is neither MQTT PUBACK nor
proof that a cloud application durably stored the business fact.

That behavior is an established compatibility contract for the generic
`DurableOutbox` and `UplinkPublisher` ports. Changing it in place would silently
alter other adapters and existing installations.

AetherCloud ADR-0006 and ADR-0007 describe durable CloudLink sessions and IoT
telemetry. At the initial clean `d731ecb` audit, the reference repository had
domain/application and memory implementations only, no executable CloudLink wire
package or MQTT ingress, and required a Thing Model revision for every point.
During this implementation its uncommitted working tree gained a separately
developed experimental `1.0-cloud.1` codec/ingress and optional edge-native model
mapping. Its incompatible vocabulary was subsequently replaced by the public
alpha.3 contract and product codec. Production credential verification and
durable session/telemetry stores still do not exist.
AetherIot's authoritative `PointSample` owns an
instance ID, point kind, point ID, finite numeric value, source timestamp, and
quality, but does not own a Thing Model revision. Inventing that revision at the
edge would create a false business fact.

Customers may already operate an MQTT broker. Requiring an AetherCloud-owned
broker would make the broker a product dependency rather than a transport choice.

## Decision

### Protocol and transport boundary

CloudLink is a transport-neutral application protocol. MQTT is its first binding;
an in-memory binding is used for deterministic tests and future bindings may be
added without changing stream identity or acknowledgement semantics.

MQTT v3.1.1, QoS 1, bounded packets, explicit topic ACLs, and application-level
durable acknowledgements are the compatibility baseline. MQTT 5 message expiry,
response topics, correlation data, receive maximum, user properties, and session
expiry may be negotiated later. Correctness cannot depend on them.

The MQTT endpoint, authentication, TLS roots/client identity, and topic prefix are
operator configuration. A customer-selected shared broker is supported directly.
An AetherCloud-managed broker is optional, not required. A private broker that
cannot be reached by AetherCloud will need a future customer-controlled bridge or
site connector; that connector is not part of this slice.

### Versioned topics and ACLs

The candidate binding uses these non-retained QoS 1 topics:

```text
{prefix}/v1/gateways/{gatewayId}/up/session
{prefix}/v1/gateways/{gatewayId}/up/heartbeat
{prefix}/v1/gateways/{gatewayId}/up/manifest
{prefix}/v1/gateways/{gatewayId}/up/telemetry
{prefix}/v1/gateways/{gatewayId}/up/data-loss

{prefix}/v1/gateways/{gatewayId}/down/session
{prefix}/v1/gateways/{gatewayId}/down/ack
{prefix}/v1/gateways/{gatewayId}/down/replay
```

Prefix and gateway segments are validated at runtime and cannot contain MQTT
wildcards, empty segments, or control characters. They are commissioned routing
identifiers; compositions must never derive them from tenant display names,
arbitrary user input, or credentials. A gateway publisher/subscriber is granted
only its own namespace. The gateway ID in a topic or payload is a routing claim;
the cloud must bind it to the authenticated connection before trusting it.

Telemetry, acknowledgements, and physical commands are never retained. LWT/status
may be added as a separate operational observation, but it never proves device or
plant health.

### Session and credential binding

Session establishment negotiates a `major.minor` protocol version and binds one
verified gateway identity plus credential generation to a random session ID and
a monotonic session epoch. The candidate hello carries a credential identifier,
generation, origin model, challenge ID, Gateway key ID, and structurally valid
Ed25519 signature object. It is a frozen alpha.3 proposal, not a production
transcript. Generic shared-Broker mode requires the specified replay-bounded
establishment signature and a session-bound Gateway signature on every later
uplink. Trusted out-of-band Broker principal attestation for every
delivered publish is the reviewed alternative. Raw enrollment tokens, private
keys, and passwords are never sent, and proof material is never logged.

An old session epoch cannot acknowledge or advance a new session. The server's
durable cursor is authoritative during resume; a client cursor is only a claim.
Exact signing projections are public; production verification, rotation,
revocation, provisioning, and verifier ownership remain planned.

The development verifier and fixtures use non-secret identifiers only. They are
not a CA, KMS, enrollment service, or production credential lifecycle.

### Dedicated CloudLink spool

CloudLink uses a new capability-oriented spool rather than changing generic
`DurableOutbox` semantics. Its record lifecycle is:

```text
queued -> offered -> transport-published -> cloud-durably-acknowledged -> removable
```

Stream epoch and next position survive process restart. Batch identity and the
canonical SHA-256 business-content digest are allocated once and reused on every
replay. Transport retry counts, trace context, and MQTT properties are outside the
business digest. Equal identity plus equal digest is a replay; equal identity plus
a different digest is a conflict and fails closed.

MQTT client acceptance and PUBACK are transport evidence only. A record becomes
removable solely after a CloudLink durable ACK validates session, stream, stream
epoch, contiguous position, batch identity, and digest. Duplicate ACKs are
idempotent. Stale or conflicting ACKs are rejected.

The spool is bounded. If capacity policy discards an earliest unacknowledged
record, it durably records the exact lost position range and earliest retained
cursor. A replay request below that cursor produces explicit data-loss evidence;
the edge never fabricates missing telemetry. Torn journal tails are recoverable;
corruption before a valid later record fails closed. Stream epoch rotation is
explicit and cannot silently discard pending records.

This slice validates optional expiry and refuses to offer expired content. The
separate expiry-to-data-loss state transition remains planned; until it exists,
an expired record stays durable and can contribute to a later capacity-overflow
loss range rather than being silently deleted.

### Business telemetry and Runtime Manifest

Business point telemetry contains only facts the edge owns: `InstanceId`,
`PointKind`, `PointId`, finite `f64`, source timestamp, and `PointQuality`, plus the
coherent SHM topology publication epoch and snapshot digest. An optional semantic
binding may be added only when commissioning supplies a real, verified model
reference. Cloud-side enrichment is preferred to an invented Thing Model revision.

The current SHM slot encoding preserves value, raw value, and timestamp but does
not preserve acquisition quality. Its `LiveState` adapter therefore exposes
accepted finite SHM values as `good`. The candidate contract preserves quality as
a field, but this implementation limitation must not be represented as original
device-quality fidelity.

Runtime Manifest reports embed the existing closed v1 manifest and its canonical
RFC 8785 SHA-256 checksum. CloudLink does not create a second manifest model.

Point telemetry, alarm facts, runtime/connection health, acquisition-path
operational telemetry, and OpenTelemetry traces/metrics remain distinct classes.
CloudLink point batches are not OpenTelemetry metrics, and operational telemetry
does not enter point history.

### Migration

Runtime mode is explicit:

- `legacy`: current adapter and topics; deprecated, but the compatibility default;
- `cloudlink-v1`: experimental CloudLink topics and application ACK semantics;
- `dual`: both namespaces during measured migration.

Dual mode never derives a second CloudLink identity for a fact merely because the
legacy path also publishes it. Legacy `write`, `call-data`, and other control
topics are not translated into CloudLink. CloudLink v1 contains no arbitrary RPC,
SHM write, register write, point write, or physical device-control capability.

New installations may default to `cloudlink-v1` only after all of the following:

1. both repositories close every pending AetherContracts import and pass the
   complete shared TCK;
2. both repositories pass a real broker interop suite;
3. a production credential lifecycle and durable cloud stores are composed;
4. upgrade and rollback behavior has field evidence; and
5. operator documentation and ACL templates are released.

Legacy code and topics may be removed only after the supported upgrade window has
elapsed, telemetry consumers have migrated, no supported installation requires the
old command path, and a separate ADR records the removal. Until then, `legacy` is
deprecated rather than deleted.

### Failure isolation

CloudLink is never in acquisition, automation, alarm, safety-interlock, or local
control loops. Broker or cloud unavailability grows/replays the bounded spool and
may emit data-loss evidence, but cannot stop SHM publication, rules, alarms,
history, safety behavior, or local control.

## Joint coordination remaining

Release distribution, full fixture execution, and the opt-in real Broker alpha
harness are complete. Before this candidate can become a production protocol,
the repositories must still:

1. implement production per-uplink origin verification and key lifecycle;
2. align Cloud's internal per-record telemetry indexing with the frozen
   one-position-per-batch delivery model without inventing positions;
3. implement trusted per-publish Broker attestation where that origin model is used;
4. persist session epochs, stream cursors, batch identity/digest receipts,
   manifests, telemetry, and conflicts in production stores; and
5. return durable ACKs only after the business transaction commits.

These are production compatibility findings; the bounded alpha integration is
machine-evidenced, but it does not close these gates.

## Consequences

- Existing installations keep their current behavior unless an operator opts in.
- CloudLink replay and deletion semantics no longer depend on MQTT PUBACK.
- The local runtime still requires no broker, cloud process, Redis, PostgreSQL,
  Docker, or account for its default tests and deterministic behavior.
- The first real-broker harness is opt-in and can use any MQTT v3.1.1 broker.
- More local disk and implementation complexity are accepted in exchange for
  durable application acknowledgement and explicit loss evidence.
- Physical control is deliberately absent and would require a separate safety ADR,
  capability contract, authorization policy, confirmation policy, and audit path.
