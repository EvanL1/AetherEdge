# Target repository layout

The repository is the smallest industry-neutral AetherEdge kernel. Stable
contracts, shared kernel implementation, six process owners, and tooling have
separate roots. Optional third-party implementations live downstream rather
than in an in-tree extension layer.

```text
crates/
  aether-domain          pure types and invariants
  aether-ports           industry-neutral capability traits
  aether-application     commands, queries, policies, capability registry
  aether-pack            versioned declarative Pack loader
  aether-data-processing strict transport-neutral processor codec
  aether-dataplane       physical SHM layout, slots, mmap I/O, snapshots
  aether-sdk             supported public facade
  aether-testkit         reusable port conformance suites

libs/
  aether-auth-jwt        shared application-interface authentication
  aether-shm-bridge      native SHM runtime implementation
  aether-store-local     default zero-external-service storage
  common                 shared service bootstrap and configuration DTOs
  aether-routing         internal logical routing projection
  aether-runtime-catalog exact build/runtime capability manifest
  aether-rules           deterministic industry-neutral rule engine
  ...                    other shared kernel implementation

services/
  io                     physical acquisition and sole T/S writer
  automation             deterministic rules and governed C/A dispatch
  alarm                  independent alarm evaluation
  history                historian and history database owner
  api                    authenticated remote application boundary
    adapters/            API-private HTTP/data-processing implementations
  uplink                 sole cloud and CloudLink runtime owner
    adapters/            Uplink-private transport implementations

tools/
  aether                 CLI and MCP launcher
  simulator              protocol simulation and its only allowed scripts

examples/                minimal composition proofs
packs/                   declarative, manifest-validated domain assets
contracts/               pinned and repository-owned machine contracts
ai/                      agent navigation, policy, runbooks, and evals
docs/                    current concepts, guides, references, and ADRs
firmware/                separately targeted embedded workspace
```

There is deliberately no `extensions/` root. A downstream integration imports
the supported SDK and port contracts, runs the relevant testkit conformance
suite, and owns its external clients, credentials, and deployment lifecycle.
AetherEdge does not load downstream scripts, dynamic libraries, or child
processes.

## Storage capabilities

Storage remains split by intent rather than database vocabulary:

| Port | Responsibility | Kernel implementation |
|---|---|---|
| `LiveState` | Current point values | read-only SHM adapter |
| acquisition writer | Acquisition-owned T/S updates | IO-owned SHM writer |
| configuration repositories | Devices, mappings, rules | local SQLite |
| `HistorySink` | Append historical samples | History-owned embedded storage |
| `HistoryQuery` | Bounded historical windows | History service boundary |
| `DurableOutbox` | Offline store-and-forward | local append-only journal |
| `UplinkPublisher` | Transport delivery | Uplink-owned transport |
| `AuditSink` | Durable operation audit | local file/SQLite |

SHM remains the only live-state authority. A downstream mirror may implement a
published read-side port, but no kernel process reads it as current state.

## Process and adapter ownership

Concrete mechanisms belong to the process that owns their lifecycle:

- IO owns physical protocol connections, channel lifecycle, acquisition, and
  publication into SHM.
- Automation owns deterministic rule scheduling and governed command dispatch.
- History owns the historian database. API must query History through an
  internal application boundary rather than opening the database directly.
- API owns authenticated remote transport and any retained API-private HTTP
  clients.
- Uplink owns CloudLink and legacy cloud transport sessions, spool draining,
  acknowledgement, and replay.

Shared `libs/` packages provide kernel implementation used by more than one
process. They are not public plug-in points. Non-composition libraries depend
on domain and port contracts rather than selecting concrete runtime adapters.

## Data processing

Aether Data Processing remains opt-in and does not add a seventh process:

```text
aether-domain                    task identity, values, quality, provenance
aether-ports                     HistoryQuery, CovariateSource, Clock, DataProcessor
aether-application               frame assembly, policy, invocation, validation
aether-data-processing           strict v1 DTOs and canonical digest
services/api/adapters/           transitional concrete query/processor clients
services/api                     authenticated composition and HTTP routes
packs/<industry>/data-processing declarative task and semantic binding assets
```

Processors receive bounded frames and cannot read SHM, SQLite, configuration,
or credentials directly. Results are derived artifacts, never authoritative
live state. The current direct read-only SQLite History adapter is transitional;
the target is API-to-History `HistoryQuery` composition.

## External integrations and protocol plugins

A downstream integration must:

1. use an industry-neutral published port or obtain a separately accepted
   contract change;
2. remain outside this repository;
3. be statically composed in a downstream Rust distribution;
4. pass the matching testkit suite;
5. avoid direct SHM/storage mutation and route commands through the shared
   application API; and
6. declare capability risk, permission, confirmation, idempotency, and audit
   policy before remote exposure.

SunSpec is an example of a possible downstream IO plugin. The standard kernel
contains no SunSpec models, discovery, feature, or protocol advertisement. A
future generic plugin contract requires a real consumer and a separate ADR.

Home Assistant, Redis mirrors, vendor stores, and domain processors follow the
same downstream ownership rule.

## AI-native repository contract

- `AGENTS.md` is the canonical development policy.
- `llms.txt` and `ai/docs-manifest.json` are generated from the agent catalog.
- `ai/safety-policy.yaml` is the capability-risk authority.
- AI is an application client, never part of acquisition or a safety loop.

## Runtime invariant

Production remains six independently supervised Rust processes. The standard
distribution requires no Redis, PostgreSQL, broker, browser, LLM, Python, or
protocol simulator. Optional deployment profiles cannot change SHM authority,
remote ingress through `aether-api:6005`, or governed command ownership.
