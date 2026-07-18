---
title: Build Applications with AI
description: Generate contract-first IoT applications without turning a Web UI or an AI agent into a second runtime authority
updated: 2026-07-15
---

# Build Applications with AI

AetherEdge is headless by design. It provides a deterministic edge runtime and the machine-readable
contracts an AI agent needs to build a site-specific application. A generated Web UI, mobile app,
CLI, or backend is a replaceable client: it is never part of the live-state, desired-state, or
safety authority.

This is one available foundation for AetherIoT's broader AI-native direction:
people describe outcomes, agents discover the physical space and generate
governed changes, contracts make those changes inspectable, and AetherEdge
executes commissioned behavior without the model. The complete end-user
conversation, simulation, and continuous-adaptation lifecycle is not yet a
shipped product. See [AI-native platform](../overview/ai-native-platform.md).

This guide defines the development method shared by reference applications and downstream products.

## Optional Agent Skill

Optionally add the repository's `aether-iot` Skill to a compatible coding assistant. Bun is used
only to run the third-party `skills` CLI; this command does not install the AetherEdge runtime or
Rust SDK:

```bash
bunx skills add EvanL1/AetherEdge --skill aether-iot
```

The Skill is deliberately small. It teaches the workflow and routes the agent to the current online
documentation instead of embedding a stale copy. The documentation service publishes:

- `/llms.txt` — the curated document index;
- a Markdown twin for every page by appending `.md` or sending
  `Accept: text/markdown`.

Agents should read the compact index first and fetch only the pages needed for
the current task rather than loading the entire documentation corpus.

Connect `aether mcp` when the agent also needs live runtime information. The default MCP surface is
read-only; do not enable writes merely to generate an application.

## Use the contract stack

An application should discover AetherEdge in this order:

| Source | What it establishes |
|---|---|
| Runtime manifest | Exact Kernel version, target, features, protocols, and capabilities |
| Active Pack | Domain vocabulary, supported assets, and compatibility requirements |
| OpenAPI | Exact HTTP paths, schemas, status codes, authentication, and command policy |
| MCP | Live tools and structured runtime results available to an AI client |
| Online Markdown | Architecture, workflows, safety guidance, and domain explanation |

Do not substitute a README example or model memory for a running release's OpenAPI document. Do not
assume that an installed Pack, optional adapter, or write capability exists until its versioned
contract says so.

## Start from a read-only user story

Write the first prompt as a bounded outcome, for example:

```text
Build a read-only operations page for this AetherEdge site. Show service health,
the current topology revision, point values with quality and freshness, active
alarms, and the last hour of available history. Do not add device controls.
```

Before generating code, the agent should produce a short capability inventory:

1. runtime and Pack version;
2. queries and subscriptions required by the page;
3. public capabilities that satisfy them;
4. capabilities that are missing or still internal;
5. the exact OpenAPI documents used for client generation.

A missing public query is an application-boundary gap. It is not permission to expose an internal
service port, scrape SQLite, attach to SHM, or invent an endpoint.

The same rule applies to conversational configuration. If the runtime does not
declare a capability, the agent must report the gap. It cannot fabricate a
device feature, silently approximate a policy, or convert natural-language
confidence into physical authority.

## Keep one remote boundary

Only `aether-api` is intended for remote application traffic. It publishes fixed authenticated
namespaces under `/api/v1/io`, `/api/v1/automation`, `/api/v1/history`, `/api/v1/uplink`, and
`/api/v1/alarm`; their owning process APIs remain on loopback.

A generated remote client must therefore:

- call those gateway-prefixed capabilities only through authenticated `aether-api` on port 6005;
- use the running gateway's OpenAPI contract to generate types and requests;
- use its authenticated WebSocket contract for live updates when available;
- report an unavailable use case instead of proxying an internal port;
- keep credentials out of source control, URLs, logs, and browser persistence not designed for
  secrets.

Local development tools may inspect loopback OpenAPI documents on the edge host, but this does not
turn those ports into supported remote interfaces.

## Preserve IoT state semantics

A useful IoT screen needs more than a numeric value. Preserve and display contract fields for:

- point identity and engineering unit;
- value plus health or quality;
- source timestamp and observed freshness;
- physical topology epoch;
- desired configuration revision;
- active, degraded, or reconciliation-pending runtime state.

Never silently coerce missing, stale, unhealthy, or generation-mismatched data to zero. Never merge
desired state and active runtime state into one optimistic status. When a topology change is in
progress, keep the last coherent generation visible or show that a coherent view is unavailable.

Presentation names, groups, units, enums, and recommended visualizations belong to the active
Domain Pack or the downstream application. A generic client must not infer energy semantics from
the AetherEdge kernel.

## Add commands as a separate phase

Introduce a command only after the read-only workflow is complete. For every command, render and
enforce the server-declared contract:

- risk and required permission;
- explicit confirmation policy;
- idempotency and expected-revision behavior;
- request correlation and audit result;
- accepted, degraded, and uncertain outcomes.

Keep the returned `request_id`, `command_id`, and resulting revision. Do not automatically retry a
non-idempotent command after a timeout, incomplete audit, accepted-but-degraded response, or unknown
physical outcome. Device-command acceptance proves that the local command plane accepted the
request; feedback telemetry is required to show physical execution.

The backend enforces these rules. A confirmation dialog is useful presentation, but it is never the
security boundary.

## Verify the generated application

Every maintained reference or downstream application should pass:

1. type checking, linting, unit tests, and a production build;
2. contract fixtures for healthy, stale, unauthorized, degraded, and uncertain outcomes;
3. a check that no request targets direct process ports, SHM, or SQLite;
4. a read-only default in which control components and write credentials are absent;
5. an integration check against a safe-empty or simulated AetherEdge composition.

Reference applications demonstrate the contract and can be replaced. They do not define API
behavior, configuration authority, or domain truth.

## Related pages

- [Agent Quickstart](https://docs.aetheriot.workers.dev/en/agent-quickstart/) — install and connect from zero
- [Connect AI Assistants](ai-assistants.md) — MCP setup and write gating
- [HTTP API](../reference/http-api.md) — public exposure and operation contracts
- [System Architecture](../concepts/architecture.md) — service and data boundaries
- [Getting Started](getting-started.md) — safe-empty runtime setup
