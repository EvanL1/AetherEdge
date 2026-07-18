---
title: Safe stop and control revocation
description: Contain an unsafe or uncertain control path while preserving deterministic local safety and audit evidence.
---

# Safe stop and control revocation

Use this runbook when an agent, cloud session, integration, rule, or channel
must no longer be allowed to request physical actions.

AetherEdge has governed disable operations for specific resources, but it does
not yet expose one universal software emergency-stop capability. A commissioned
physical emergency stop, device interlock, or site operating procedure remains
the primary authority for immediate hazard control.

## Contain

1. Apply the site safety procedure or physical interlock first when people or
   equipment may be at risk.
2. Revoke the caller permission or credential at its authority boundary.
3. Disable the narrowest affected control surface: the Home Assistant governed
   control feature, an action route, a rule, or a communication channel.
4. Use only the typed application command with current revision, explicit
   confirmation, and durable audit. Do not write SHM, SQLite, provider APIs, or
   protocol registers directly.
5. Record the operation identity, `request_id`, resulting revision, terminal
   audit state, and runtime reconciliation status.

## Unknown outcome

If a command times out, loses its response, reports incomplete audit, or leaves
runtime convergence unknown, do not submit it again automatically. Read the
authoritative desired revision, resource state, audit trail, and physical
observation. Keep the path disabled or physically isolated until a human can
resolve conflicting evidence.

Provider acceptance, MQTT acknowledgement, and an Edge command receipt are not
proof that the physical process reached a safe state. Verify the relevant
sensor or device feedback under the site's commissioning policy.

## Restore

Restore access one capability at a time only after the initiating credential or
policy problem is fixed, deterministic safety behavior is healthy, the runtime
projection matches desired state, and a human explicitly approves the change.

See [safe operations](../guides/safe-operations.md), the
[MCP tools reference](../reference/mcp-tools.md), and
[configuration rollback](configuration-rollback.md).
