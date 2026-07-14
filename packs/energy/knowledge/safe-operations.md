---
title: Safe Operations for AI Agents
description: Which writes reach real devices, how write gating works, and the operating rules an AI agent must follow
updated: 2026-07-12
---

# Safe Operations for AI Agents

Aether controls real equipment: PCS inverters, battery stacks, diesel generators. The `aether mcp` server exposes this system to AI agents, and some of its tools move real hardware. This page is the safety contract: which tools are dangerous and why, how the write gate actually works, which state-reading mistakes lead to bad decisions, and the rules an agent must follow when operating the system.

## The write surface

The production MCP catalog has 44 tools: 23 read-only tools that are always
registered and 21 governed write tools that exist only when the server is
started with `--allow-write`. The static `MCP_WRITE_CAPABILITY_MAPPING` in
`tools/aether/src/mcp.rs` maps those tools to the transport-neutral application
capability catalog.

### Device-affecting — these reach physical equipment

These calls can initiate commands that move real hardware: a PCS power setpoint
may change, a breaker may close, or a generator may start or stop. Tool success
only reports local command-plane acceptance; physical completion requires
read-back verification.

| Tool | Application capability | Meaning of success |
|------|------------------------|--------------------|
| `models_instances_action` | `device.write_point` | The local command plane accepted the command |
| `rules_execute` | `automation.rule.execute` | The rule was evaluated and selected commands were accepted or rejected locally |

(T/S/C/A are the four channel point types — telemetry, signal, control, adjustment; M denotes an instance measurement point — see [Data Model](../concepts/data-model.md).)

`models_instances_action` is the only external point-control tool. It addresses
a device instance action, which the routing layer resolves to a channel point
and dispatches through shared memory to io and out to the device. Direct
channel C/A tools were removed so an agent cannot bypass instance routing,
confirmation, and command audit.

### Physical command topology — these change where future commands go

These tools do not execute equipment when they return, but they change the
physical C/A point selected by later logical actions. Treat them with the same
care as device control: inspect the current route, require explicit operator
intent, and do not retry an incomplete audit or publication response.

| Tool | Application capability | Meaning of success |
|------|------------------------|--------------------|
| `routing_action_upsert` | `automation.routing.manage` | The route was persisted and its committed command map was published |
| `routing_action_delete` | `automation.routing.manage` | The route was removed and the command map was republished |
| `routing_action_set_enabled` | `automation.routing.manage` | The route's enabled state was persisted and republished |

### Rule, alarm-policy, and alert-state mutations

These writes do not necessarily move equipment when they return, but they
change future automation, detection, or operator-visible alert state. An
enabled rule may later dispatch device commands, an alarm-rule edit changes
what conditions are reported, and resolving an alert clears only its current
active indication.

| Tool | Application capability | Mutation |
|------|------------------------|----------|
| `rules_create` | `automation.rule.manage` | Create a disabled business-rule shell |
| `rules_update` | `automation.rule.manage` | Update a business rule |
| `rules_delete` | `automation.rule.manage` | Delete a business rule |
| `rules_enable` | `automation.rule.manage` | Enable a business rule |
| `rules_disable` | `automation.rule.manage` | Disable a business rule |
| `alarms_rule_create` | `alarm.rule.manage` | Create an alarm rule |
| `alarms_rule_update` | `alarm.rule.manage` | Update an alarm rule |
| `alarms_rule_delete` | `alarm.rule.manage` | Delete an alarm rule |
| `alarms_rule_enable` | `alarm.rule.manage` | Enable an alarm rule |
| `alarms_rule_disable` | `alarm.rule.manage` | Disable an alarm rule |
| `alarms_resolve` | `alarm.alert.resolve` | Resolve one active alert indication |

### Channel commissioning and lifecycle mutations

These commands change the authoritative desired channel configuration and can
stop or redirect data acquisition. They share the `io.channel.manage`
application capability and are high-risk, confirmed, audited, and
non-idempotent.

| Tool | Mutation |
|------|----------|
| `channels_create` | Create a channel disabled by default unless `enabled` is explicitly true |
| `channels_update` | Patch channel desired configuration, optionally guarded by `expected_revision` |
| `channels_delete` | Delete the channel and measurement-owned dependents; action-route references cause a conflict |
| `channels_enable` | Request an active runtime projection, optionally guarded by `expected_revision` |
| `channels_disable` | Fence the runtime projection, optionally guarded by `expected_revision` |

