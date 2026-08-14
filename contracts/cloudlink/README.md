# CloudLink contracts

This directory is the only AetherEdge CloudLink contract surface. Schema IDs,
file paths, MQTT topics, session establishment, delivery envelopes, ACKs, and
replay requests have one canonical form and expose no protocol negotiation or
compatibility mode.

The MQTT root is `{prefix}/gateways/{gateway_id}`. QoS 1 PUBACK is transport
evidence only; a record leaves the durable Edge spool only after its matching
CloudLink application ACK.

Alarm state transitions use the single `alarm-event` business kind and
`up/alarm` MQTT route. Their `event_id` is the durable batch identity. Aggregate
alarm counts and operator-requested state replays are reconstructable state and
do not enter this transition stream. Edge reserves a protected identity receipt
before accepting each alarm and retains it after Cloud ACK; other lossless
CloudLink kinds discard their local identity receipt after ACK.

The immutable AetherContracts release under `contracts/aether-contracts/` is
retained solely as provenance. Its release-scoped names are not accepted as
alternate AetherEdge wire identifiers. The pinned release-manifest digest is
recorded in `contract-manifest.json`.

Validate a fixture with an explicit local base URI:

```bash
cd contracts/cloudlink
uvx check-jsonschema \
  --base-uri "file://$PWD/" \
  --schemafile telemetry-batch.schema.json \
  fixtures/telemetry-batch.valid.json
```
