---
id: cloudlink-v1alpha1
status: experimental-auth-proposal
version: 0.1.0-alpha.4
normative: true
---

# CloudLink v1 alpha 1

AetherContracts is the sole interoperability authority for this protocol
slice. Product-repository wire profiles, manifests, gates, and evidence files
are non-authoritative implementation overlays and may not add or redefine wire
fields. The historical joint-core provenance records only where alpha.2 input
bytes came from; it grants no continuing joint ownership.

Alpha.3 freezes the closed core envelope and time/identity fields, session
challenge/hello/acceptance, heartbeat, Runtime Manifest report, telemetry
batch, data-loss evidence, replay request, and an unsigned durable application
ACK. TypeScript, Rust, C99, and C++17 execute the same public fixture manifest
and stable failure strings. Those fixture surfaces are experimental and are not
complete production transport codecs.

Alpha.4 adds an explicitly activated Integration extension without making its
message kinds part of the alpha.3 core entry Schema. The extension reuses the
same `aether.cloudlink` 1.0 session, uplink fields, authentication projection,
business digest, durable position, replay, data-loss, and durable-ACK
semantics. Its entry Schemas reference the reusable
`envelope.schema.json#/$defs/uplinkEnvelope` shape. Validating an Integration
delivery with the closed core `envelope.schema.json` is intentionally
insufficient and fails; consumers select the message-kind-specific extension
entry Schema.

The authentication profile distinguishes two origin models. In
`gateway-signed`, the Gateway first publishes
`aether.cloudlink.session-challenge-request.v1` on the existing
`{prefix}/v1/gateways/{gatewayId}/up/session` topic. The request is a trigger,
not an identity credential. Cloud may issue a challenge only after resolving
the claimed `gateway_id`, `credential_binding.credential_id`, and
`credential_binding.generation` to an active commissioned Gateway delegation,
and it must rate-limit challenge requests. An ineligible or limited request
causes no challenge to be issued. The authentication proposal remains disabled
by default and requires explicit activation.

The challenge-request object is closed and contains exactly `schema`,
`protocol`, `message_kind`, `gateway_id`, `credential_binding`,
`offered_protocol_versions`, `client_nonce`, and `resume`.
`credential_binding` contains exactly `credential_id` and `generation`; it
does not select an origin model or carry authentication evidence. The
`offered_protocol_versions`, `client_nonce`, and `resume` values are repeated
unchanged in the later session-establishment signing projection.

Cloud persists the complete signed challenge before publishing it on the
existing `down/session` topic. A repeated eligible request while that
challenge remains unexpired republishes the exact persisted challenge,
including identical bytes, challenge identifier, nonce, issue time, expiry,
key identifier, and signature. It must not regenerate or silently extend the
challenge. After the Gateway validates the challenge, it publishes
`session-hello`. Cloud verifies the active credential generation, Gateway key,
signature, exact persisted challenge, and strict deadline before accepting a
session. Durable session acceptance and one-time challenge consumption are one
atomic state transition: neither may commit without the other, a failed
attempt does not consume the challenge, and a consumed challenge cannot
establish another session.

The Cloud challenge signing object maps its `schema` to the literal
`aether.cloudlink.session-challenge-signing.v1alpha1`; every remaining member
maps one-to-one from the same-named `session-challenge` member. The Gateway
session-establishment signing object maps `credential_id` and
`credential_generation` from
`session-hello.credential_binding.credential_id` and `.generation`,
`cloud_nonce` from the exact persisted challenge, and all remaining members
from the same-named `session-hello` member. Both projections are encoded as
RFC 8785 JCS UTF-8 bytes before Ed25519 signing. The ordered field lists and
complete language-neutral source mapping are normative in
`profiles/cloudlink/v1alpha1/authentication.json`.

Challenge validity uses an explicit canonical `evaluation_time_ms` and passes
only while `evaluation_time_ms < expires_at_ms`. Equality is expired and fails
as `MESSAGE_EXPIRED` without consuming the challenge or accepting a session.

The alpha.4 `session-accepted` response is unsigned and contains neither
`challenge_id` nor `client_nonce`. A Gateway therefore cannot use this wire
version to exclude a delayed or replayed response from another handshake at the
protocol layer. This is a production authentication blocker, not an implicit
property of the Broker connection. A signed response that binds the accepted
challenge and client request is a planned gate for the next protocol version;
alpha.4 wire fields remain unchanged.

