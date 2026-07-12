# ADR-0008: Route external device actions through the application boundary

## Status

Accepted and implemented for external device actions on 2026-07-11. The
nonexistent instance-measurement compatibility surface and the rule engine's
direct action-dispatch path were removed on 2026-07-12. Physical action-route
mutations joined the same application boundary on 2026-07-12.

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
9. Each device action produced by deterministic rule execution enters the
   shared `ControlApplication` through an automation-owned, transport-neutral
   facade. The facade creates a unique UUID-backed `CommandId` and matching
   audit request ID, supplies the fixed commissioned
   `local:aether-automation-rule-engine` service actor with the exact
   `device.control` permission, and sets explicit confirmation. The existing
   safety policy still evaluates that permission and confirmation; the facade
   does not bypass authorization. This applies equally to scheduled rules and
   manually triggered rules.
10. The former CLI/MCP `models instances measurement` surface is removed rather
    than preserved as a compatibility shim. Automation has no matching HTTP
    route and must not gain a `LiveStateWriter` to recreate one. Synthetic T/S
    acquisition remains available only through io's explicit development-only
    simulation entry point, gated by `AETHER_ALLOW_SIMULATION_WRITES=true`; it
    is not an instance-state correction mechanism and must never become an
    automation write path.
11. Manual rule execution is a separate high-risk
    `automation.rule.execute` application command. HTTP, CLI, and MCP must
    present the same signed Admin/Engineer identity and explicit confirmation;
    the application audits the human invocation before calling the
    deterministic runtime. Each resulting device action also receives its own
    attempted plus succeeded/failed audit records under the commissioned
    service identity described in item 9. If any attempted action fails, the
    aggregate rule execution fails, the manual application boundary reports a
    failed invocation, and scheduled execution does not start its cooldown.
12. The unpublished `RuleExecutor::with_action_dispatch` compatibility API and
    its direct `aether_rtdb_shm::ActionDispatch` field were removed after all
    production and test composition roots moved to the governed facade. No
    compatibility shim remains. Reintroduction is forbidden unless a staged
    migration has an explicit owner and removal criterion; direct SHM command
    dispatch from `aether-rules` is not an accepted fallback.
13. Action-route upsert, delete, and enablement are the high-risk
    `automation.routing.manage` command. HTTP, CLI, and MCP use one
    `ActionRoutingApplication`; generic compatibility batch routes reject
    action entries. The SQLite adapter validates the logical action and the
    physical C/A target, commits the mutation, then republishes the runtime
    command map. Publication failure clears the map so a stale physical target
    cannot remain authoritative. `aether sync` must fail closed for action
    entries until a governed batch command exists.

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
- Failure to persist a rule action's attempted record disables that action
  before it reaches the SHM-backed `CommandDispatcher`.
- Every valid rule device action has a distinct command/audit correlation ID;
  while the audit sink remains available, `ControlApplication` persists an
  attempted plus terminal succeeded/failed pair.
- The public command surface has one device-control operation instead of
  competing instance and direct-channel variants.
- Physical command topology changes are signed, confirmed, audited, and
  correlated consistently across HTTP, CLI, and MCP.
- A failed routing publication revokes the runtime map instead of leaving a
  previously commissioned command target active.

### Negative

- Direct callers of the loopback automation endpoint must provide a valid
  access JWT or the dedicated uplink service credential plus confirmation.
- CLI device control requires an authenticated access token instead of ambient
  local-user trust.
- Deployments that bind aether-automation to a non-loopback address need a
  stronger service-to-service authentication adapter.
- Commissioned rule actions are attributed to a service identity rather than
  the human who manually invoked the containing rule. The outer rule audit
  retains the human identity and the per-device records retain their unique
  command correlation IDs.

## Verification

```bash
cargo test -p aether-application --test application_contract
cargo test -p aether-automation --test test_application_control
cargo test -p aether-automation --test test_rule_execution_boundary
cargo test -p aether-automation --test test_rule_action_control
cargo test -p aether-application --test rule_execution_application
cargo test -p aether-api token_validation_does_not_emit_spoofable_identity_headers
cargo test -p aether-io test_simulation_writes_are_disabled_by_default
cargo test -p aether retired_instance_measurement_write_is_absent_from_capability_surfaces
cargo test -p aether --test removed_instance_measurement_surface
cargo test -p aether-application --test action_routing_application
cargo test -p aether-automation --test test_action_routing_boundary
cargo test -p aether mcp::tests::routing_action
./scripts/check-architecture.sh
```
