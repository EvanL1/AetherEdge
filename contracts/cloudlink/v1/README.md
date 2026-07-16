# CloudLink MQTT v1 AetherEdge implementation overlay

This directory consumes AetherContracts `v0.1.0-alpha.3`, the sole CloudLink
wire authority. Imported files are exact release bytes. Local manifests,
profiles, gates, scenarios, and migration notes are non-authoritative AetherEdge
implementation/readiness/evidence overlays and cannot redefine the public
contract. Complete distribution adoption is not production conformance.

CloudLink is the transport-neutral application protocol. MQTT 3.1.1 with QoS
1 and non-retained messages is its first binding. MQTT PUBACK never proves
application durability. A delivery is removable at the Edge only after a
matching `durable-ack` binds the verified session, stream epoch, batch
position, batch identity, business digest, and durable receipt.

## Implemented alpha.3 rules

- JSON is strict and closed with `additionalProperties: false`.
- Protocol `uint64` and Unix millisecond times are canonical decimal strings.
- Gateway and session identities are canonical lowercase UUIDs.
- One durable stream position identifies one business batch, not one sample.
- The business digest is `sha256:` plus the lowercase SHA-256 of RFC 8785 JCS
  over `{protocol_version,message_kind,payload}` in that field vocabulary.
- Session, transport retry, MQTT properties, trace context, and replay attempt
  metadata are outside the business digest.
- The exact proposal profile distinguishes Gateway signatures from
  trusted-adapter external origin evidence. Production key provisioning,
  rotation, revocation, and verifier ownership remain planned, so this is not
  a production authentication profile. Proof material
  must never be logged.
- Telemetry carries Edge-owned PointAddress, finite `f64` value, source time,
  quality, and topology evidence. Optional model binding may be supplied only
  when commissioning actually established it.
- V1 contains no RPC, Point/Register write, retained command, or physical
  control message.

`fixture-manifest.json` and the root complete-consumer lock pin every required
alpha.3 artifact with `pending_imports: []`. Product tests execute the public
fixtures, but do not establish production credentials, Cloud crash durability,
signed ACK, or cutover readiness.

`wire-profile.json` and the interoperability files are product overlays only.
The public replay identity is `(gateway_id, stream_id, stream_epoch, position)`;
`batch_id` and `digest` are stable bindings. Alpha.3 durable application ACKs
are unsigned. `session-authentication.schema.json` is retained only as
superseded migration history and is never contract authority.
