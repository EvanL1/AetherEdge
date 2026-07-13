---
title: Writing Rules
description: Author rules through the HTTP API or a downstream product console
updated: 2026-07-10
---

# Writing Rules

Rules are Aether's control logic: flows that read measurement points (M),
evaluate conditions, and write action points (A). They execute inside automation
(port 6002) and are authored through the application API, either directly or
through a downstream product console. This guide covers the authoring
mechanics; for how the engine schedules and executes rules, see
[Rule Engine](../concepts/rule-engine.md), and for a worked control
strategy, see [Control Strategies](../domain/control-strategies.md).

## Anatomy of a rule

A rule row in the SQLite `rules` table carries:

- **`id`** — an auto-assigned integer; you never choose it.
- **`name`** and **`description`** — for humans.
- **`enabled`** — new rules start disabled; the scheduler skips disabled
  rules entirely.
- **`priority`** — orders evaluation when several rules are due; see
  [Control Strategies](../domain/control-strategies.md) for how priority
  combines with mutually exclusive conditions to arbitrate between rules
  that write the same actuator.
- **`cooldown_ms`** — a minimum gap after a successful execution that
  performed at least one action, suppressing re-execution until it elapses.
- **`trigger_config`** — when the rule runs. Two variants, discriminated by
  `"type"`:
  - `{"type": "interval", "interval_ms": 1000}` — periodic evaluation on
    the scheduler tick. Rules with no `trigger_config` default to a
    1000 ms interval (or to their `cooldown_ms` as the period, if set).
  - `{"type": "on_change", "point_refs": [{"instance": 1, "point_type":
    "measurement", "point": 0}], "time_deadband_ms": 200,
    "value_deadband": null}` — event-driven evaluation when a subscribed
    point changes, filtered by a time deadband (minimum gap between
    triggers) and an optional value deadband (absolute or percent change
    threshold).
- **The flow** — the logic itself: a start node fanning out to input nodes
  (read a measurement point or load configuration parameters), through
  decision nodes (condition branches), to action nodes (write an action
  point), ending at an end node.

The flow is stored twice — `flow_json`, the full visual-editor document,
and `nodes_json`, the compact topology the engine executes — and the two
columns are always derived together from the editor document by one
function. [Rule Engine](../concepts/rule-engine.md) explains why.

## Via a product console

The independent [AetherEMS](https://github.com/EvanL1/AetherEMS) Console
provides a Vue Flow rule editor. It edits the complete visual document — nodes
with canvas positions, labels, edges, and viewport — and submits that document
through the same authenticated rule command API. AetherIot does not bundle the
Console or grant it direct SQLite/SHM access. The server derives both stored
representations together, so `flow_json` and the execution topology cannot
drift (see [Rule Engine](../concepts/rule-engine.md) for the invariant).

## Via the HTTP API

automation serves the rule API (`services/automation/src/rule_routes.rs`):
Swagger UI at `http://localhost:6002/docs` is the per-operation contract source.
Every mutation below accepts only a Bearer Admin/Engineer actor, requires
`confirmed: true`, and writes mandatory audit records before changing SQLite
or reloading the scheduler.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/rules` | Paginated list (summary fields: id, name, enabled, description) |
| POST | `/api/rules` | Create a metadata-only stub |
| GET | `/api/rules/{id}` | Full rule, including both flow columns |
| PUT | `/api/rules/{id}` | Partial update; the flow and trigger land here |
| DELETE | `/api/rules/{id}` | Delete the rule |
| POST | `/api/rules/{id}/enable` | Set enabled |
| POST | `/api/rules/{id}/disable` | Set disabled |
| POST | `/api/rules/{id}/execute` | Execute immediately (real execution — see below) |
| GET | `/api/rules/{id}/variables` | Variables the rule reads, for monitoring |
| GET | `/api/scheduler/status` | Scheduler running flag, rule counts, tick interval |
| POST | `/api/scheduler/reload` | Force re-read of all rules from SQLite |

Creating a rule is a **two-step reality**: `POST /api/rules` accepts only a
name and description and inserts a stub — empty `{}` topology, no editor
document, disabled. The flow content only lands via `PUT /api/rules/{id}`:

```bash
# 1. Create the stub; the response carries the assigned id
curl -X POST http://localhost:6002/api/rules \
  -H "Authorization: Bearer $AETHER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name": "Battery SOC Protection", "description": "Protect battery when SOC is too low", "confirmed": true}'
