# Target repository layout

The migration target separates stable libraries, optional executable
extensions, user/AI interfaces, and declarative industry knowledge.

```text
crates/
  aether-domain        pure types and invariants
  aether-ports         external capability traits
  aether-application   commands, queries, policies, capability registry
  aether-pack          versioned, fail-closed declarative pack manifest loader
  aether-data-processing strict transport-neutral processor JSON codec
  aether-dataplane     physical SHM layout, slots, mmap I/O, snapshots
  aether-runtime       reusable service lifecycle and supervision primitives
  aether-rules         industry-neutral rule engine
  aether-sdk           stable public facade and builder
  aether-testkit       extension conformance suites

extensions/
  protocols            official device drivers behind feature flags
  store-local          memory/file/embedded persistence
  redis-bridge         optional StateMirror/EventBridge implementation
  postgres-history     optional HistorySink implementation
  mqtt-uplink          optional cloud uplink
  http-api             optional REST/WebSocket transport
  http-data-processor  optional local/remote DataProcessor adapter
  linux-platform       Linux GPIO/CAN/device support
  python-transform     optional Python transform host

interfaces/
  cli                  human command-line transport
  mcp                  AI transport over the application capability API
  local-api            local process boundary when needed

services/              production process-isolation boundaries
  io                   aether-io: protocol acquisition and sole T/S writer
  automation           aether-automation: models, rules, and C/A dispatch
  alarm                aether-alarm: independent alarm evaluation
  history              aether-history: independent historian
  api                  aether-api: independent local API/WebSocket process
  uplink               aether-uplink: independent cloud uplink

tools/
  aether               CLI and MCP launcher
  simulator            simulator

examples/
  minimal-gateway      dependency-free in-process composition proof only
  energy-gateway       fail-safe AetherEMS overlay composition proof

packs/
  energy               energy models, mappings, rules, and knowledge
  building             building-automation assets
  factory              manufacturing assets

integrations/
  load-forecasting     opt-in request-driven legacy forecasting adapter

ai/                    agent navigation, invariants, safety, runbooks, evals
contracts/             machine-readable config/command/event/tool schemas
tests/                 conformance, integration, scenario, and chaos tests
```

## Storage capabilities

Storage is split by intent rather than database command vocabulary:

| Port | Responsibility | Typical implementation |
|---|---|---|
| `LiveState` | Current point values | SHM or memory |
| `LiveStateWriter` | Acquisition-owned point updates | SHM writer or memory |
| `ConfigRepository` | Devices, mappings, and rules | file or SQLite |
| `HistorySink` | Append historical samples | local SQLite or PostgreSQL |
| `HistoryQuery` | Read bounded historical observation windows | local history service or embedded store |
| `DurableOutbox` | Offline store-and-forward | local journal or SQLite |
| `UplinkPublisher` | Transport delivery boundary | MQTT, HTTP, or custom cloud adapter |
| `StateMirror` | Optional external live-state view | Redis |
| `AuditSink` | Durable operation audit | local file/SQLite or PostgreSQL |

One adapter may implement several ports, but no port exposes vendor commands.

## Data-processing capabilities

Aether Data Processing is a vertical slice through the existing stable layers,
not a new dependency direction and not a mandatory seventh process:

```text
aether-domain::data_processing       task identity, processing/result values, quality, provenance
aether-ports (data_processing/clock) HistoryQuery, CovariateSource, Clock, and DataProcessor ports
aether-application::data_processing  data binding, frame assembly, policy, processor invocation, validation
aether-data-processing               strict v1 JSON DTOs, codec, and canonical input digest
extensions/sqlite-history-query      default read-only, task-scoped interval aggregation
extensions/http-history-query        optional pre-aligned Last/Reject history transport
extensions/http-data-processor       optional processor transport
services/api                         opt-in v1 runtime composition and authenticated HTTP routes
packs/<industry>/data-processing     declarative tasks, semantic bindings, units, schemas
```

Version 1 composes the application inside opt-in `aether-api` and isolates the
Load-Forecasting implementation in a sidecar. It returns `DerivedData` directly
over authenticated HTTP; there is no result cache, CLI/MCP binding, scheduler,
or standalone Aether orchestration process. `aether-data-processor` is reserved
as a possible future process name if shared orchestration later requires one.
With no configured processor, the runtime does not advertise data-processing
execution capabilities and acquisition, history, alarms, automation, API
access, and uplink continue normally.

Processors receive complete, bounded processing frames from the application.
They cannot discover points by reading SQLite, query the historian directly,
or attach to SHM. Returned values are expiring derived-data artifacts; they
never become authoritative T/S state or bypass the control application. This
application-level facility does not replace protocol decoding, the rule
engine, history storage, alarms, or bulk ETL. See
[Data Processing](../concepts/data-processing.md),
[Data Processing Flow](../concepts/data-processing-flow.md), and
[ADR-0009](../adr/0009-aether-data-processing.md).

## AI-native repository contract

- `AGENTS.md` is the canonical development policy.
- `llms.txt` is the compact documentation index.
- `ai/catalog.yaml` maps capabilities and components to code and tests.
- `ai/safety-policy.yaml` classifies operational risk.
- External contracts are available as schemas under `contracts/`.
- Generated indexes are committed and checked for drift.

AI is a control-plane client, never part of a hard real-time acquisition or
safety loop.

## Runtime composition rule

Process boundaries are intentional architecture, not legacy directory noise.
Production keeps the six service binaries independently restartable and
resource-limited. Shared crates remove duplicated policy and wire contracts;
they do not collapse the services into one failure domain. A future `aetherd`
may be offered as an opt-in development profile, but it must compose the same
ports and may not become the only runnable form.

## Phase-two implementation status

- `aether-dataplane` contains the physical SHM core. Production channel-aware
  readers, acquisition writers, command sinks, generation lifecycle, and
  PointWatch adapters live in `extensions/shm-bridge`. The former
  `aether-rtdb-shm` compatibility aggregate has been removed.
- `extensions/store-local::FileOutbox` is the default durable outbox option;
  it is a versioned append-only journal, not an external database.
- `OutboxForwarder` in the application layer connects `DurableOutbox` to an
  `UplinkPublisher` without importing MQTT or another transport.
- `aether-alarm` and `aether-api` consume SHM plus isolated PointWatch event
  channels, with polling reconciliation.
- `aether-history` and `aether-uplink` enumerate logical series from SQLite and sample SHM;
  their historical/uplink outputs use embedded SQLite and `FileOutbox`.
- All six service default dependency trees exclude Redis, PostgreSQL, and
  `workspace-hack`. External-store coupling is confined to explicit extension
  crates and deployment profiles.