A successful channel mutation can still report `runtime_projection` as
`degraded`. That means desired state committed but runtime reconciliation is
required; it is not a safe retry signal. Preserve `request_id`, inspect
`resulting_revision` and `reconciliation_required`, then inspect current state
and audit records before an operator authorizes any follow-up.

### Data-integrity mutations are excluded from MCP

| Compatibility surface | Status |
|------|----------------------------------|
| io channel simulation write | Available only through explicit development CLI/HTTP paths; not an MCP tool |

This tool does not touch a device, which makes it look safe. It is not.
It writes into acquisition live state, and downstream consumers treat the
value as telemetry. Alarm rules can trigger (or fail to trigger), control rules
can compute actions, and dashboards can display the injected value as truth.
Never use it against a system connected to real equipment except in a
deliberate, supervised test. Direct instance-measurement writes are not an
available CLI, MCP, or automation HTTP capability; automation must not be given
a live-state writer to recreate one. `channels_write` is disabled by default at
the io service and
returns 403 unless the operator explicitly starts io with
`AETHER_ALLOW_SIMULATION_WRITES=true` in an isolated development environment.

### Remaining configuration mutations stay excluded from MCP

These remaining compatibility operations change live-state inputs or
persisted configuration. Channel point batches can reshape acquired data,
while MQTT or certificate changes can disconnect or redirect cloud traffic.
They are not made safe merely by being configuration rather than immediate
device commands.

| Area | Existing compatibility operations (not MCP tools) |
|------|-------|
| Channel point batch | `channels_points_batch` |
| Cloud connectivity (MQTT, certificates) | `net_mqtt_config_set`, `net_mqtt_reconnect`, `net_mqtt_disconnect`, `net_cert_upload`, `net_cert_delete` |

Channel point-batch and uplink operations remain outside MCP until both their
application boundary and explicit capability mapping have been reviewed.
Channel lifecycle, rule and alarm-rule mutations, and alert resolution are
exposed only through the exact governed mappings above. `--allow-write` never
promotes a wrapper automatically.

## How write gating works

`aether mcp` starts the server with only the 23 read-only tools registered.
`aether mcp --allow-write` additionally merges a `ToolRouter` containing only
the 21 governed writes. This is registration-time gating, decided once at
startup in `AetherMcp::new` (`tools/aether/src/mcp.rs`). It is not confirmation:
each invocation must independently send `confirmed: true`.

The consequence: when `--allow-write` is off, the write tools are **absent from the `tools/list` response** — not present-but-flagged, absent. An AI client cannot call what it cannot see, so the safety property holds regardless of how capable or how misaligned the model is, and regardless of how the client is configured.

Contrast this with MCP's `readOnlyHint` annotation. In the implementation,
read-only tools carry no annotation; the 21 write tools are marked
`annotations(read_only_hint = false)`. The hint is advisory and does not
replace signed authorization, per-call confirmation, or audit. The generated
public surface is listed in [MCP Tools Reference](../../../docs/reference/mcp-tools.md).

Tests in `tools/aether/src/mcp.rs` assert the exact 23+21 route counts, verify
that excluded mutation names remain absent even with `--allow-write`, and
require every exposed write to exist in `aether_application::capability_catalog()`
as a command with `Always` confirmation and `Required` audit.

For every governed write, the MCP bridge reads `AETHER_ACCESS_TOKEN`, sends it
as an `Authorization: Bearer` credential, and generates an `X-Request-ID` for
the HTTP request. Preserve the returned `request_id` and any `command_id`.
Timeouts, `audit.status="incomplete"`, and incomplete route-publication results
do not prove that nothing happened, so they must never trigger an automatic
retry. Inspect current state and audit records before an operator decides
whether to issue a separately confirmed follow-up command.

Registration is only the first gate. Every device command has an exclusive
deadline (5 seconds by default), and adjustment points persist an inclusive
minimum, maximum, and positive step. Automation validates the resolved point
policy before dispatch; the UDS listener rejects expired frames; and io's
per-channel `CommandGuard` repeats point existence, deadline, finite-value,
range, and step validation immediately before the protocol adapter touches
hardware. A batch is dispatched only after every member passes. The existing
`producer_id + seq` pair is the transport request identity, so this safety
envelope does not add an external queue or database dependency.

