---
title: Safe Operations for AI Agents
description: Which writes reach real devices, how write gating works, and the operating rules an AI agent must follow
updated: 2026-07-11
---

# Safe Operations for AI Agents

Aether controls real equipment: PCS inverters, battery stacks, diesel generators. The `aether mcp` server exposes this system to AI agents, and some of its tools move real hardware. This page is the safety contract: which tools are dangerous and why, how the write gate actually works, which state-reading mistakes lead to bad decisions, and the rules an agent must follow when operating the system.

## The write surface

The MCP server has 48 tools: 23 read-only tools that are always registered, and 25 write tools that exist only when the server is started with `--allow-write` (the full list is the `WRITE_TOOL_NAMES` constant in `tools/aether/src/mcp.rs`). The 25 write tools fall into three severity groups.

### Device-affecting — these reach physical equipment

A successful call moves real hardware: a PCS power setpoint changes, a breaker closes, a generator starts or stops. The tool descriptions in the source carry the warning verbatim.

| Tool | Description (quoted from source) |
|------|----------------------------------|
| `models_instances_action` | "Execute a control action on an instance. This writes to a real device via SHM + io." |

(T/S/C/A are the four channel point types — telemetry, signal, control, adjustment; M denotes an instance measurement point — see [Data Model](../concepts/data-model.md).)

`models_instances_action` is the only external point-control tool. It addresses
a device instance action, which the routing layer resolves to a channel point
and dispatches through shared memory to io and out to the device. Direct
channel C/A tools were removed so an agent cannot bypass instance routing,
confirmation, and command audit.

### Data-integrity — forges live telemetry

| Tool | Description (quoted from source) |
|------|----------------------------------|
| `channels_write` | "Inject a simulated T/S value into the acquisition SHM plane. This does not command a device, but downstream rules and alarms treat it as telemetry." |
| `models_instances_measurement` | "Set a measurement value on an instance. This overwrites the live inst:{id}:M value -- the same field real device telemetry populates -- so rules, alarms, and dashboards will treat it as genuine until the next real update arrives." |

These tools do not touch a device, which makes them look safe. They are not.
They write into acquisition or instance live state, and downstream consumers
treat the values as telemetry. Alarm rules can trigger (or fail to trigger),
control rules can compute actions, and dashboards can display the injected
value as truth. Never use them against a system connected to real equipment
except in a deliberate, supervised test.
For that reason, `channels_write` is disabled by default at the io service and
returns 403 unless the operator explicitly starts io with
`AETHER_ALLOW_SIMULATION_WRITES=true` in an isolated development environment.

### Configuration — no immediate device effect, but governs future behavior

These tools change persisted configuration. Nothing moves when the call returns, but the changed configuration decides what the system does from then on: a modified alarm rule decides what gets alerted, a modified control rule decides what gets commanded, a deleted channel silently stops data collection.

| Area | Tools |
|------|-------|
| Channel CRUD and lifecycle | `channels_create`, `channels_update`, `channels_delete`, `channels_enable`, `channels_disable`, `channels_points_batch` |
| Rule CRUD and lifecycle | `rules_create`, `rules_update`, `rules_delete`, `rules_enable`, `rules_disable`, `rules_execute` |
| Alarm rule CRUD and lifecycle | `alarms_rule_create`, `alarms_rule_update`, `alarms_rule_delete`, `alarms_rule_enable`, `alarms_rule_disable` |
| Cloud connectivity (MQTT, certificates) | `net_mqtt_config_set`, `net_mqtt_reconnect`, `net_mqtt_disconnect`, `net_cert_upload`, `net_cert_delete` |

Two entries in this group deserve extra care. `rules_execute` runs a rule's action branch immediately — if the rule commands a device, the command is dispatched for real, so treat it with the same caution as the device-affecting group. And `net_mqtt_disconnect` / `net_cert_delete` can sever the site's cloud link; on a remotely managed installation that may be the connection you are operating through.

## How write gating works

`aether mcp` starts the server with only the 23 read-only tools registered. `aether mcp --allow-write` additionally merges in a second `ToolRouter` containing the 25 write tools. This is registration-time gating, decided once at startup in `AetherMcp::new` (`tools/aether/src/mcp.rs`).

The consequence: when `--allow-write` is off, the write tools are **absent from the `tools/list` response** — not present-but-flagged, absent. An AI client cannot call what it cannot see, so the safety property holds regardless of how capable or how misaligned the model is, and regardless of how the client is configured.

Contrast this with MCP's `readOnlyHint` annotation. In the implementation, read-only tools carry no annotation at all; only the 25 write tools are explicitly marked `annotations(read_only_hint = false)`. The hint is advisory: a client may be configured to auto-approve everything, or may not honor the annotation at all. The hint helps well-behaved clients present confirmation prompts; it is not the gate. The gate is registration. The generated public surface is listed in [MCP Tools Reference](../reference/mcp-tools.md).

