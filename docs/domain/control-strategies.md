---
title: Control Strategies as Rules
description: Expressing SOC management, peak shaving, and demand control as executable rule flows
updated: 2026-07-10
---

# Control Strategies as Rules

## Rules are the strategy substrate

Aether has no hardcoded "peak shaving mode" or "backup mode." Every control
strategy is a rule flow: nodes that read points, evaluate conditions, and write
action points (A). Strategies live as data (rules in SQLite, edited visually or via
the API), not as code paths, so a site's control behavior can be changed without
rebuilding or restarting anything.

A rule is stored in two columns that are always produced together: `flow_json`
(the full visual-editor graph, including layout) and `nodes_json` (the compact
execution topology the engine actually runs). See
[Rule Engine](../concepts/rule-engine.md) for how the parser, scheduler, and
executor fit together.

Rules fire in one of two ways:

- **Interval** — the rule is evaluated on the scheduler tick at a fixed period
  (`{"type": "interval", "interval_ms": 1000}`). Good for strategies that must
  re-assert a setpoint continuously, like the SOC management example below.
- **OnChange** — the rule subscribes to specific points and is evaluated when
  one of them changes, delivered through the PointWatch event plane. Two
  deadbands filter noise, combined with AND semantics: `time_deadband_ms`
  limits trigger frequency, and `value_deadband` (absolute or percent) ignores
  micro-fluctuations. Good for strategies that react to discrete events, like a
  breaker state flip.

Rules can read any measurement point (M). There is no standalone
calculation-engine layer that publishes derived quantities as ordinary
points — to aggregate a quantity such as total site power, either read a
meter point that measures the total directly, or read the individual
points into rule variables and combine them in a `calculation` node inside
the rule.

## Worked example: battery SOC management

The optional energy-pack example
`packs/energy/examples/config/automation/rules/battery_soc_management.json`
implements automatic state-of-charge management with diesel generator (DG)
backup and PV curtailment. It ships disabled with `"priority": 100`; a site
commissioning plan must map and explicitly enable it before use.

### Input nodes

Two `input` nodes run from the start node in parallel:

- `read_soc` reads the current SOC measurement
  (`"source_type": "measurement_point"`, instance `battery_system`, point
  `soc_current`).
- `load_config` loads three threshold parameters from configuration:
  `soc_recover_threshold`, `soc_upper_limit`, and `soc_lower_limit`.

The template's variable glossary maps these to the short names used in
conditions: `SOC` (current battery SOC), `SOC_r` (recover threshold), `SOC_u`
(upper limit), `SOC_l` (lower limit), plus power variables `P_e` (ESS power,
positive means discharge, negative means charge), `P_d` (diesel generator
power), `P_p` (PV power), `P_l` (current load), and `P_pMax` (PV rated power).

### The decision node

Both inputs feed the `decision` node `soc_condition`, which declares three
condition branches. Quoting the condition strings from the JSON:

1. `"SOC <= SOC_l && DG.status == 'stopped'"` → output `low_soc`. The battery
   has drained to the lower limit and the diesel generator is not running, so
   backup generation is needed.
2. `"SOC >= SOC_r && DG.status == 'running'"` → output `normal_mode`. The
   battery has recovered past the recover threshold while the generator is
   running, so the system can return to normal operation.
3. `"SOC >= SOC_u"` → output `high_soc`. The battery is at its upper limit, so
   PV output must be curtailed to stop overcharging.

If none of the conditions match, no branch fires and the cycle ends without
writing anything — the flow encodes only the transitions, not a resting state.

### Action nodes

Each branch leads to an `action` node:

