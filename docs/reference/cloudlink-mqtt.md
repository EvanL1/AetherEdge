# CloudLink MQTT contract

CloudLink has one AetherEdge wire contract. There is no migration mode,
protocol negotiation, fallback topic tree, or alternate schema alias.

## MQTT binding

- MQTT 3.1.1, QoS 1, non-retained messages.
- Canonical root: `{prefix}/gateways/{gateway_id}`.
- Edge publishes session, heartbeat, manifest, telemetry, alarm transitions,
  Integration topology, Integration observations, and data-loss messages below
  `up/`.
- Edge subscribes only to the same Gateway's session, ACK, and replay messages
  below `down/`.
- Production transport requires TLS. Topic prefix and Gateway ID are validated
  before any broker connection is created.

MQTT PUBACK proves only broker receipt. A record leaves the bounded file spool
after a matching CloudLink application ACK binds its session, stream epoch,
position, batch identity, digest, and receipt identity.

Every CloudLink business fact emitted by this service uses non-evicting pending
admission. Alarm transitions also reserve a persistent post-ACK identity
receipt so an Alarm HTTP retry after a lost response returns the original
stream position instead of creating another Cloud event. Runtime Manifest,
telemetry, Integration, and data-loss records do not retain post-ACK receipts
and therefore cannot exhaust the alarm ledger. When the protected alarm ledger
is full, new alarm admission and Uplink readiness fail closed while ordinary
CloudLink ACK progress continues.

## Wire identity

Every message uses an unversioned `aether.cloudlink.*` schema identifier. The
wire carries no protocol-version offer or selection. Business digests are
SHA-256 over RFC 8785 canonical JSON containing exactly `message_kind` and
`payload`; session, retry, MQTT, trace, and authentication metadata are outside
that digest.

Session epochs and stream positions remain monotonic. Unknown schemas, unknown
fields, unsafe integers, stale sessions, digest mismatches, and retired local
file formats fail closed. CloudLink exposes no physical-control topic.

## Contract files and provenance

The canonical schemas and fixtures are in [`contracts/cloudlink/`](../../contracts/cloudlink/).
The immutable AetherContracts release under `contracts/aether-contracts/` is
retained as provenance and verified by `aether-contracts.lock.json`. Its
release-scoped paths are not selectable AetherEdge interfaces and are never
used as fallback wire identifiers.

The broker-independent codec lives in `crates/aether-cloudlink`; the MQTT
binding lives under `services/uplink/adapters/cloudlink-mqtt`. Default tests do
not require a broker. The opt-in shared-broker tests use
`AETHER_CLOUDLINK_BROKER_HOST`, `AETHER_CLOUDLINK_BROKER_PORT`,
`AETHER_CLOUDLINK_GATEWAY_ID`, and `AETHER_CLOUDLINK_TOPIC_PREFIX`.
