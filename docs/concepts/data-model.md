---
title: Data Model
description: Products, instances, and T/S/C/A points - and why an instance is a pure thing-model with no status field
updated: 2026-07-10
---

# Data Model

Aether models the physical plant in three layers: products (device-type
templates), instances (individual devices), and points (single measurable or
actionable quantities). The central design invariant is that an instance is a
**pure thing-model**: it holds logical structure plus current values, and
nothing else. Connectivity, alarms, and routing live in separate datasets that
reference the instance without ever being copied onto it.

## Three layers

**Product** — a template describing what points a device *type* has. Defined
in `Product` (`libs/aether-config/src/automation.rs`): a unique `product_name`, an
optional `parent_name` for the type hierarchy (Station → ESS → Battery/PCS,
and so on), plus three point lists:

- `measurements` — measurement point definitions (id, name, unit, description)
- `actions` — action point definitions (id, name, unit, description)
- `properties` — property templates (static configuration values, e.g. rated power)

The product library ships as JSON files under `libs/aether-model/src/products/`,
where the same three lists appear as the `M`, `A`, and `P` arrays and the
hierarchy as `name` / `pName`. Product files are validated in code by
`validate_product_dir` (`libs/aether-model/src/product_lib.rs`), which parses
each JSON file and reports per-file errors.

**Instance** — one physical device. Defined in `Instance` / `InstanceCore`
(`libs/aether-config/src/automation.rs`):

- `instance_id` (numeric, unique) and `instance_name`
- `product_name` — which template this device instantiates
- `parent_id` — position in the site topology (None for root instances)
- `properties` — concrete values filled in for the product's property templates
- optional `created_at` — a creation timestamp (a bookkeeping fact about the
  record itself, not aggregated runtime state)

