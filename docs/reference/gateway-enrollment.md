---
title: AetherCloud Gateway enrollment
description: Generate and claim a local AetherEdge Gateway identity without implying an active CloudLink credential or session.
updated: 2026-07-29
---

# AetherCloud Gateway enrollment

Gateway enrollment is an installation-time identity Claim. The implemented
foundation generates an Ed25519 key, persists pending identity state, calls a
strict HTTP Claim endpoint, and records the matching Claim-server
acknowledgement.

`claimed` is not `credential-active`, `cloudlink-connected`, or `online`.
AetherCloud production Claim deployment, credential issuance, and production
CloudLink composition remain planned.

## Enroll interactively

Use the hidden terminal prompt so the Enrollment Token is absent from process
arguments and shell history:

```bash
aether cloud enroll \
  --cloud-url https://api.aetheriot.ai \
  --tenant-id 11111111-1111-4111-8111-111111111111 \
  --project-id 22222222-2222-4222-8222-222222222222 \
  --gateway-id 33333333-3333-4333-8333-333333333333
```

For an explicitly non-interactive secret provider, stream exactly one bounded
token line to stdin:

```bash
secret-provider-command | aether cloud enroll \
  --cloud-url https://api.aetheriot.ai \
  --tenant-id 11111111-1111-4111-8111-111111111111 \
  --project-id 22222222-2222-4222-8222-222222222222 \
  --gateway-id 33333333-3333-4333-8333-333333333333 \
  --token-stdin
```

There is deliberately no `--token` option. Do not place the token in an
environment variable, configuration file, command argument, log, or agent
transcript.

On success, human output ends with:

```text
身份已配对，CloudLink 凭据尚未激活
```

For stable non-secret output, put the global JSON flag before the command:

```bash
aether --json cloud enroll \
  --cloud-url https://api.aetheriot.ai \
  --tenant-id 11111111-1111-4111-8111-111111111111 \
  --project-id 22222222-2222-4222-8222-222222222222 \
  --gateway-id 33333333-3333-4333-8333-333333333333
```

The data object contains only:

- `cloud_url`
- `tenant_id`
- `project_id`
- `gateway_id`
- `public_key_fingerprint`
- `enrollment_state`
- `claimed_revision`

It never contains the Enrollment Token, private key, Authorization material,
or the complete public key.

## Read local status

Status is a local read and does not contact AetherCloud or a Broker:

```bash
aether cloud status
aether --json cloud status
```

The possible enrollment states are:

| State | Meaning |
|---|---|
| `unconfigured` | No local Gateway identity exists |
| `key-generated` | The private key and immutable scope are durable |
| `claim-pending` | The stable Claim idempotency key is durable; retry is safe |
| `claimed` | The configured Claim server acknowledged the public-key binding and revision |

MQTT connection state is intentionally absent.

## Retry and conflicts

If the HTTP result is unknown, run the same command again with a valid
Enrollment Token. AetherEdge reuses the durable private key, public key,
fingerprint, and `Idempotency-Key`. It does not create a second identity.

Repeating an already-claimed command for the exact same Cloud, Tenant, Project,
and Gateway is a successful local no-op. A different immutable scope conflicts
and fails closed. The CLI provides no force-overwrite option.

## Development loopback server

Production enrollment accepts HTTPS only. A local test server may use HTTP
only with an explicit flag and only on exactly `localhost` or `127.0.0.1`:

```bash
aether cloud enroll \
  --cloud-url http://127.0.0.1:8080 \
  --tenant-id 11111111-1111-4111-8111-111111111111 \
  --project-id 22222222-2222-4222-8222-222222222222 \
  --gateway-id 33333333-3333-4333-8333-333333333333 \
  --allow-insecure-localhost
```

The flag does not permit another HTTP host, an IPv6 alias, a URL path, user
information, query, fragment, or redirect.

## Local storage

The identity is stored under:

```text
<data-directory>/uplink/identity
```

The installed data directory is selected by the normal CLI path resolution;
`--db-path` can select an isolated data directory for development and tests.
The identity directory is `0700`; its private-key and state files are `0600`,
as is the adjacent enrollment lock in the parent directory. Initial creation
atomically renames a complete, fsynced `0700` staging directory; later state
changes fsync and atomically replace the state file. The directory chain and
expected identity paths reject symbolic links, unsafe permissions, non-regular
files, hard links, malformed state, and identity replacement. Unrelated extra
entries are not presented as validated identity material.

This ordinary-file implementation is the current Linux baseline only. It does
not provide TPM, Secure Enclave, system-keyring, HSM, or hardware-level key
protection.

## HTTP Claim contract

Endpoint and headers:

```http
POST {cloudOrigin}/api/v1/fleet/enrollment-claims:claim
Content-Type: application/json
Idempotency-Key: <stable UUID>
```

Request:

```json
{
  "schema": "aether.cloud.gateway-enrollment-claim.v1",
  "tenantId": "11111111-1111-4111-8111-111111111111",
  "projectId": "22222222-2222-4222-8222-222222222222",
  "gatewayId": "33333333-3333-4333-8333-333333333333",
  "enrollmentToken": "<opaque token>",
  "credentialRequest": {
    "algorithm": "ed25519",
    "publicKey": "<unpadded base64url raw 32-byte public key>",
    "fingerprint": "<64 lowercase hex SHA-256>"
  }
}
```

Accepted success response:

```json
{
  "schema": "aether.cloud.gateway-enrollment-claimed.v1",
  "gatewayId": "33333333-3333-4333-8333-333333333333",
  "state": "claimed",
  "revision": 1
}
```

The adapter rejects unknown fields or schemas, identity mismatch, any state
other than `claimed`, zero or non-JSON-safe revisions, non-JSON success
responses, oversized bodies, redirects, timeouts, and HTTPS downgrade. Remote
response bodies are not copied into errors.

The response contains no invented CloudLink credential bundle. See
[Gateway identity recovery](../recovery/gateway-identity-recovery.md) for the
ownership and recovery boundaries.