Every gateway-signed uplink uses the exact
`aether.cloudlink.uplink-signing.v1alpha1` projection in the authentication
profile. A delivery maps Gateway, credential generation, session, message kind,
send time, and delivery stream, epoch, position, batch, and digest directly
from the uplink. `business_digest` is exactly `delivery.digest`.
`expires_at_ms` maps to JSON null only when the wire member is absent. These
delivery rules apply to the core envelope, Integration topology and
observation deliveries, and the Integration Control receipt.

A heartbeat maps `sent_at_ms` from `heartbeat.observed_at_ms`.
`expires_at_ms`, `stream_id`, `stream_epoch`, `position`, `batch_id`, and
`business_digest` are all JSON null. Implementations must not infer delivery
members, an expiry, or a business digest for a heartbeat. The complete ordered
field list and both mappings are normative in
`profiles/cloudlink/v1alpha1/authentication.json`.

Before durable-identity lookup, freshness, or replay evaluation, Cloud verifies
the signature over that exact canonical object, the active Gateway key, the
Broker principal's exact authorization namespace, the current active Gateway
and session identity, the session epoch, and the credential generation. A
heartbeat then uses the heartbeat interval negotiated for that accepted session
and an explicit canonical `evaluation_time_ms`; it never reads an ambient
clock. It is fresh only when both
`observed_at_ms <= evaluation_time_ms + heartbeat_interval_ms` and
`evaluation_time_ms < observed_at_ms + 3 * heartbeat_interval_ms` hold.
Equality at the stale boundary fails as `MESSAGE_EXPIRED`; excessive future
skew fails as `AUTHENTICATION_INVALID`. Every addition and multiplication is
checked in the uint64 domain, and any overflow fails closed as
`AUTHENTICATION_INVALID`.

For each accepted session, Cloud persists the highest accepted heartbeat
`observed_at_ms` and the digest of its exact signing object. After freshness
checks, the same time and same digest is an idempotent replay and never
refreshes server liveness. The same time with another signing projection, or a
lower still-fresh time, fails as `AUTHENTICATION_INVALID` without refreshing
liveness. A higher heartbeat advances replay state and refreshes liveness only
when it is fresh. An already stale message fails as `MESSAGE_EXPIRED` and
changes no state.

In `trusted-connector-broker-attestation`, a configured trusted ingress adapter
supplies origin evidence outside the payload and binds the exact received MQTT
payload bytes. A payload cannot attest to itself. Topic names, payload
identity, challenge requests, and MQTT credentials alone are never Gateway
authentication.

The MQTT `{prefix}` is an authorization namespace, not identity evidence. One
namespace requires unique `gateway_id` values. A multi-tenant deployment uses
tenant- and project-isolated prefixes or Cloud-global Gateway identifiers, and
the Broker principal is restricted to the exact namespace and Gateway uplink
publish and downlink subscribe topics. Cross-namespace and wildcard-Gateway
access are forbidden. Neither the prefix nor the complete topic authenticates
the Gateway.

The proposal specifies the request, persistence and retry transition, Ed25519
algorithm, unpadded-base64url encoding, RFC 8785 JCS signing objects,
absent-value rules, and replay bounds exactly in
`profiles/cloudlink/v1alpha1/authentication.json`. Production key provisioning,
rotation, revocation, verifier ownership, and production signature verification
remain planned. Together with the unsigned, uncorrelated `session-accepted`
response and unsigned durable ACK, these gaps keep the authentication gate a
default-off proposal and block production cutover.
Ordinary logs and public evidence must exclude signatures, nonces, credential
identifiers, and raw authentication transcripts.

The alpha.4 durable-ACK JSON shape is explicitly unsigned. Success means the
application fact and receipt were durably committed before ACK publication,
but alpha.4 contains no production store/outbox implementation and makes no
crash-durability claim. A future signed ACK is a separate command/profile and
requires a Cloud key lifecycle plus production restart evidence. MQTT PUBACK is
never an application durable receipt.

`acknowledged_position = N` is cumulative within exactly one `stream_epoch`.
It proves that every position after the preceding acknowledged cursor and
through `N` is resolved. A position is resolved only when its application fact
and replay receipt are durably committed, or when a previously accepted valid
data-loss declaration for the exact same Gateway, stream, and stream epoch
explicitly covers that position. Evidence from another stream or epoch cannot
close the gap. The ACK's batch and digest still bind `N` to its exact persisted
delivery; data-loss coverage may close only intermediate gaps.

Out-of-order persistence is permitted, but an unresolved position blocks the
cumulative cursor. Cloud must not acknowledge past that gap, and an ACK that
does so fails as `ACK_PREFIX_GAP` without changing the cursor. Once the missing
delivery is durably committed, or valid data-loss evidence for it is durably
accepted, Cloud recomputes the contiguous prefix and may advance. A Gateway
may delete spooled positions `<= N` only after validating such a cumulative
ACK for its current session and exact stream epoch.

