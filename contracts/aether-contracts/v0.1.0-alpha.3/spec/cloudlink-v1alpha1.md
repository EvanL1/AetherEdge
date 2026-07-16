---
id: cloudlink-v1alpha1
status: experimental-auth-proposal
version: 0.1.0-alpha.3
normative: true
---

# CloudLink v1 alpha 1

AetherContracts is the sole interoperability authority for this protocol
slice. Product-repository wire profiles, manifests, gates, and evidence files
are non-authoritative implementation overlays and may not add or redefine wire
fields. The historical joint-core provenance records only where alpha.2 input
bytes came from; it grants no continuing joint ownership.

Alpha.3 freezes the closed envelope and time/identity fields, session
challenge/hello/acceptance, heartbeat, Runtime Manifest report, telemetry
batch, data-loss evidence, replay request, and an unsigned durable application
ACK. TypeScript, Rust, C99, and C++17 execute the same public fixture manifest
and stable failure strings. Those fixture surfaces are experimental and are not
complete production transport codecs.

The authentication profile distinguishes two origin models. In
`gateway-signed`, Cloud issues a one-time signed challenge, the Gateway signs
the exact session-establishment object, and every uplink signs the exact
per-uplink object. In `trusted-connector-broker-attestation`, a configured
trusted ingress adapter supplies origin evidence outside the payload and binds
the exact received MQTT payload bytes. A payload cannot attest to itself.
Topic names, payload identity, and MQTT credentials alone are never Gateway
authentication.

The proposal specifies the Ed25519 algorithm, unpadded-base64url encoding, RFC
8785 JCS signing objects, absent-value rules, and replay bounds exactly in
`profiles/cloudlink/v1alpha1/authentication.json`. Production key provisioning,
rotation, revocation, verifier ownership, and production signature verification
remain planned, so the authentication gate remains a proposal and cutover is
blocked.
Ordinary logs and public evidence must exclude signatures, nonces, credential
identifiers, and raw authentication transcripts.

The alpha.3 durable-ACK JSON shape is explicitly unsigned. Success means the
application fact and receipt were durably committed before ACK publication,
but alpha.3 contains no production store/outbox implementation and makes no
crash-durability claim. A future signed ACK is a separate command/profile and
requires a Cloud key lifecycle plus production restart evidence. MQTT PUBACK is
never an application durable receipt.

Repeated delivery with the same replay identity and digest is idempotent. Reuse
of an identity with a different digest is wire-valid but context-invalid; it is
quarantined as `DIGEST_CONFLICT` and receives no successful receipt. Data loss
is explicit evidence and never causes Cloud to fabricate samples.

Data-loss evidence satisfies
`first_lost_position <= last_lost_position < earliest_retained_position`.
Heartbeat and resume cursor arrays contain at most one entry for each
`(stream_id, stream_epoch)`. Violations are context-invalid, do not change a
business fact, and do not permit a successful application receipt.

The only durable position identity is
`(gateway_id, stream_id, stream_epoch, position)`. `batch_id` and `digest` are
stable bindings of that position, not fields that create a second identity;
changing either cannot bypass conflict detection. A business digest is the
lowercase SHA-256 of RFC 8785 JCS over exactly
`{protocol_version,message_kind,payload}`. It provides content integrity and
replay comparison, not publisher authentication. The machine-readable form is
`profiles/cloudlink/v1alpha1/core.json`.

`expires_at_ms` is optional; when omitted, that field imposes no message
deadline. When present, it must be greater than or equal to `sent_at_ms`, or
the message is context-invalid with `INVALID_EXPIRY_WINDOW`. Expiration is
evaluated against an explicit canonical uint64 `evaluation_time_ms`: the check
passes only while `evaluation_time_ms < expires_at_ms`, and equality is already
expired with `MESSAGE_EXPIRED`. The portable TCK supplies this evaluation time
as scenario input and never consults an ambient wall clock. Both expiry
failures leave business state unchanged and forbid a successful application
receipt.

The embedded Runtime Manifest checksum is lowercase SHA-256 over RFC 8785 JCS
of the complete manifest object with its top-level `checksum` member omitted.
Its digest omits the `sha256:` prefix because the enclosing checksum object
already declares the algorithm.

`envelope.schema.json` is the reusable structural base. Consumers
must validate an uplink with its message-kind entry Schema
(`runtime-manifest-report`, `telemetry-batch`, or `data-loss`); using the base
alone does not validate the discriminator-to-payload relationship.

The alpha telemetry slice currently carries only finite JSON-number values
for telemetry/status points. Non-numeric Thing Model value types, events, and
the topology-to-model point-resolution contract are planned, not silently
inferred. An optional sample `model` value is only a commissioning hint and is
not sufficient mapping authority.

This slice does not freeze exact signed `int64`, `uint64`, decimal, byte, or
string sample encodings. A JSON number is not a substitute for an exact 64-bit
integer contract.

Legacy transport remains the default. CloudLink contains no physical-control,
direct SHM, or direct register operation.
