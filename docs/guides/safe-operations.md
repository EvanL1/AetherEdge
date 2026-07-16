---
title: Safe Operations for Applications and Agents
description: Preserve identity, confirmation, audit, topology fencing, and uncertain command outcomes when operating AetherEdge
updated: 2026-07-13
---

# Safe Operations for Applications and Agents

AetherEdge treats every browser, CLI, AI assistant, and generated backend as an
untrusted application client. Read operations and commands use the same
application boundary, but commands additionally require an authenticated
actor, an explicit permission, the declared confirmation policy, and durable
audit evidence. Access to an HTTP route or MCP tool is never device authority.

## Use the public application boundary

Remote clients connect only to authenticated `aether-api:6005` routes and the
gateway WebSocket. IO, automation, history, uplink, and alarm process ports are
internal loopback interfaces. Do not expose them, proxy them from a product UI,
or bypass the application API by writing SHM, SQLite, Pack files, or an
external mirror.

Live point state comes from SHM. A database, cache, history adapter, browser
store, or AI context may be delayed and must not silently replace that
authority. Preserve topology epoch, source timestamp, freshness, health, and
configuration revision whenever the contract supplies them.

## Start read-only

- MCP omits write tools unless `--allow-write` is deliberately selected.
- Generated applications should omit command controls until the use case and
  permissions are explicit.
- Device control is deny-by-default. A Viewer token never becomes a command
  identity because the client sends a role or actor header.
- The gateway forwards signed credentials and confirmation but strips
  caller-supplied actor identity headers. Each downstream application service
  independently verifies the signed identity.

## Treat command metadata as policy

Before issuing a command, verify its current OpenAPI or MCP metadata:

- query or command classification;
- risk level and required permission;
- idempotency and retry policy;
- confirmation requirement;
- expected revision or topology fence;
- audit and correlation fields.

Send `confirmed: true` or `x-aether-confirmed: true` only after the operator has
confirmed the concrete target and effect. Preserve `request_id`, `command_id`,
and resulting revision values in logs and UI state.

## Handle uncertain outcomes

Do not automatically retry a non-idempotent command after timeout, an accepted
response with incomplete audit, a degraded runtime projection, or a lost
connection. The durable change may already exist. Query the command receipt,
audit record, configuration revision, and reconciliation state first.

Command acceptance means the local command plane accepted the request. It does
not prove physical execution. Use feedback telemetry, with a matching topology
generation and freshness policy, before reporting a closed-loop result.

## Keep deterministic safety independent of AI

AI may propose, explain, or invoke governed application capabilities. It must
not participate in protocol polling, acquisition timing, SHM publication,
safety interlocks, or hard real-time control. The edge runtime and commissioned
rules must remain deterministic when every AI client is disconnected.

Energy-specific limits, equipment semantics, and commissioning policy belong
to the active domain Pack. For the official energy implementation, read the
[AetherEMS Energy Pack safety guide](https://github.com/EvanL1/AetherEMS/blob/main/packs/energy/knowledge/safe-operations.md).

## Related pages

- [Build Applications with AI](build-applications-with-ai.md)
- [Connect AI Assistants](ai-assistants.md)
- [Application control boundary](../adr/0008-application-control-boundary.md)
- [Shared memory](../concepts/shared-memory.md)