Repeated delivery with the same replay identity and digest is idempotent. Reuse
of an identity with a different digest is wire-valid but context-invalid; in
the core reducer it is quarantined as `DIGEST_CONFLICT` and receives no
successful receipt. Data loss is explicit evidence and never causes Cloud to
fabricate samples.

When the gateway-signed per-uplink gate is enabled, authentication runs before
the core reducer. Cloud first selects the repository partition from the
validated Broker/deployment authorization namespace and only then looks up the
four-field durable position identity. The namespace comes from trusted ingress
context, never the payload. A global lookup without that namespace partition
is forbidden. The wire replay identity remains
`(gateway_id, stream_id, stream_epoch, position)`; the namespace is an external
storage and authorization partition, not a new wire field. Identical four-field
identities in different validated namespaces are independent records.

For each durable position in that partition, Cloud stores one committed replay
record containing the namespace binding, replay identity, an
`immutable_delivery_digest`, and one current-session binding of `session_id`,
`session_epoch`, and the exact 13-field signing-object digest. A record from
another namespace fails as `AUTHENTICATION_INVALID`. The immutable digest is
SHA-256 over RFC 8785 JCS UTF-8 for exactly
`{schema,gateway_id,credential_generation,message_kind,sent_at_ms,expires_at_ms,stream_id,stream_epoch,position,batch_id,business_digest}`;
`schema` is the literal
`aether.cloudlink.immutable-delivery-replay.v1alpha1`. It deliberately excludes
only `session_id`, `session_epoch`, and authentication material from the
13-field uplink signing object. A raw signature is not part of either replay
digest.

That committed record means the delivery's business effect and replay record
were atomically made durable. A merely authenticated, reserved, or pending
record never has this meaning, cannot bypass expiry, and must not be passed to
the replay reducer as committed state. On first acceptance, the business
effect, immutable digest, and current-session binding commit atomically before
any successful receipt.

Within the same current session, an exact 13-field signing-object replay is
idempotent and repeats no business effect. A delivery from a newly authenticated
current session may rebind the same committed delivery only when the immutable
digest is identical and its `session_epoch` is strictly greater than the
recorded epoch. The rebind replaces the single current-session binding; it does
not accumulate historical bindings, modify the business fact, or repeat its
effect. The new binding must be durable before Cloud reissues a receipt bound
to that current session. A lower epoch, or the same epoch with another
`session_id`, fails as `AUTHENTICATION_INVALID`. The old session also fails the
current-session check before replay lookup.

Alpha.4 keeps `credential_generation` immutable across a delivery rebind.
Credential rotation therefore requires a future stream-epoch or migration
contract; an implementation must not silently re-sign an outstanding alpha.4
delivery with a new generation. `sent_at_ms` comes from the persistent Edge
enqueue fact and never changes on retry or restart. An originally present
`expires_at_ms` also remains unchanged, and an originally absent value remains
JSON null in both replay digests. Any changed Gateway, generation, message kind,
send time, expiry, stream, stream epoch, position, batch, or business digest at
the durable identity fails as `AUTHENTICATION_INVALID`. For an existing
committed record, this conflict check precedes expiry so an attacker cannot
hide a changed delivery behind `MESSAGE_EXPIRED`.

Data-loss evidence satisfies
`first_lost_position <= last_lost_position < earliest_retained_position`.
Challenge-request, heartbeat, and resume cursor arrays contain at most one
entry for each `(stream_id, stream_epoch)`. Violations are context-invalid, do
not change a business fact, and do not permit a successful application
receipt.

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
the message is context-invalid with `INVALID_EXPIRY_WINDOW`. For a delivery
without a committed replay record, expiration is a hard gate on the first
business effect. It is evaluated against an explicit canonical uint64
`evaluation_time_ms`: the check passes only while
`evaluation_time_ms < expires_at_ms`, and equality is already expired with
`MESSAGE_EXPIRED`. A pending authentication record does not alter that result.

Expiry authorizes the first effect; it does not revoke an already committed
business fact. After common authentication and an exact immutable-digest match,
an existing committed delivery may be replayed in the same session or rebound
to a strictly higher current session epoch even after the original expiry.
That path changes only the current authentication binding when necessary and
may reissue the current-session receipt after the binding is durable; it never
repeats or refreshes the business fact. The portable TCK supplies evaluation
time as scenario input and never consults an ambient wall clock.