That is the complete field list. There is no `status`, `health`, `online`,
`degraded`, or `alarm_state` field — see [the purity rule](#the-purity-rule).

**Point** — a single measurable or actionable quantity, identified by a
numeric id that is unique within its product and its point list. A point on
the acquisition side is something the device reports (state of charge, breaker
position); a point on the command side is something the system can set (start
command, power setpoint).

## Point types

Channel-side points use the four-type classification defined by `PointType`
in `libs/aether-core/src/types.rs` (re-exported through `aether-model`). The
enum documentation calls this the standard IEC "Four Remote" classification;
each variant carries a serde alias for the Chinese-standard code (YC/YX/YK/YT).

| Type | Name | Signal kind | Direction | Write owner |
|------|------|-------------|-----------|-------------|
| `T` | Telemetry | Analog measurement (YC) | device → system | io |
| `S` | Signal | Digital status (YX) | device → system | io |
| `C` | Control | Digital command (YK) | system → device | automation |
| `A` | Adjustment | Analog setpoint (YT) | system → device | automation |

`PointType` provides the category predicates the codebase uses everywhere:
`is_measurement()` is true for T and S, `is_action()` for C and A,
`is_analog()` for T and A, `is_digital()` for S and C.

Write ownership is enforced by construction in the shared-memory layer
(`libs/aether-rtdb-shm/src/unified_shm.rs`): io creates the SHM file
through `UnifiedWriter`, which has a general `set()` for T/S slots. automation
opens the same file through `ActionWriter`, a newtype wrapper that exposes
only `set_action()` — there is no `set()` on it, so writing a T or S slot from
automation is a compile error, and `set_action()` additionally rejects any
non-action `point_type` at runtime.

On the instance side, the four channel types collapse into two roles, defined
by `PointRole` in `libs/aether-model/src/types.rs`:

- `M` (Measurement) — data flows device → model
- `A` (Action) — data flows model → device

Live values are addressed by typed SHM coordinates. Channel-to-model and
model-to-channel mappings are durable configuration in SQLite and are loaded
into in-process routing indexes. A custom mirror may choose its own external
key shapes, but those keys are not part of the core data model.

## The purity rule

An instance holds **logical structure plus current values, and nothing else**.
The `Instance` struct has identity, hierarchy, properties, and point mappings.
No aggregated state of any kind — no `status`, no `health`, no `online`, no
`alarm_state` — exists on it.

This is a deliberate design decision, not an omission. Each candidate status
field is actually a property of a *different* subsystem with its own writer
and its own lifecycle:

- "Online" is a property of a **communication channel**, not a device. An
  instance's points bind to channel points through the routing tables, and
  nothing forces them all onto one channel. An instance-level online flag
  would be a lossy aggregate of per-channel facts that io publishes in the
  channel-health SHM segment.
- "Has an active alarm" is a property of the **alarm event stream**, which
  alarm owns in its own tables. Alarms reference points by coordinates;
  the reference points one way only.
- "Last control write failed" is a property of a **single call**, and it
  surfaces in that call's return value (see
  [Consequences for UIs and agents](#consequences-for-uis-and-agents)).

Copying any of these onto the instance would create a second copy of a fact
whose authoritative writer lives elsewhere. Second copies go stale, conflict
with the original under partial failure, and force every writer to know about
every consumer. Keeping the instance pure means each fact has exactly one
writer and consumers join the datasets they need at read time.

## Four orthogonal datasets

The runtime state of the system is split across four datasets. Each has one
writer; none is derived from or copied into another.

| Dataset | Where | Meaning | Writer |
|---------|-------|---------|--------|
| Instance current values | Typed point slots in live-state SHM, resolved through the routing index | Thing-model values (a point may be NaN if never acquired) | io (`M` source values); automation (`A`, on action execution) |
| Channel connectivity | Channel-health SHM segment | Per-channel online/offline state and heartbeat | io |
| Alarm events | alarm SQLite tables `alert` and `alert_event` (`services/alarm/src/db.rs`) | Event stream: trigger/recovery rows addressing points by `service_type` + `channel_id` + `data_type` + `point_id` (for instance points, `service_type` is `"inst"` and the id column holds the instance id) | alarm |
| Routing configuration | SQLite mappings loaded into per-process indexes | Static point-to-point wiring between channel points and instance points | Configuration sync/API transaction |

The routing indexes are the join keys between the others: C2M maps a
channel point (`{channel_id}:{T|S}:{point_id}`) to an instance measurement
(`{instance_id}:M:{point_id}`), and M2C maps an instance action
(`{instance_id}:A:{point_id}`) back to a channel command point. Alarm rules
address values by the same coordinates. Instances
never reference alarms, connectivity, or routing back — the arrows point one
way.

## NaN as a sentinel

In the shared-memory plane, "no data has ever been written here" is encoded
in the value itself. Every SHM `PointSlot`
(`libs/aether-rtdb-shm/src/core/slot.rs`) is created with both its value and
raw-value fields set to a quiet IEEE-754 NaN (the hardcoded bit pattern
`SLOT_UNWRITTEN_BITS = 0x7FF8_0000_0000_0000`). The first real write replaces
the sentinel with a finite double. This removes the historical ambiguity where
a zero-initialized slot was indistinguishable from a genuine reading of 0.0.

There is **no side-channel quality flag** in the cross-service data plane.
The 32-byte `PointSlot` layout is value, timestamp, raw value, seqlock
sequence, and dirty flag — nothing else. (io's protocol layer does track
per-point quality codes internally in `services/io/src/protocols/core/`,
but they never cross the SHM boundary.) The value is the data; consumers must
check for NaN explicitly:

- SHM readers probe `PointSlot::is_unwritten()` or `f64::is_nan()` on the
  returned value.
- The rule engine (`libs/aether-rules/src/executor.rs`) tracks which
  variables were unavailable in `RuleReadOutcome` and skips evaluation rather
  than substituting 0.0 — otherwise a condition like `current < threshold`
  would silently fire on missing data.

The rule for every consumer is the same: absence of a valid finite value is a
first-class outcome you must handle, not an error state recorded somewhere
else.

## Consequences for UIs and agents

**Graying out a control button is the client's join.** To decide whether a
control on instance 7 can currently reach its device, resolve the instance's
action point through the M2C routing index to a channel id, then read that
channel from the channel-health SHM segment. The backend deliberately does not
pre-join this onto the instance. automation performs the same join at write
time before dispatching.

**Control-write failures surface in the caller's return value, never as
instance state.** When the target channel is offline, automation rejects the write
with `AutomationError::ChannelUnreachable` (`services/automation/src/error.rs`); a
degraded dispatch path (SHM written but the notification to io failed)
fails with `AutomationError::DispatchDegraded`. Both reach HTTP through
`AetherErrorTrait::http_status` (`libs/errors/src/lib.rs`), where their
categories — `ResourceBusy` and `Network` respectively — both map to
**HTTP 503**; callers distinguish them by error code
(`AUTOMATION_CHANNEL_UNREACHABLE` vs `AUTOMATION_DISPATCH_DEGRADED`), and both are
retryable. In the rule engine, an action that cannot be resolved produces an
`ActionResult` with `success: false` and a NaN value via the executor's
`action_skipped` path, attributed to the variable that caused the skip.

In every case the failure is information for *that caller, that call* — it is
reported, logged, and possibly retried, but nothing is written back onto the
instance. The next reader of `inst:{id}:A` sees the last accepted command,
not a failure flag.

## Related pages

- [System Architecture](architecture.md) — services and how they communicate
- [Shared Memory](shared-memory.md) — the SHM plane in depth: slots, seqlock, writer ownership
- [Data Flow](data-flow.md) — the uplink and downlink paths end to end
- [Product Models](../domain/product-models.md) — the product library and its domain meaning
- [Safe Operations](../domain/safe-operations.md) — why control writes are gated and how failures propagate