External instance actions have an additional application boundary. A signed
HTTP session becomes a `RequestContext`; uplink uses its separately generated
service credential. Loopback access and caller-supplied identity headers grant
no authority. The
`device.write_point` capability then requires the `device.control` permission
and explicit confirmation. Rejected, attempted, succeeded, and failed outcomes
are written to `command_audit_events` in automation's local SQLite database.
If the mandatory pre-dispatch audit cannot be stored, the command is not sent.
Redis and PostgreSQL are not involved. See
[ADR-0008](../../../docs/adr/0008-application-control-boundary.md) for the trust boundary.

## Reading state correctly

Three properties of Aether's data model routinely mislead agents that assume a conventional "device object with a status field" design. Misreading any of them can turn a well-intentioned write into a harmful one. See [Data Model](../concepts/data-model.md) for the full picture.

**1. NaN means "temporarily unavailable" — never zero, never "off".** Measurement slots in shared memory initialize to an IEEE-754 quiet NaN sentinel (`SLOT_UNWRITTEN_BITS` in `crates/aether-dataplane/src/core/slot.rs`), the explicit "no data has ever been written here" marker. The source is explicit about why: it "avoids the historical 0.0 ambiguity where a default-initialised slot was indistinguishable from a real device reading of zero." If a battery's power reading is NaN, the battery is not idle and not off — the value is unknown, most likely because the channel has not delivered data yet. Any computation that coerces NaN to 0 (a sum of feeder powers, a state-of-charge average) produces a plausible-looking wrong number. HTTP and MCP readers resolve the same SHM state, so they must preserve that unavailable outcome rather than inventing a value.

**2. Channel connectivity is per-channel state, not an instance attribute.** io publishes each channel's online state and heartbeat into the dedicated channel-health SHM segment. A missing or stale entry means "unknown", not "online". This status is deliberately **not** aggregated onto instances — an instance has no `online` field, and its measurement values do not change shape when its channel drops (they simply stop updating or read NaN). Before writing to a device, resolve which channel serves it and check that channel with the read-only `channels_status` tool. A write dispatched toward an offline channel does not reach the device.

**3. Alarms are an event stream, not an instance state.** Alarms live in alarm's own tables (`Alert` for active alarms, `AlertEvent` for the trigger/recovery history — `services/alarm/src/models.rs`), addressing points by `service_type` + `channel_id` + `data_type` + `point_id`. They reference the measurement plane; they are never written back into it. An instance with three active high-severity alarms is byte-for-byte identical, in its measurement values, to a healthy one. If your task is "is this device okay to operate", reading its measurements is not enough — query the alarm tools (`alarms_list`) as a separate step.

## Operating rules

An AI agent operating Aether follows these rules verbatim:

1. **Prefer read-only mode.** Run against `aether mcp` (no flag) by default; request `--allow-write` only for a task that actually needs it, and drop back afterward.
2. **Before any device write, check the channel is online and read the current value.** Resolve the instance or point to its channel, confirm connectivity via `channels_status`, and read the present value so you know what you are changing and by how much.
3. **Never invent a measurement correction path.** Instance measurements are read-only projections of acquisition data. Synthetic input is allowed only at io's explicitly enabled development simulation boundary, never through automation.
4. **Treat NaN and absent fields as unknown, not zero.** Exclude NaN readings and missing fields from aggregates, and never base a control decision on them.
5. **After a write, read back and verify.** A returned success means the local command plane accepted the command, not that the device executed it or reached the target state. Read the corresponding measurement and confirm it moved as expected.
6. **Do not invent MCP configuration tools.** The write allowlist is exactly the 21 tools above. It includes the five governed channel CRUD/lifecycle tools, rule CRUD/lifecycle, alarm-rule CRUD/lifecycle, alert resolution, and the three governed action-routing tools; channel point-batch and uplink mutations remain absent.
7. **Never automatically retry a write.** A skipped action is a report, while a timeout or incomplete audit/publication result may hide a committed action. Preserve its request identity, inspect current state and audit records, and require a fresh operator decision before another confirmed invocation. A retry loop against an offline generator start command becomes a queue of surprises when the link recovers.

## Related pages

- [System Architecture](../concepts/architecture.md) — the services these tools talk to
- [Data Model](../concepts/data-model.md) — instances, channels, points, and why they are orthogonal
- [Using Aether with AI Assistants](../guides/ai-assistants.md) — setting up the MCP server
- [CLI Reference](../reference/cli.md) — the `aether` commands behind each tool