The embedded Runtime Manifest checksum is lowercase SHA-256 over RFC 8785 JCS
of the complete manifest object with its top-level `checksum` member omitted.
Its digest omits the `sha256:` prefix because the enclosing checksum object
already declares the algorithm.

`envelope.schema.json` is the reusable structural base. Consumers
must validate an uplink with its message-kind entry Schema
(`runtime-manifest-report`, `telemetry-batch`, or `data-loss`); using the base
alone does not validate the discriminator-to-payload relationship.

## Integration extension

The extension profile is
`profiles/cloudlink/v1alpha1/integration.json`. It adds exactly two uplink
message kinds:

- `integration-topology-snapshot`, whose `payload` is the complete unchanged
  `aether.integration.topology-snapshot.v1alpha1` object;
- `integration-observation-batch`, whose `payload` is the complete unchanged
  `aether.integration.observation-batch.v1alpha1` object.

The outer `gateway_id` is bound by the authenticated CloudLink session. It is
not repeated inside the provider-neutral payload. Provider URLs, tokens,
cookies, credential references, arbitrary provider objects, and authentication
material remain forbidden. Topic names and payload identities still do not
authenticate a Gateway.

Each Integration instance uses distinct topology and observation streams. For
one stream epoch, the binding
`(gateway_id, stream_id, stream_epoch, message_kind, integration_id)` is
immutable. Reusing it for another Integration or message kind fails as
`STREAM_BINDING_CONFLICT`. Positions retain the existing CloudLink identity
and replay rules; this extension does not create another position or ACK
protocol.

A topology delivery uses
`delivery.batch_id = "topology-" + payload.snapshot_generation`. One accepted
generation is bound to one exact CloudLink position and digest. An exact
position replay is idempotent. A lower generation at a new position fails as
`TOPOLOGY_GENERATION_STALE`; reusing the accepted generation at a new position
fails as `TOPOLOGY_GENERATION_CONFLICT`. A topology payload is one complete
atomic replacement and is never fragmented.

An observation delivery requires
`delivery.batch_id == payload.batch_id`. Within one Gateway, Integration, and
topology generation, a payload batch identity is bound to one exact stream
position and digest. Reusing it at another position fails as
`BATCH_ID_CONFLICT`; an outer/payload mismatch fails as `BATCH_ID_MISMATCH`.
Cloud must already have accepted the exact `integration_id` and
`snapshot_generation`. An older generation fails as
`TOPOLOGY_GENERATION_STALE`; an unseen newer generation fails as
`REFERENCE_NOT_FOUND`. Only then are entity, point, quality, and value-type
references evaluated against that topology.

The business digest remains lowercase SHA-256 over RFC 8785 JCS of exactly
`{protocol_version,message_kind,payload}`, so it covers the entire unchanged
Integration object. A successful existing `durable-ack` is permitted only
after the normalized Integration fact and replay receipt are durably committed
before ACK publication. The ACK repeats the exact Gateway/session, stream
epoch, position, batch, and digest bindings. MQTT PUBACK remains only transport
evidence.

The MQTT limit of 262144 bytes applies to the complete UTF-8 message. An
oversized full topology fails as `FIELD_BOUND`; partial topology publication is
forbidden. A producer may partition observations only at observation
boundaries into independent public batches with distinct batch identities,
each of which independently fits the complete-message limit.

The extension is not implied by accepting base protocol version `1.0`.
Before emission, the current Runtime Manifest must declare
`aether.cloudlink.integration.v1alpha1` in `protocols`, the Cloud consumer must
explicitly enable that exact extension, and the rollout must upgrade Cloud
before enabling Edge publication. An alpha.3 consumer correctly rejects the
unknown message kinds. Mixed-version deployments keep Integration publication
disabled; they do not downgrade, reinterpret a topology as numeric telemetry,
or probe support by sending business data.

The alpha telemetry slice currently carries only finite JSON-number values
for telemetry/status points. Non-numeric Thing Model value types, events, and
the topology-to-model point-resolution contract are planned, not silently
inferred. An optional sample `model` value is only a commissioning hint and is
not sufficient mapping authority.

This slice does not freeze exact signed `int64`, `uint64`, decimal, byte, or
string sample encodings. A JSON number is not a substitute for an exact 64-bit
integer contract.

Legacy transport remains the default. Core CloudLink and the read-only
Integration extension contain no physical-control, direct SHM, or direct
register operation. The separately versioned, default-off
`aether.cloudlink.integration-control.v1alpha1` extension is defined by
`spec/integration-control-v1alpha1.md`; it exposes only one governed semantic
power-set action and does not add arbitrary RPC.
