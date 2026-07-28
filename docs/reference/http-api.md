---
title: HTTP API
description: The single remote gateway, service-local OpenAPI, authentication, and compatibility conventions
updated: 2026-07-14
---

# HTTP API

AetherEdge runs six HTTP services, but remote applications have one network
boundary: `aether-api` on port 6005. This page describes the gateway, its
service-local contracts, and the security conventions that apply across it.

> Each service-generated OpenAPI document is the source of truth for its
> paths, parameters, request and response schemas, status codes, media types,
> and operation-specific security requirements. `aether-api` is the sole
> Swagger UI owner: it presents all six documents through fixed gateway paths.

## Built-in documentation

| Service | Default boundary | Loopback source document | Gateway Swagger document |
|---|---|---|---|
| `aether-io` | loopback | `http://127.0.0.1:6001/openapi.json` | `http://<edge-host>:6005/openapi/io.json` |
| `aether-automation` | loopback | `http://127.0.0.1:6002/openapi.json` | `http://<edge-host>:6005/openapi/automation.json` |
| `aether-history` | loopback | `http://127.0.0.1:6004/openapi.json` | `http://<edge-host>:6005/openapi/history.json` |
| `aether-api` | remote gateway | — | `http://<edge-host>:6005/openapi/gateway.json` |
| `aether-uplink` | loopback | `http://127.0.0.1:6006/openapi.json` | `http://<edge-host>:6005/openapi/uplink.json` |
| `aether-alarm` | loopback | `http://127.0.0.1:6007/openapi.json` | `http://<edge-host>:6005/openapi/alarm.json` |

Swagger is opt-in only through `aether-api`'s `swagger-ui` Cargo feature. To
include the single gateway UI in an installer build, use:

```bash
./scripts/build-installer.sh <version> <arch> -s rust --enable-swagger
```

When enabled, `http://<edge-host>:6005/docs` offers a document selector for
the gateway and all five loopback services. The gateway re-bases service paths
onto its authenticated `/api/v1/<service>` namespace so Swagger "Try it out"
never targets a loopback port. The gateway documents are public routes, but
protected operations still require their declared credentials. Enable the UI
only on a trusted commissioning or development network.

## Exposure boundary

Only `aether-api` is a remote-facing service. Its authenticated application
gateway exposes five fixed namespaces and forwards them only to configured
loopback services:

| Remote namespace | Service-local owner |
|---|---|
| `/api/v1/io/*` | `aether-io` |
| `/api/v1/automation/*` | `aether-automation` |
| `/api/v1/history/*` | `aether-history` |
| `/api/v1/uplink/*` | `aether-uplink` |
| `/api/v1/alarm/*` | `aether-alarm` |

The target is selected by the namespace, never by caller input. Startup
validation accepts only explicit loopback HTTP origins. The gateway preserves
the original signed Bearer token and the small set of documented command and
conditional-request headers; it discards caller-supplied actor headers and
sanitizes upstream transport errors. The direct service ports remain internal
and must not be published from the device.

Generated applications and downstream product interfaces use the corresponding
gateway-prefixed path on port 6005. Gateway-proxied OpenAPI preserves each
service's operation schemas and security declarations while re-basing only the
supported paths onto that fixed namespace. This does not make a service-local
port a supported client interface. A missing operation still has to be added
through the owning application boundary rather than invented by a UI, attached
directly to SHM, or implemented as a storage write.

Loopback is a deployment boundary, not an identity credential. IO channel
commissioning plus selected automation and alarm commands authenticate at the
operation boundary, but many other local management routes in io, history,
uplink, automation, and alarm still rely on host isolation. Do not infer that a
direct service port is safe to expose because some of its operations declare a
Bearer scheme.

The current full channel-configuration query can include protocol parameters
and per-channel logging configuration. It remains compatibility debt pending a
redacted, authenticated application query capability. Keep the io port on
loopback and do not proxy that response to an untrusted client.

## Authentication model

`aether-api` protects its management routes with a signed access JWT. REST
clients send it only in the standard header:

```http
Authorization: Bearer <access-token>
```

Login, refresh-token lifecycle endpoints, service health, and—when compiled
in—the documentation routes form the public transport surface described by
the gateway OpenAPI document. Public registration is disabled unless explicitly
enabled. The required `JWT_SECRET_KEY` must contain at least 32 bytes and must
be managed outside source control.

The gateway requires an access JWT before forwarding any namespace request.
The owning service then applies operation-specific authorization:

- io channel create, update, delete, enable, and disable require an Admin or
  Engineer Bearer JWT with `io.channel.manage`;
- automation device actions accept a Bearer access JWT or the dedicated
  `AetherService <token>` uplink credential;
- automation rule management and manual execution require an Admin or Engineer
  Bearer JWT with the capability documented for the operation;
- alarm rule mutation and alert resolution require an Admin or Engineer Bearer
  JWT;
- forwarded identity headers and loopback reachability do not satisfy these
  protected command boundaries.

## Governed commands

Commands that can change channel configuration, device, rule, processing, or
alarm state declare their risk, permission, idempotency, confirmation, and
audit policy in OpenAPI. The application command boundary enforces those
declarations; the HTTP handler must not write SHM or storage directly.

For a protected mutation, follow the operation schema exactly. Depending on the
operation, explicit confirmation is carried as `x-aether-confirmed: true` or in
the request body. Supply `x-request-id` when documented so retries and audit
records share a stable correlation ID. Never assume that confirmation or
identity may be forwarded in an undeclared header.

An accepted governed command includes its `request_id` and audit outcome in the
response described by OpenAPI. If dispatch or persistence succeeded but the
terminal audit append failed, the operation is still accepted and its audit
state is marked incomplete and non-retryable. Retain the correlation ID and do
not automatically submit the command again. Failure to record the attempted
audit fails closed before dispatch.

Device-command acceptance means the local command plane accepted the request;
it is not proof that physical equipment executed it. Use feedback telemetry for
closed-loop confirmation.

For channel commissioning, SQLite desired configuration and the active
protocol runtime are deliberately distinct. Existing-resource mutations may
document an optional `x-aether-expected-revision` compare-and-set guard. An
accepted response can report an activation-pending or degraded runtime
projection after desired state has committed; reconcile by `request_id` and
`resulting_revision` rather than automatically repeating the non-idempotent
mutation. The exact headers, receipt fields, and per-operation status codes are
defined by the I/O OpenAPI document.

## Response compatibility

Most business handlers return the shared success envelope:

```json
{ "success": true, "data": { "...": "..." }, "metadata": { "...": "..." } }
```

`metadata` is omitted when empty. Health probes, service banners, and CSV
exports intentionally use their own representations.

Error responses are still migrating and may use one of these compatibility
shapes:

- `{ "success": false, "error": { "code": 400, "message": "..." } }`;
- `{ "success": false, "message": "..." }`;
- the flat `AetherError` mapping with `error_code`, `category`, and `retryable`.

Clients must treat the operation's OpenAPI status and response content type as
authoritative and tolerate the documented compatibility shape. Do not infer a
universal error schema from another service.

## Contributor contract

When a route, schema, security rule, response, or feature gate changes, update
the owning service's generated OpenAPI annotations and tests in the same change.
Remote examples must use the gateway-prefixed form while local contract tests
may use the owning loopback service. Do not add a second endpoint list to
Markdown. Run the six-service parity check:

```bash
./scripts/check-openapi-contracts.sh
```

This check verifies each service-owned OpenAPI contract and compiles the
single gateway Swagger UI. It also runs in the Rust CI workflow.