# → {"success": true, "data": {"id": 3, "name": "Battery SOC Protection", "status": "created"}}

# 2. Write the flow and trigger
curl -X PUT http://localhost:6002/api/rules/3 \
  -H "Authorization: Bearer $AETHER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d @rule.json

# 3. Enable it
curl -X POST http://localhost:6002/api/rules/3/enable \
  -H "Authorization: Bearer $AETHER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"confirmed": true}'
```

where `rule.json` supplies the editor document and trigger:

```json
{
  "flow_json": {
    "nodes": [
      {"id": "start", "type": "start", "position": {"x": 0, "y": 0},
       "data": {"config": {"wires": {"default": ["end"]}}}},
      {"id": "end", "type": "end", "position": {"x": 100, "y": 0}}
    ],
    "edges": []
  },
  "trigger_config": {"type": "interval", "interval_ms": 1000},
  "confirmed": true
}
```

That flow is the minimal valid document (it does nothing); for a full
strategy with input, decision, and action nodes, see the shipped template
`packs/energy/rules/battery_soc_management.json`, which
[Control Strategies](../domain/control-strategies.md) walks through node by
node. A malformed flow fails the PUT as a unit — nothing is stored — and a
malformed `trigger_config` is rejected at the same boundary.

The `aether` CLI wraps the same endpoints. Set `AETHER_ACCESS_TOKEN` and pass
`--confirmed` to `rules create`, `update`, `enable`, `disable`, and `delete`;
`delete --force` only skips the interactive prompt. `rules list` remains
read-only.

## Testing a rule

**There is no dry-run.** `POST /api/rules/{id}/execute` — and equally the
`rules_execute` MCP tool and `aether rules execute <id> --confirmed` — performs
a real execution through the authenticated, confirmed, and audited application
command: the flow is evaluated against live values, and any action that fires
is submitted to the local command plane. Acceptance does not prove that the
physical device executed the command or reached the target value.

So test against hardware that does not exist yet. The Virtual protocol has
no feature gate precisely so it is always available for this:

1. Create a Virtual-protocol channel with control and adjustment points
   matching what the rule will write, and route a scratch instance's action
   points to it (see [Connect Devices](connect-devices.md)).
2. Point the rule's actions at the scratch instance and execute:

   ```bash
   AETHER_ACCESS_TOKEN='<signed access JWT>' \
     aether rules execute 3 --confirmed
   ```

3. Check the result. The command response reports `actions_attempted` and
   `actions_succeeded`, where success means local command-plane acceptance.
   Read back the corresponding measurements to verify physical behavior. The
   detailed execution path and action outcomes remain persisted locally in
   SQLite `rule_history` for API and WebSocket readers.

4. Once the branch selection and written values look right, re-target the
   rule's actions at the production instance and enable it.

## Reload

You normally never think about reloading: all five rule CRUD endpoints —
create, update, delete, enable, disable — trigger a scheduler reload after
their database write, so changes made through the API or the editor take
effect immediately, with no service restart.

The explicit `POST /api/scheduler/reload` exists for out-of-band writes:
a bulk import or `aether sync` pushing rule files into SQLite behind the
scheduler's back. After such a write, hit the endpoint once and the
scheduler re-reads every enabled rule and rebuilds its on-change
subscriptions atomically. `GET /api/scheduler/status` confirms the result —
`running`, total and enabled rule counts, and the tick interval.

## Related pages

- [Rule Engine](../concepts/rule-engine.md) — dual-column storage, scheduling, execution, hot reload
- [Control Strategies as Rules](../domain/control-strategies.md) — expressing SOC management and peak shaving as flows
- [Connect Devices](connect-devices.md) — channels, Virtual protocol, point mapping
