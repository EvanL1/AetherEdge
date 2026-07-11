---
title: Rule Engine
description: Dual-column rule storage, tick and event scheduling, execution, and hot reload
updated: 2026-07-10
---

# Rule Engine

Aether's rule engine lives in the `aether-rules` library and runs inside
automation. It has three parts: a parser that turns the visual editor's flow
document into a compact execution topology, a scheduler that decides when each
rule runs, and an executor that walks the topology, evaluates conditions, and
writes action points. This page covers the engine mechanics; for how to express
control strategies as rule flows (with a worked state-of-charge example), see
[Control Strategies](../domain/control-strategies.md).

## Two columns, one writer

Every rule persists in the SQLite `rules` table as two parallel columns:

- `flow_json` — the complete Vue Flow editor document: nodes with positions and
  labels, edges, viewport, metadata. This is what the visual editor loads to
  repopulate its canvas.
- `nodes_json` — the compact execution topology the engine actually runs: a
  `RuleFlow` with a `start_node` ID and a map of node ID to execution node. The
  parser (`extract_rule_flow`) keeps only what execution needs and discards
  positions, labels, and edge styling.

The invariant: both columns are always produced together by one function,
`aether_rules::flow_column_values()`, which parses the editor document and
returns a `FlowColumns` struct holding both serialized strings. The struct uses
named fields rather than a tuple so call sites cannot silently swap the two
values when binding SQL parameters, and an invalid flow fails as a unit — there
is no partial output. No code path serializes either column independently.

There are three production call sites, and any new write path to the `rules`
table must go through the same function:

- `repository::upsert_rule` in `libs/aether-rules/src/repository.rs` (used by
  rule import)
- the `PUT /api/rules/{id}` handler in `services/automation/src/rule_routes.rs`
- the config syncer in `tools/aether/src/core/syncer.rs` (`aether sync`)

One nuance: `POST /api/rules` creates a metadata-only stub — an empty `{}`
topology, a NULL editor document, and `enabled = false`. The flow content
always arrives later via PUT, which derives both columns together. Legacy rules
imported in compact-only form keep a NULL `flow_json`; their `nodes_json` still
comes from the same function.

Why this matters: if the columns diverged, the editor would display one logic
while the engine executed another — an operator auditing a strategy would be
reading a lie. Funneling every write through one producer makes that divergence
structurally impossible rather than a code-review concern.

## Scheduling

A single scheduler loop (`RuleScheduler::start`) multiplexes two inputs with
`tokio::select!`: a periodic tick (100 ms by default, `DEFAULT_TICK_MS`) and,
when the PointWatch event plane is wired in, a bounded channel of point-change
events. A rule declares one of two trigger types in its `trigger_config`
column:

- **Interval** (`{"type": "interval", "interval_ms": 1000}`) — the rule is due
  on any tick where the time since its last execution has reached
  `interval_ms`. Rules with no `trigger_config` default to a 1000 ms interval
  (or to their `cooldown_ms` as the period, if set).
- **OnChange** — the rule subscribes to specific measurement points (M) or
  action points (A) via `point_refs` and fires when a subscribed value changes
  beyond its deadbands.

OnChange rules are served by two paths running in parallel. The fast path is
event-driven: io publishes a PointWatch event when a subscribed point is
written, a dispatcher maps the `(channel, point)` pair to rule IDs, and the
scheduler evaluates those rules immediately. The event carries the new value,
so the trigger decision needs no read-back from the real-time database or
shared memory. The tick path is the fallback: each tick, the scheduler samples
all subscribed points in one batch (directly from shared memory when available)
and re-evaluates every OnChange rule — this covers multi-point rules and keeps
rules firing if the event socket is down.

Two deadbands filter noise, combined with AND semantics:

- `time_deadband_ms` — a rule-level frequency limit; the rule will not fire
  again until this much time has passed since its last trigger.
- `value_deadband` — either absolute (`|new - last| > threshold`) or percent.
  Without one, any change between finite values counts.

NaN values (Aether's "temporarily unavailable" sentinel) never count as a
change; the first finite value after a gap triggers once. After each trigger,
the per-point "last value" advances to what the executor actually read during
execution, so future comparisons are anchored to the value the rule logic
actually saw.

Due rules execute concurrently with bounded parallelism (four at a time by
default). Independently of triggers, a rule may declare a `cooldown_ms`; the
cooldown starts only after an execution that succeeded and performed at least
one action, and suppresses re-execution until it elapses.

## Execution

The executor walks the compact topology from the start node, following each
node's wires: switch nodes evaluate condition branches and select an output
wire, change nodes write a value to a point, calculation and period-delta nodes
compute derived values, and the end node terminates the path.

Input variables are read through the SHM-backed `RuleLiveState`, and the reads
are strict: if a variable's data is unavailable this cycle, the evaluation short-circuits rather
than substituting a default — a missing reading must never satisfy a condition
like `current < threshold` as if it were zero. The same discipline applies to
writes: a computed value that is NaN, infinite, or out of range is rejected and
the action recorded as failed, never coerced to a number.

Writes to action points take the command path: the executor resolves the instance's
model-to-channel route, then dispatches through the same `ActionDispatch` used
by automation's HTTP control endpoint — a write to the shared-memory command slots
(the C/A slots) followed by a Unix domain socket notify to io (see
[Shared Memory](shared-memory.md)). The dispatcher checks the shared-memory
writer generation before and after the write, so a rule firing across a io
restart is detected and dropped instead of landing in a stale slot.

When the target is unavailable — no shared-memory writer after a io
restart, a missing C/A slot, or a degraded notify socket — the action is
recorded as failed with a reason and the rule's success flag reflects it.
Commands are never queued for later delivery: the next cycle re-evaluates from
current values instead of replaying a stale setpoint against a device that has
since come back in a different state.

## Results and observability

Every execution produces a result record: success flag, the list of actions
executed (each with target, point, value, and its own success flag), the
execution path as visited node IDs, the matched condition, a snapshot of
variable values, and per-node execution details. The scheduler writes this
record to local durable and observable surfaces:

- to a per-rule log file, so each rule has an independent, greppable history;
- to SQLite `rule_history`, which keeps the structured execution result and
  error for API/WebSocket consumers.

Neither rule execution nor result observation requires an external database.

## Hot reload

`POST /api/scheduler/reload` calls `RuleScheduler::reload_rules`, which
re-reads all enabled rules from SQLite and replaces the scheduler's in-memory
rule set wholesale under a write lock — running executions finish against the
rule they started with. When the PointWatch handles are wired in (production
mode), the reload then rebuilds the event plane from the fresh rule set as a
unit: the subscription bitmap that tells io which points to publish events
for, and the dispatcher index that maps `(channel, point)` pairs to rule IDs.
A newly added or re-targeted OnChange rule starts receiving events immediately,
with no service restart.

In practice the endpoint is rarely needed: the rule CRUD endpoints (create,
update, delete, enable, disable) each trigger the same reload after their
database write. Explicit reload matters for out-of-band writes — a bulk import
or `aether sync` pushing rule files from configuration — where the scheduler
would otherwise not know the table changed.

## Related pages

- [Control Strategies as Rules](../domain/control-strategies.md) — expressing strategies as rule flows
- [Shared Memory](shared-memory.md) — the command slots and event plane the engine rides on
- [Data Flow](data-flow.md) — where rule execution sits in the end-to-end paths
- [Data Model](data-model.md) — the points and instances rules read and write
