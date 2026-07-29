# ADR-0041: Require CAS-only channel mutations and canonical receipts

## Status

Accepted and implemented on 2026-08-20.

## Context

The governed channel command already published a desired-state revision on
channel reads, and the first-party CLI and MCP clients required that revision
for update, delete, enable, and disable. The transport-neutral port and HTTP
adapter nevertheless retained a revisionless compatibility path. It serialized
blind writes by channel identity but could not detect that a caller had based a
mutation on stale desired state.

The mutation response also carried two representations of the same facts:
`id` and `channel_id`, `enabled` and `desired_enabled`, and `runtime_status` and
`runtime_projection`. It echoed request fields that the canonical CLI did not
consume and attached an always-empty response metadata map. Separately,
`GET /api/status` existed for a retired dashboard even though `/health` already
reports uptime, version, channel state, SQLite, SHM, watchdog, and system
metrics.

## Decision

1. Require `x-aether-expected-revision` for every online update, delete, enable,
   and disable command. Creation omits it because no prior entity revision
   exists.
2. Make the revision mandatory in the transport-neutral `ChannelMutation`
   variants. Remove revisionless constructors and the duplicate
   `*_with_revision` constructor family; the canonical constructors are the
   revisioned forms.
3. Keep per-channel serialization inside the SQLite adapter as an execution
   guard, but never use it as a substitute for caller-visible compare-and-set.
4. Return one canonical mutation receipt containing `channel_id`, `request_id`,
   operation, resulting revision, desired enabled state, runtime projection,
   reconciliation state, completion-audit state, retryability, and message.
5. Remove request-field echoes, duplicate status/identity aliases, and empty
   metadata from channel mutation and reconciliation envelopes.
6. Remove `GET /api/status` and its dashboard-specific DTO. `/health` remains
   the service readiness and operational summary endpoint.
7. Add source and OpenAPI checks that reject restoration of these compatibility
   surfaces.

## Consequences

- A caller cannot silently overwrite desired state after another commissioner
  advances the channel revision.
- Ports, application, HTTP, CLI, and MCP now expose one CAS contract.
- Mutation receipts cannot disagree across duplicate identity, enabled-state,
  or runtime-status fields.
- Removing the dashboard endpoint does not remove any deployment health or
  protocol diagnostic evidence.
- Offline `aether sync` remains a separately supervised commissioning path and
  does not call the online mutation command.
