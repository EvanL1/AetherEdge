# ADR-0008: Route external device actions through the application boundary

## Status

Accepted and implemented for external device actions on 2026-07-11.

## Context

Aether already described device control as a capability with a permission,
risk level, confirmation policy, and audit contract. The automation HTTP
action endpoint nevertheless called `InstanceManager::execute_action`
directly. CLI and MCP reached the same direct endpoint, so the metadata was
descriptive rather than an enforced runtime boundary.

The standalone runtime cannot introduce Redis or PostgreSQL merely to audit a
command. It also must preserve the process boundary between aether-api,
aether-automation, and aether-io.

## Decision

1. External instance actions enter the transport-neutral
   `ControlApplication` use case before automation routing or SHM dispatch.
2. The use case requires `device.control`, explicit confirmation, a finite
   command value, and mandatory audit records. It fails closed if the first
   durable audit record cannot be written.
3. aether-automation independently verifies the original signed access JWT
   before deriving actor ID and role. Identity and role forwarding headers are
   never authentication credentials, even on loopback.
4. The local CLI and MCP client use the same automation HTTP use case and must
   present `AETHER_ACCESS_TOKEN` from an authenticated Admin or Engineer
   session. Loopback reachability alone grants no command authority.
5. The local uplink process accepts cloud `inst:A` commands through the same
   automation use case. It presents the separately generated
   `AETHER_UPLINK_CONTROL_TOKEN`; automation maps that credential to the fixed
   `local:aether-uplink` actor and ignores caller-supplied identity headers.
   Direct cloud `io:C/A` commands remain rejected.
6. Command audit events are stored in automation's local SQLite database by
   the reusable `aether-store-local` adapter.
   External audit systems may mirror those events through an extension but
   are never required for the default distribution.
7. The command continues through the existing routing cache, channel-health
   gate, SHM command slot, and UDS notification. SHM remains the live-state and
   command-transport authority.
8. aether-io's public `/write` endpoint rejects C/A writes. T/S simulation
   writes are also disabled by default and require the explicit
   `AETHER_ALLOW_SIMULATION_WRITES=true` development opt-in, because forged
   measurements can trigger real rules. Direct C/A CLI and MCP tools are
   removed; all external device control uses instance actions.
9. Internal deterministic rule execution is not reclassified as an external
   actor by this change. Its application/audit convergence is a separate
   migration step.

## Consequences

### Positive

- HTTP, CLI, and MCP instance actions enforce the same permission,
  confirmation, deadline, and audit policy.
- Missing or malformed identity is denied and recorded instead of silently
  falling through to the device dispatcher.
- Forged `x-aether-auth-source`, `x-aether-actor-id`, and
  `x-aether-actor-role` headers do not create an authenticated actor.
- Process isolation and the no-external-service default are preserved.
- A failed audit database disables new external commands before dispatch.
- The public command surface has one device-control operation instead of
  competing instance and direct-channel variants.

### Negative

- Direct callers of the loopback automation endpoint must provide a valid
  access JWT or the dedicated uplink service credential plus confirmation.
- CLI device control requires an authenticated access token instead of ambient
  local-user trust.
- Deployments that bind aether-automation to a non-loopback address need a
  stronger service-to-service authentication adapter.
- Internal rule-engine actions still require a separate audit convergence
  decision because they are deterministic runtime behavior, not external
  actors.

## Verification

```bash
cargo test -p aether-application --test application_contract
cargo test -p aether-automation --test test_application_control
cargo test -p aether-api token_validation_does_not_emit_spoofable_identity_headers
cargo test -p aether-io test_simulation_writes_are_disabled_by_default
./scripts/check-architecture.sh
```
