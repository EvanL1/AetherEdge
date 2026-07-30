---
title: "ADR-0028: AetherCloud gateway enrollment identity boundary"
description: Implements only local CLI-to-test-Claim-server identity binding; the production endpoint, credential issuance, and authenticated CloudLink remain unavailable.
updated: 2026-07-29
---

# ADR-0028: AetherCloud gateway enrollment identity boundary

## Status

Accepted on 2026-07-29 for the first local AetherEdge enrollment foundation.
This decision proves a real `aether` process can generate and durably stage an
identity, call a strict local HTTP Claim server, and persist a `claimed`
acknowledgement. It does not claim that the AetherCloud production endpoint,
CloudLink credential issuance, or an authenticated production CloudLink
session is available.

## Context

CloudLink already defines a Gateway-signed session handshake, but AetherEdge
previously had no real installation-time path from an Enrollment Token to a
durable Gateway key and Cloud Claim. A UI-only simulation would leave the
private-key owner, retry semantics, and local recovery behavior undefined.

Installation may occur before `aether-uplink` is running. A seventh resident
service would add lifecycle and ownership complexity without creating another
domain capability. Passing an Enrollment Token on a command line would expose
it through process inspection and shell history.

Claim acknowledgement is intentionally narrower than CloudLink
authentication. The temporary Claim contract binds a public-key fingerprint
to a Gateway ID, but returns no signed credential bundle, broker
authentication material, or authenticated-session evidence.

## Decision

### Ownership and composition

Runtime Gateway identity, private-key use, credential rotation, and CloudLink
session state belong exclusively to `aether-uplink`.

The `aether` CLI may compose the enrollment application use case once during
installation because the Uplink service may not yet be running. The CLI does
not own an MQTT session, copy the private key into configuration, start another
daemon, or report CloudLink connectivity.

The transport-neutral application boundary consists of typed enrollment,
key-generation, identity-store, and clock ports. The provisional HTTP DTOs and
endpoint path stay inside the concrete Cloud enrollment adapter. The local
file schema stays inside `aether-store-local`.

### State machine and retry

The local state machine is:

```text
unconfigured -> key-generated -> claim-pending -> claimed
```

For a new identity, AetherEdge:

1. generates an Ed25519 private seed with the operating-system CSPRNG;
2. persists the seed, public key, immutable Cloud scope, and
   `key-generated` state before contacting Cloud;
3. derives the public-key fingerprint as lowercase hexadecimal
   `SHA-256(raw 32-byte Ed25519 public key)`;
4. derives and persists one stable UUID idempotency key, advancing to
   `claim-pending`;
5. sends one bounded Claim attempt; and
6. atomically advances to `claimed` only after a strict matching response.

An unknown network result leaves the identity in `claim-pending`. A later
invocation reuses the same private key, public key, fingerprint, and
idempotency key. A different Tenant, Project, Gateway, or Cloud origin
conflicts with the existing identity and fails closed. Repeating the same
already-claimed scope is a successful local no-op and does not contact Cloud.

The idempotency key is a durable compatibility contract, not a random retry
token. It is UUIDv5 under namespace
`2e01d20b-3147-5a82-9599-23e20a9dc172` over this exact UTF-8 transcript, with
single LF separators and no trailing LF:

```text
aether.cloud.gateway-enrollment-claim.v1
<normalized cloud origin>
<canonical tenant UUID>
<canonical project UUID>
<canonical gateway UUID>
<64-character public-key fingerprint>
```

Changing the namespace, field order, separators, normalization, or fingerprint
definition would invalidate durable pending and claimed state and therefore
requires an explicit storage migration.

### Secret and local-storage baseline

The Enrollment Token is accepted only through a hidden terminal prompt or the
explicit `--token-stdin` mode. It is never a CLI argument, persisted field,
log value, error detail, or JSON output field. Token and private-seed buffers
use the existing zeroizing secret types at their ownership boundaries.