- `rule1_low_soc_backup` (branch `low_soc`) issues two actions: a `control`
  action targeting `diesel_generator` with command `start`, then a `set_power`
  action on `diesel_generator` with value `P_pMax` (expression
  `P_d = P_pMax` — run the generator at the PV system's rated power).
- `rule2_normal_mode` (branch `normal_mode`) issues a `control` action on
  `diesel_generator` with command `stop`, then a `set_power` action on
  `pv_system` with value `P_pMax` (expression `P_p = P_pMax` — restore PV to
  rated power).
- `rule3_high_soc_curtail` (branch `high_soc`) issues one `set_power` action on
  `pv_system` with value `P_l` (expression `P_p = P_l` — cap PV output at the
  current load, so no surplus charges the already-full battery).

The low-SOC branch has one extra wrinkle: after commanding the generator to
start, a second `decision` node `check_dg_running` evaluates
`"DG.status == 'running'"`. Its `yes` edge goes to the end node; its `no` edge
loops back to `rule1_low_soc_backup`, retrying the start command until the
generator confirms it is running. The template's metadata backs this up with
`"retry_on_failure": true` and `"max_retries": 3`.

### Flow diagram

```text
start
 ├─▶ read_soc     battery_system.soc_current
 └─▶ load_config  soc_recover_threshold / soc_upper_limit / soc_lower_limit
          │
          ▼
    soc_condition
     ├─ low_soc ────▶ rule1_low_soc_backup ──▶ check_dg_running ── yes ─▶ end
     │                  (DG start, P_d = P_pMax)   │
     │                        ▲──────── no ────────┘   (retry until running)
     ├─ normal_mode ▶ rule2_normal_mode ─────────────────────────────────▶ end
     │                  (DG stop, P_p = P_pMax)
     └─ high_soc ───▶ rule3_high_soc_curtail ────────────────────────────▶ end
                        (P_p = P_l)
```

The template is interval-evaluated. Its metadata records
`"execution_interval": 5`, but the engine does not read that value; since the
template declares no `trigger_config`, the effective trigger is the default
`Interval` with `interval_ms: 1000`. Interval evaluation suits a hysteresis
strategy: the rule re-checks SOC every cycle, and the gap between `SOC_l` and
`SOC_r` prevents the generator from flapping on and off around a single
threshold.

## Pattern: peak shaving

There is no shipped peak-shaving rule; this section describes how you would
assemble one from the same node types the SOC template uses.

Peak shaving discharges the battery when grid import approaches a demand limit.
The flow shape mirrors the SOC template exactly:

- An `input` node with `"source_type": "measurement_point"` reading the grid
  meter's active power (or, if site power is spread across several PCS units,
  input nodes for each PCS power point combined in a `calculation` node).
- A second `input` node loading a demand threshold and a recovery threshold
  from `config_params` — two thresholds, for the same hysteresis reason the SOC
  template uses `SOC_l` and `SOC_r`.
- A `decision` node with two condition branches: grid power above the demand
  threshold routes to a discharge branch; grid power back below the recovery
  threshold routes to a restore branch.
- `action` nodes using `set_power`: the discharge branch writes a PCS power
  setpoint (discharge to offset the excess import), and the restore branch
  writes the setpoint back to its normal value.

An Interval trigger re-asserts the setpoint each cycle as load moves; an
OnChange trigger on the meter point with an absolute value deadband (say, a few
kW) reacts faster while ignoring measurement noise.

## Pattern: demand control with priorities

Like peak shaving, this is assembly guidance rather than a shipped feature.
When several strategies can write the same actuator — for example, peak shaving
and SOC protection both setting PCS power — you need a deterministic outcome.

Rules carry a `priority` field for this: the SOC template declares
`"priority": 100`, and the engine loads and evaluates rules in descending
priority order (higher priority runs earlier; equal priorities tie-break by
rule ID). Priority alone orders execution — it does not lock an actuator — so a
lower-priority rule that fires later in the same cycle would still overwrite an
earlier write. The robust pattern combines two things:

- Give the protective rule (SOC limits, equipment safety) the higher priority
  number so it evaluates first each cycle.
- Make the conditions mutually exclusive, the way the SOC template's branches
  are: include the protective rule's guard variable (for instance, SOC within
  safe bounds) as a condition in the economic rule's `decision` node, so the
  peak-shaving branch simply does not fire when the protective rule is active.

That keeps arbitration explicit in the rule conditions, where an operator can
read it, instead of implicit in write ordering.

## Execution guarantees

What a strategy author can rely on:

- **Every execution is recorded.** Results are persisted in local SQLite
  `rule_history` and exposed to subscribed clients over WebSocket, so you can
  watch a strategy's decisions live and audit the recent past without an
  external database.
- **Offline targets skip, they do not queue.** If an action targets an instance
  whose channel is offline, automation rejects the write before it reaches shared
  memory; the executor records the action as skipped with a reason. Commands
  are never buffered for later delivery — when the device comes back, the next
  rule cycle re-evaluates from current values instead of replaying stale
  setpoints.
- **Missing inputs skip the cycle.** If a variable the rule reads is
  unavailable (for example, the measurement is NaN because the device has not
  reported), the cycle is skipped and logged rather than evaluated against
  garbage.
- **Changes hot-reload.** `POST /api/scheduler/reload` atomically rebuilds the
  scheduler's rule set and OnChange subscriptions; edits take effect
  immediately, with no service restart.
- **There is no dry-run.** The `rules_execute` MCP tool and `aether rules
  execute` perform a real execution: the flow is evaluated, and whichever
  actions the conditions select are dispatched through shared memory to io
  and on to the device. (The CLI accepts a `--force` flag, but the server's
  execute endpoint takes no request body — the flag is currently ignored.)
  There is no test or dry-run endpoint. Trial a
  new strategy against a Virtual-protocol channel first (see
  [Connect Devices](../guides/connect-devices.md)) before pointing it at
  real hardware.
