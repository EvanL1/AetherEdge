# OceanClaw + nex-env IoT Safety Lab

**Status**: Proposal / Backlog — not scheduled
**Recorded**: 2026-07-11

## Purpose

Preserve a future experiment in which Aether provides an IoT environment for
OceanClaw and nex-env. This plan records the idea and its architectural
boundaries; it is not an implementation or release commitment.

## Why this experiment is useful

Aether provides a deterministic, multi-process IoT edge runtime with SHM as
the live-state authority, local rules and alarms, command dispatch, and device
simulation. OceanClaw explores AI-agent planning and orchestration. nex-env
explores policy enforcement, approvals, durable intent, receipts, and
reconciliation around external side effects.

Together they can exercise failure modes that ordinary API benchmarks do not
represent well: continuously changing telemetry, stale or invalid state,
offline devices, local safety interlocks, non-idempotent physical operations,
and a crash after a device acts but before its acknowledgement is recorded.

The experiment must not make either external project, or their PostgreSQL,
Python, Go, sandbox, or cloud dependencies, part of the Aether edge kernel.

## Proposed model

```text
OceanClaw agent
      |
      | typed capability invocation
      v
nex-env command gate
      | policy / approval / intent / receipt
      v
Aether lab ingress
      |
      v
EdgeApplication -> command dispatcher -> Aether IO -> simulator or test bench
```

The responsibilities remain separate:

- OceanClaw is the agent planning, collaboration, and evaluation layer.
- nex-env is an experimental command-policy and verifiable-evidence layer.
- Aether is the deterministic IoT environment and the ground-truth oracle for
  live state, alarms, dispatched commands, and observed device effects.

The experiment should live in a separate lab repository or an explicitly
optional lab pack. It must not enter Aether's default Cargo workspace, default
Compose/systemd deployment, CI verification path, or released dependency
graph. The lab must pin clean revisions or image digests of all participants.

Initial integration may use a small structured JSON CLI packaged as an
OceanClaw skill. MCP or another tool transport may replace it later without
changing the Aether application capability contract.

## First scenario

Start with an industry-neutral simulated process cell, such as a tank or
cold-room controller, with level, temperature, pressure, flow, pump, valve,
leak, and emergency-stop signals. Include both idempotent setpoints and at
least one non-idempotent operation.

The first stages use simulation only. HIL or physical equipment requires a
separate review, local hard limits, a physical emergency stop, and human
supervision.

## Safety and dependency boundaries

- OceanClaw and nex-env never participate in acquisition, SHM ownership,
  deterministic rules, alarms, interlocks, or other hard real-time behavior.
- An agent cannot access SHM, device buses, protocol credentials, or Aether
  internal service ports directly.
- All AI-originated commands enter through the same typed Aether application
  API used by other interfaces. Aether performs its own authorization and
  audit even when nex-env has allowed an operation.
- Queries receive read-only live-state access. No lab adapter receives
  `LiveStateWriter`.
- Loss of OceanClaw or nex-env fails the AI command path closed while Aether's
  acquisition and local safety behavior continue normally.
- A non-idempotent command with an uncertain result becomes `UnknownOutcome`
  and requires reconciliation. It is never retried automatically.

## Minimum evaluation invariants

The experiment must independently compare the OceanClaw tool trace and nex-env
evidence with Aether and simulator ground truth. At minimum it must demonstrate:

- zero unauthorized physical side effects;
- zero automatic replays of non-idempotent commands;
- at most one dispatch for a retried operation identity;
- a durable intent and a receipt, explicit gap, or `UnknownOutcome` for every
  observed side effect;
- zero interruption of acquisition and local safety behavior when either AI
  component is unavailable;
- rejection of stale, invalid, out-of-range, unapproved, or policy-revoked
  control attempts; and
- consistency between the agent's final claim, the evidence record, and the
  device-state oracle.

## Preconditions

Before scheduling the lab, resolve:

1. A stable operation identity that survives hold, approval, and retry.
2. Approval binding to actor, gateway, capability, typed arguments, policy
   revision, expiry, and nonce rather than a boolean confirmation flag.
3. Explicit idempotency classification and `UnknownOutcome` reconciliation.
4. Network isolation that makes the command gate the agent's only route to the
   lab ingress.
5. Aether interface convergence on `EdgeApplication` so no write transport can
   bypass the capability policy and audit path.
6. Licensing and redistribution terms for every repository and combined image.

## Non-goals

- Merging OceanClaw or nex-env into Aether.
- Adding their infrastructure dependencies to the standalone edge runtime.
- Treating an HTTP proxy, prompt, MCP annotation, or agent policy as a physical
  safety boundary.
- Claiming that simulation results certify unattended production control.

## Promotion rule

Create an ADR only if this experiment is scheduled and requires a lasting
change to Aether's supported architecture, dependency direction, command path,
or data authority.
