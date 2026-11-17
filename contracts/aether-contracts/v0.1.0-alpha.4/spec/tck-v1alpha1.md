---
id: tck-v1alpha1
status: experimental
version: 0.1.0-alpha.4
normative: true
---

# Language-neutral TCK v1 alpha 1

Repository tests compile every normative JSON Schema, validate valid and
wire-invalid fixtures, preserve context-invalid distinctions, and verify every
declared SHA-256.

The repository reference runner executes the published core scenarios for
integer precedence, strict raw JSON, business and Runtime Manifest digests,
minimal session/replay/ACK context, data-loss and cursor rules, and Thing Model
key conflicts. Integration scenarios additionally freeze topology identity and
reference resolution, exact snapshot-generation binding, point value-type
matching, quality/value presence, and canonical signed integer, decimal, and
Base64url boundaries. The reference runner is not a production CloudLink or
provider state machine. Wire-invalid fixtures currently prove structural
rejection; exact Schema-to-failure-code and JSON-path mapping remains planned.

The authentication TCK additionally freezes the closed
`session-challenge-request` surface, its existing `up/session` MQTT route,
default-off and non-authenticating request policy, exact persisted-challenge
retry, strict `evaluation_time_ms < expires_at_ms` deadline, and atomic
one-time challenge consumption with session acceptance. Its language-neutral
uplink reference freezes exact delivery and heartbeat signing projections,
explicit-null rules, accepted-session heartbeat freshness, checked uint64
arithmetic, no-liveness-refresh heartbeat replay, and session-independent
delivery replay. Delivery scenarios freeze the immutable replay projection,
same-session exact replay, strictly higher-session-epoch rebind, same-epoch
session conflict, epoch rollback, old-session rejection, generation
immutability, and restart-stable single-binding state. They keep the four-field
wire replay identity unchanged while proving namespace-partitioned independent
records and cross-namespace record rejection; the namespace is trusted ingress
context, never payload data. They also prove that a committed business fact may
rebind and recover its current-session durable ACK after original expiry, while
an expired first delivery or pending authentication record cannot. A conflict is reported as
`AUTHENTICATION_INVALID` before persisted-replay expiry and never repeats a
business effect. All evaluation times and negotiated heartbeat intervals are
explicit scenario inputs; the runner never consults an ambient clock. The TCK
helper is a pure reducer: `committed_delivery` is a precondition meaning the
business effect and replay record already committed atomically, while its
returned next state is not durable until the caller commits it. The TCK also
asserts that alpha.4 `session-accepted` has no signature or handshake correlation
and therefore does not satisfy the planned production gate.
Integration Control fixtures reject receipt `failure_code` values longer than
128 characters.

The durable-ACK scenarios freeze cumulative contiguous-prefix behavior:
out-of-order durable positions cannot advance across an undeclared gap, valid
data-loss evidence for the exact same stream epoch may resolve an intermediate
gap, evidence from another epoch cannot, and a later durable fill allows the
cursor to advance. The reference runner consumes explicit persisted positions
and loss ranges; it does not infer storage state.

CloudLink Integration tests additionally freeze unchanged public payload
wrapping, entry-Schema discrimination, authenticated outer Gateway identity,
immutable stream and batch bindings, topology-generation ordering, exact
durable-ACK binding, secret rejection, and the complete-message size budget.
They do not prove a product database transaction, Broker deployment, or
restart recovery.

Integration Control tests freeze the independent default-off activation, the
single closed `device.power.set.v1` action, exact topology target, mandatory
governance and confirmation evidence, Cloud signing projection, job replay
binding, and provider receipt evidence. They structurally reject arbitrary
provider operations, URLs, tokens, and physical-completion claims. They do not
prove a product Home Assistant invocation or a physical actuator outcome.

The portable black-box runner protocol is planned as NDJSON over standard input
and output. Operations are `validate`, `canonicalize`, `digest`,
`verify-signature`, and `check-compatibility`. It will compare acceptance,
stable failure code, JSON path, canonical bytes, digest, and state outcome; it
will not compare language-specific error prose.

TypeScript, Rust, C, and C++ remain experimental until each executes the same
manifest and scenario set. The real-Broker dual harness and destructive fault
injection are separate opt-in evidence and never enter the default offline test
path.