A test in `tools/aether/src/mcp.rs` (`write_router_is_empty_without_allow_write`) asserts that no name in `WRITE_TOOL_NAMES` appears in the read-only router — with a route-count safety net of exactly 23 tools — so the gating is guarded against regression.

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
HTTP session becomes a `RequestContext`; CLI/MCP must provide the same signed
token through `AETHER_ACCESS_TOKEN`, while uplink uses its separately generated
service credential. Loopback access and caller-supplied identity headers grant
no authority. The
`device.write_point` capability then requires the `device.control` permission
and explicit confirmation. Rejected, attempted, succeeded, and failed outcomes
are written to `command_audit_events` in automation's local SQLite database.
If the mandatory pre-dispatch audit cannot be stored, the command is not sent.
Redis and PostgreSQL are not involved. See
[ADR-0008](../adr/0008-application-control-boundary.md) for the trust boundary.

## Reading state correctly

Three properties of Aether's data model routinely mislead agents that assume a conventional "device object with a status field" design. Misreading any of them can turn a well-intentioned write into a harmful one. See [Data Model](../concepts/data-model.md) for the full picture.

**1. NaN means "temporarily unavailable" — never zero, never "off".** Measurement slots in shared memory initialize to an IEEE-754 quiet NaN sentinel (`SLOT_UNWRITTEN_BITS` in `libs/aether-rtdb-shm/src/core/slot.rs`), the explicit "no data has ever been written here" marker. The source is explicit about why: it "avoids the historical 0.0 ambiguity where a default-initialised slot was indistinguishable from a real device reading of zero." If a battery's power reading is NaN, the battery is not idle and not off — the value is unknown, most likely because the channel has not delivered data yet. Any computation that coerces NaN to 0 (a sum of feeder powers, a state-of-charge average) produces a plausible-looking wrong number. HTTP and MCP readers resolve the same SHM state, so they must preserve that unavailable outcome rather than inventing a value.

**2. Channel connectivity is per-channel state, not an instance attribute.** io publishes each channel's online state and heartbeat into the dedicated channel-health SHM segment. A missing or stale entry means "unknown", not "online". This status is deliberately **not** aggregated onto instances — an instance has no `online` field, and its measurement values do not change shape when its channel drops (they simply stop updating or read NaN). Before writing to a device, resolve which channel serves it and check that channel with the read-only `channels_status` tool. A write dispatched toward an offline channel does not reach the device.

**3. Alarms are an event stream, not an instance state.** Alarms live in alarm's own tables (`Alert` for active alarms, `AlertEvent` for the trigger/recovery history — `services/alarm/src/models.rs`), addressing points by `service_type` + `channel_id` + `data_type` + `point_id`. They reference the measurement plane; they are never written back into it. An instance with three active high-severity alarms is byte-for-byte identical, in its measurement values, to a healthy one. If your task is "is this device okay to operate", reading its measurements is not enough — query the alarm tools (`alarms_list`) as a separate step.

## Operating rules

An AI agent operating Aether follows these rules verbatim:

1. **Prefer read-only mode.** Run against `aether mcp` (no flag) by default; request `--allow-write` only for a task that actually needs it, and drop back afterward.
2. **Before any device write, check the channel is online and read the current value.** Resolve the instance or point to its channel, confirm connectivity via `channels_status`, and read the present value so you know what you are changing and by how much.
3. **Never use `models_instances_measurement` to "correct" data.** It does not fix anything — it forges telemetry, and every downstream consumer (alarm rules, control rules, dashboards) will act on the forgery as if a device reported it.
4. **Treat NaN and absent fields as unknown, not zero.** Exclude NaN readings and missing fields from aggregates, and never base a control decision on them.
5. **After a write, read back and verify.** A returned success means the command was dispatched, not that the device reached the target state. Read the corresponding measurement and confirm it moved as expected.
6. **Configuration deletes are not undoable.** `channels_delete`, `rules_delete`, and `alarms_rule_delete` permanently remove configuration. Enumerate first (`channels_list`, `rules_list`, `alarms_rules_list`), confirm the exact target by ID, and state what you are about to delete before calling.
7. **A skipped action is a report, not an error to retry blindly.** When the system declines a write — typically because the target channel is offline — that outcome is informative. Investigate why the channel is offline before retrying; a retry loop against an offline generator start command becomes a queue of surprises when the link recovers.

## Related pages

- [System Architecture](../concepts/architecture.md) — the services these tools talk to
- [Data Model](../concepts/data-model.md) — instances, channels, points, and why they are orthogonal
- [Using Aether with AI Assistants](../guides/ai-assistants.md) — setting up the MCP server
- [CLI Reference](../reference/cli.md) — the `aether` commands behind each tool