The Linux file baseline uses `<data-directory>/uplink/identity`, a `0700`
identity directory, `0600` private-key and state files inside it, and a `0600`
adjacent enrollment lock in the parent directory. Initial creation writes a
`0700` staging directory beside the final identity directory, fsyncs its
regular files, atomically renames the complete directory, and fsyncs the
parent. Later state transitions use a temporary regular file inside the
identity directory, followed by file `fsync`, atomic rename, and directory
`fsync`.

The adapter validates the directory chain and the expected private-key, state,
and lock paths. It rejects symbolic links or non-regular expected files, unsafe
directory permissions, hard-linked identity files, malformed state, and
identity replacement. It does not claim to inventory unrelated extra directory
entries.

Ordinary files are only the current Linux baseline. They are not a TPM,
Secure Enclave, system keyring, HSM, or hardware-backed protection claim.

### Provisional Claim transport

The first adapter sends:

```text
POST {cloudOrigin}/api/v1/fleet/enrollment-claims:claim
Content-Type: application/json
Idempotency-Key: <stable UUID>
```

The closed request schema is
`aether.cloud.gateway-enrollment-claim.v1`. It contains the canonical Tenant,
Project, and Gateway UUIDs, the opaque Enrollment Token, and an Ed25519
credential request with the unpadded base64url raw public key and canonical
fingerprint.

The only accepted success schema is
`aether.cloud.gateway-enrollment-claimed.v1`, with the same Gateway ID,
`state: "claimed"`, and a positive JSON-safe integer revision. Unknown fields,
unknown schemas, identity mismatch, another state, unsafe revisions,
non-JSON success responses, and oversized bodies fail closed.

Production accepts HTTPS only. Development HTTP requires an explicit CLI flag
and is restricted to exactly `localhost` or `127.0.0.1`. The client disables
redirect following and ambient proxies, sets connection/request/total
deadlines, and streams responses under a fixed size bound.

### Meaning of `claimed`

`claimed` means only that the Claim server acknowledged binding the submitted
public-key fingerprint to the requested Gateway ID and returned a revision.
It must not be labelled `credential-active`, `cloudlink-connected`, or
`online`.

Until AetherCloud returns a separately specified and verifiable active
credential, AetherEdge does not:

- start authenticated CloudLink;
- generate a development credential;
- fall back to anonymous MQTT;
- use the Enrollment Token as an MQTT password; or
- invent a CloudLink credential bundle.

The strict CloudLink challenge sequence remains:

```text
session-challenge-request
-> verify Cloud-signed challenge
-> sign the challenge transcript
-> Gateway-signed session hello
-> validate session accepted
```

The Edge real-broker test uses the existing strict codec and
`GatewaySessionAuthenticator`. The current AetherCloud dual harness does not
yet compose its challenge service, so cross-repository execution remains
blocked rather than accepting the legacy direct hello.

## Consequences

- A real compiled CLI process and local HTTP test server now exercise the
  complete local process-to-test-server Claim boundary without requiring a
  real Cloud account, Broker, PostgreSQL, or external service.
- Network ambiguity is recoverable without generating duplicate identities.
- `aether-uplink` has a read-only seam for loading a complete claimed identity,
  but production CloudLink composition remains gated.
- Operators must coordinate Cloud-side revocation before replacing a lost or
  compromised identity; no unsafe local overwrite or automatic reset is
  provided.
- A future AetherCloud credential contract requires its own shared authority,
  verification rules, storage transition, and production-composition evidence.

## Production exit criteria

Production pairing and CloudLink authentication may be claimed only after:

1. AetherCloud implements and deploys the matching production Claim endpoint;
2. a shared contract defines verifiable credential issuance, activation,
   rotation, revocation, and recovery;
3. Cloud trust-key delivery and rotation are composed without static test
   keys;
4. `aether-uplink` consumes the claimed identity and active credential in the
   production composition;
5. AetherCloud's harness composes challenge issuance and Gateway-signed
   session acceptance; and
6. the authenticated `session-accepted` contract binds the complete current
   handshake transcript.
