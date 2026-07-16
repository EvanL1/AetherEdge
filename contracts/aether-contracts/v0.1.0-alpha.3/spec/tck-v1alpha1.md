---
id: tck-v1alpha1
status: experimental
version: 0.1.0-alpha.3
normative: true
---

# Language-neutral TCK v1 alpha 1

Repository tests compile every normative JSON Schema, validate valid and
wire-invalid fixtures, preserve context-invalid distinctions, and verify every
declared SHA-256.

The repository reference runner executes the published core scenarios for
integer precedence, strict raw JSON, business and Runtime Manifest digests,
minimal session/replay/ACK context, data-loss and cursor rules, and Thing Model
key conflicts. It is not a production CloudLink state machine. Wire-invalid
fixtures currently prove structural rejection; exact Schema-to-failure-code
and JSON-path mapping remains planned.

The portable black-box runner protocol is planned as NDJSON over standard input
and output. Operations are `validate`, `canonicalize`, `digest`,
`verify-signature`, and `check-compatibility`. It will compare acceptance,
stable failure code, JSON path, canonical bytes, digest, and state outcome; it
will not compare language-specific error prose.

TypeScript, Rust, C, and C++ remain experimental until each executes the same
manifest and scenario set. The real-Broker dual harness and destructive fault
injection are separate opt-in evidence and never enter the default offline test
path.
