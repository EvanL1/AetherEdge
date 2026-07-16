---
name: aether-iot
description: Build, integrate, diagnose, or generate applications for the AetherEdge AI-native edge kernel. Use for AetherEdge onboarding, SDK compositions, device and topology clients, read-only operations UIs, MCP integration, Domain Packs, or governed IoT commands where live-state authority and physical-device safety must be preserved.
---

# AetherEdge

The Skill identifier remains `aether-iot` during the repository rename for
downstream compatibility. Treat AetherEdge as a headless edge runtime with machine-readable contracts. Use its documentation
for reasoning, OpenAPI and manifests for exact schemas, and MCP for live capabilities. Treat every
generated application as an untrusted client of the application boundary.

## Establish context

1. In an AetherEdge checkout, read `AGENTS.md` before changing files.
2. Outside a checkout, fetch `https://docs.aetheriot.workers.dev/llms.txt`, then fetch only the
   relevant page as Markdown by appending `.md` or requesting `Accept: text/markdown`.
3. Prefer documentation matching the installed release. Inspect local runtime facts with:

   ```bash
   aether --version
   aether --json runtime-manifest
   aether --json doctor
   ```

4. Never infer an endpoint, schema, capability, protocol, or Pack from model memory. Verify it from
   the matching runtime manifest, running OpenAPI document, MCP `tools/list`, or active Pack.

## Route the task

- For installation or safe-empty setup, read `agent-quickstart.md` and
  `guides/getting-started.md`.
- For AI or MCP connectivity, read `guides/ai-assistants.md` and
  `reference/mcp-tools.md`.
- For generated applications or Web UIs, read `guides/build-applications-with-ai.md` and
  `reference/http-api.md`.
- For device onboarding, read `guides/connect-devices.md` and the active Pack knowledge.
- For SDK embedding, read `crates/aether-sdk.md`, the minimal gateway example, and the local
  `AGENTS.md` files governing the target directory.
- For CloudLink or MQTT uplink work, read
  `docs/adr/0017-experimental-cloudlink-mqtt-edge-foundation.md`,
  `docs/adr/0018-pinned-aethercontracts-consumption.md`,
  `docs/reference/cloudlink-mqtt-v1.md`, and `contracts/cloudlink/README.md`.
  Treat the digest-pinned AetherContracts release as shared authority and the
  current product subset as distribution-only. Alpha.3 has 53 exact imports,
  no pending imports, and 25 fixture outcomes in both codecs; never equate MQTT
  PUBACK with cloud persistence or alpha evidence with production authentication.
- Before any write that can reach configuration or hardware, read
  `guides/safe-operations.md`, then the active Pack's additional safety guidance, and inspect the
  capability's current OpenAPI or MCP metadata.

## Build applications contract-first

1. Start read-only. List the queries and live subscriptions required by the requested workflow.
2. Use only the authenticated `aether-api` boundary for remote clients. Keep the IO, automation,
   history, uplink, and alarm process ports on loopback. If a use case is not published through the
   remote application boundary, report the missing capability; do not expose or proxy an internal
   port as a workaround.
3. Generate types and client calls from the running release's OpenAPI contract. Do not maintain a
   second handwritten endpoint catalogue.
4. Preserve point value, health or quality, source timestamp, freshness, topology epoch, and
   configuration revision when the contract provides them. Display unknown, stale, degraded, and
   reconciliation-pending states explicitly.
5. Obtain domain semantics from the active Pack or user requirements. Do not hard-code energy,
   building, factory, or vendor concepts into an industry-neutral application.
6. Keep UI state non-authoritative. Never write SHM, SQLite, Pack files, or runtime configuration
   directly from a browser, AI tool, CLI wrapper, or generated backend.
7. Preserve CloudLink stream epoch, position, batch identity, business digest,
   source timestamp, point quality, and topology generation. Do not invent a
   Thing Model revision or turn legacy write/call topics into CloudLink control.

## Handle commands safely

- Treat capability metadata as executable policy: query versus command, risk, permission,
  idempotency, confirmation, and audit requirements are not presentation hints.
- Do not register MCP write tools by default. Use `aether mcp --allow-write` only for the bounded
  task that requires them.
- Send identity, confirmation, expected revision, and request correlation exactly as the current
  operation contract declares.
- Preserve returned `request_id`, `command_id`, and resulting revision values.
- Do not automatically retry an accepted, non-idempotent, degraded, timed-out, or audit-incomplete
  command. Inspect its receipt and runtime reconciliation state first.
- Do not equate command acceptance with physical execution. Require feedback telemetry for a
  closed-loop result.
- Keep AI outside protocol polling, acquisition, safety interlocks, and hard real-time decisions.

## Verify generated work

Run the narrowest checks for the generated project's stack, then require at least:

- type checking, linting, tests, and a production build;
- contract fixtures for success, stale data, authorization failure, degraded reconciliation, and
  uncertain command outcomes;
- proof that remote clients never call internal service ports or direct storage;
- a read-only default with write controls absent unless the application explicitly needs them;
- no external service requirement introduced into the default AetherEdge runtime.

For changes inside AetherEdge, also run the repository verification commands from `AGENTS.md`.
