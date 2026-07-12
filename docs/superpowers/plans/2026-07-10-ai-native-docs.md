# AI-Native Docs Corpus Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an 18-page English, Neon-style docs corpus (energy-storage know-how + architecture + guides + reference), index it in a root `llms.txt`, embed a curated 11-page subset into the `aether` binary as MCP resources, and reposition the README as an AI-native EMS.

**Architecture:** Pure repo Markdown under `docs/{domain,concepts,guides,reference}/`. One generated page (`reference/mcp-tools.md`, rendered from the binary's own `tools/list`). MCP side: a new `tools/aether/src/mcp_docs.rs` module holds an `include_str!` catalog; `mcp.rs` implements rmcp's `list_resources`/`read_resource` on top of it. Zero overlap with CLAUDE.md and existing Chinese docs — do not reference, move, or edit them.

**Tech Stack:** Markdown + YAML frontmatter, bash + python3 (generator script), Rust with rmcp 2.2 (`ServerHandler` resources methods, `ListResourcesResult::with_all_items`, `ReadResourceResult::new`, `ErrorData::resource_not_found`).

**Spec:** `docs/superpowers/specs/2026-07-10-ai-native-docs-design.md`

---

## Rules for every documentation task (Tasks 1–14)

Documentation pages are prose, not code, so those tasks have no RED/GREEN steps. Instead, every page task follows the same discipline:

1. **Read the named grounding sources first.** Every factual claim must come from a named source file. If a fact is not in the sources, do not state it — no industry boilerplate, no invented behavior. When a point's meaning is ambiguous from its name alone, state the name literally and say so.
2. **Write the page in English** with this exact frontmatter shape (title/description are given per-task below — copy them exactly; they are reused verbatim in `llms.txt` (Task 16) and asserted by unit tests in Task 17):

```yaml
---
title: <given per task>
description: <given per task>
updated: 2026-07-10
---
```

3. **Verify** with the shared check (substitute the file path):

```bash
f=docs/domain/ess-primer.md
head -5 "$f" | grep -q "^title:" && head -5 "$f" | grep -q "^description:" && echo frontmatter-ok
grep -nE "TBD|TODO|FIXME|XXX|placeholder|coming soon" "$f" && echo "PLACEHOLDERS FOUND - fix them" || echo no-placeholders
wc -l "$f"
```

Expected: `frontmatter-ok`, `no-placeholders`, and a non-trivial line count (each page should be roughly 80–300 lines; `product-models.md` will be longer because it tabulates 13 products).

4. **Commit** with a `docs:` message given per task.

Style: each page answers one question. Use `##` sections, tables for enumerable facts, fenced code blocks for commands/config. American English. No marketing language. Link between pages with relative paths (e.g. `../concepts/data-model.md`).

---

### Task 1: `docs/domain/ess-primer.md`

**Files:**
- Create: `docs/domain/ess-primer.md`

**Frontmatter:**
- title: `Energy Storage Primer`
- description: `How ESS concepts - PCS, BMS, SOC, grid interface - map onto Aether products, instances, and points`

- [ ] **Step 1: Read grounding sources**

- `libs/aether-model/src/products/README.md` — the product hierarchy (Station → ESS → Battery/PCS; Generator → Diesel/PV_DCDC; Env; Load) and the JSON structure (`name`, `pName`, `P`/`M`/`A`)
- `libs/aether-model/src/products/PCS.json`, `Battery.json`, `ESS.json`, `Station.json` — real point lists to cite as examples
- `libs/aether-model/src/sunspec.rs` and `libs/aether-model/src/sunspec/model.rs` — what SunSpec support exists (`load_model`, `list_model_ids`, `expand_model`)
- `README.md` (repo root) — the 14 supported protocols list

- [ ] **Step 2: Write the page** with these sections:

- `## What an energy storage site looks like` — the physical hierarchy: a Station contains ESS units (each pairing a PCS with a Battery), backup Generators (Diesel, PV DC/DC), Loads, and environment monitoring. Reproduce the hierarchy tree from the products README (in English).
- `## The device roles` — one short paragraph per role, grounded in the point lists: **PCS** (bidirectional power conversion between the DC battery bus and the AC grid; cite its three-phase measurement points Power A/B/C, Voltage A/B/C, DC Power from `PCS.json`), **Battery/BMS** (cells and clusters, state of charge; cite SOC and other M points actually present in `Battery.json` — read it and cite what is there), **Diesel/Generator** (backup source), **PV** (DC-coupled solar via PV_DCDC), **Loads** (consumption, including EV charging and HVAC variants), **Env** (site environment sensors).
- `## Key quantities` — define only quantities that appear in the product JSONs: SOC (%), power (kW) and its per-phase split, voltage/current per phase, DC-side quantities, temperature. State the naming convention: point names come from the product JSON `name` fields.
- `## How this maps to Aether` — the core mapping table:

| Industry concept | Aether concept |
|---|---|
| Device type (PCS, battery, meter…) | Product (JSON template with P/M/A point definitions) |
| A physical device on site | Instance (created from a product) |
| Live telemetry (power, SOC…) | M points (measurements), written by comsrv |
| Commands and setpoints | A points (actions), written by modsrv |
| Nameplate data (rated power…) | P properties (static) |
| Field protocol (Modbus, IEC 104…) | Channel (one per device connection) |

- `## Standard information models` — two short paragraphs: the 14 supported field protocols (list them, from README.md), and SunSpec support (models can be loaded and expanded into point sets via the `aether-model` SunSpec module).
- `## Where to go next` — links to `product-models.md`, `control-strategies.md`, `safe-operations.md`, `../concepts/data-model.md`.

- [ ] **Step 3: Verify** (shared check from the rules section, with `f=docs/domain/ess-primer.md`)

- [ ] **Step 4: Commit**

```bash
git add docs/domain/ess-primer.md
git commit -m "docs: add domain/ess-primer, ESS concepts mapped to Aether model"
```

---

### Task 2: `docs/domain/product-models.md`

**Files:**
- Create: `docs/domain/product-models.md`

**Frontmatter:**
- title: `Built-in Product Models`
- description: `The 13 built-in device models, their hierarchy, and the meaning of every measurement and action point`

- [ ] **Step 1: Read grounding sources**

- Every file in `libs/aether-model/src/products/*.json` (13 products: Battery, Diesel, Env, ESS, EVChargingLoad, Generator, HVACLoad, Load, Load_Three_Phase, PCS, PV_DCDC, PVInverter, Station)
- `libs/aether-model/src/products/README.md` — hierarchy and JSON field semantics
- `libs/aether-model/src/product_lib.rs:107-149` — `ProductLibrary::load`: a user-supplied products dir (`config/products/*.json`) overrides builtins by `name`
- `libs/aether-model/src/sunspec/expand.rs` exports — `expand_model`, `ExpandConfig`, `ExpandFilter`

- [ ] **Step 2: Write the page** with these sections:

- `## Hierarchy` — the parent/child tree derived from each JSON's `pName` field (verify against the actual files, don't trust the README blindly; note any product whose `pName` disagrees with the README tree).
- `## Reading a product definition` — the JSON shape (`name`, `pName`, `P`, `M`, `A`; each point has `id`, `name`, `unit`, `type`), and that `id` is the point's identity within its type.
- One `## <ProductName>` section per product, in hierarchy order (Station first, then ESS/Battery/PCS, then Generator family, then Loads, then Env). Each section: one sentence on the device's role, then up to three tables (Properties/Measurements/Actions) listing **every** point: `| id | name | unit | type |`. For products with empty P/M/A (e.g. ESS, Station are grouping nodes), say so in one line instead of an empty table. Do not editorialize about what a point "probably" means — the name and unit are the documentation.
- `## Custom products and overrides` — `aether sync` loads user product JSONs from the config directory; a user product with the same `name` as a builtin replaces it (cite `product_lib.rs` behavior: builtin set loaded first, directory entries override by name, with a log line on override).
- `## SunSpec expansion` — one paragraph: SunSpec models can be expanded into concrete point sets (`expand_model` with `ExpandConfig`/`ExpandFilter`), bridging standard inverter/meter models to Aether products.

- [ ] **Step 3: Verify** — shared check, plus confirm all 13 products got a section:

```bash
grep -c "^## " docs/domain/product-models.md   # expect >= 17 (13 products + 4 framing sections)
```

- [ ] **Step 4: Commit**

```bash
git add docs/domain/product-models.md
git commit -m "docs: add domain/product-models covering all 13 builtin products"
```

---

### Task 3: `docs/domain/control-strategies.md`

**Files:**
- Create: `docs/domain/control-strategies.md`

**Frontmatter:**
- title: `Control Strategies as Rules`
- description: `Expressing SOC management, peak shaving, and demand control as executable rule flows`

- [ ] **Step 1: Read grounding sources**

- `config.template/modsrv/rules/battery_soc_management.json` — read the **whole** file; it is the worked example
- `libs/aether-rules/src/` — skim the parser/scheduler/executor module docs for node types and trigger semantics (Interval vs OnChange, dead-band)
- `config.template/modsrv/calculations.yaml` — what calculated points look like (CalcEngine feeds derived quantities that rules can read)
- `tools/aether/src/mcp.rs` and `docs/reference/mcp-tools.md` — verify the available rule tools; `rules_execute` is a real execution, not a dry run

- [ ] **Step 2: Write the page** with these sections:

- `## Rules are the strategy substrate` — control strategies in Aether are not hardcoded modes; they are rule flows: read points → evaluate conditions → write action points. Two trigger styles: Interval (evaluated on the scheduler tick) and OnChange (event-driven via the point-watch plane, with dead-band filtering).
- `## Worked example: battery SOC management` — walk the shipped template node by node: the input nodes (read SOC measurement, load threshold config params `soc_recover_threshold`/`soc_upper_limit`/`soc_lower_limit`), the decision node (quote each condition string from the JSON and explain it: low-SOC-and-DG-stopped starts the diesel generator, recovered-SOC-and-DG-running stops it, and the remaining branches — document exactly what is in the file), and the action nodes (which instance/point each one writes). Include a compact flow diagram in a fenced code block.
- `## Pattern: peak shaving` — describe the modeling approach using **only node types that exist in the template**: read a meter power measurement, compare against a demand threshold from config params, write a PCS power setpoint action. Frame it as "how you would assemble it", not as a shipped feature.
- `## Pattern: demand control with priorities` — same treatment: rules carry a `priority` field (cite it in the template JSON); higher-priority rules win contended actuators.
- `## Execution guarantees` — what a strategy author must know: execution results are recorded (Redis `rule:{id}:exec`, 24h TTL) and pushed over WebSocket; if a target channel is offline the action is skipped and reported as skipped rather than queued; rule changes hot-reload without a service restart (`POST /api/scheduler/reload`). There is no dry-run: `rules_execute` performs a real execution — test against a Virtual-protocol channel first (link `../guides/connect-devices.md`).

- [ ] **Step 3: Verify** — shared check with `f=docs/domain/control-strategies.md`

- [ ] **Step 4: Commit**

```bash
git add docs/domain/control-strategies.md
git commit -m "docs: add domain/control-strategies with SOC template walkthrough"
```

---

### Task 4: `docs/domain/safe-operations.md`

**Files:**
- Create: `docs/domain/safe-operations.md`

**Frontmatter:**
- title: `Safe Operations for AI Agents`
- description: `Which writes reach real devices, how write gating works, and the operating rules an AI agent must follow`

- [ ] **Step 1: Read grounding sources**

- `tools/aether/src/mcp.rs` — the module doc comment (registration-time gating rationale), the `WRITE_TOOL_NAMES` const in the tests module, and the `#[tool(description = ...)]` strings on the remaining write tools
- `tools/aether/src/mcp.rs` and `docs/reference/mcp-tools.md` — registration-time write gating and the generated public tool surface
- Repo CLAUDE.md is **not** a source — this page must stand alone. Ground the NaN/online/alarm facts in `libs/aether-rtdb-shm/src/` doc comments and `services/comsrv/src/` where the `comsrv:online` hash is written (grep for `comsrv:online`)

- [ ] **Step 2: Write the page** with these sections:

- `## The write surface` — a table of all write tools from `WRITE_TOOL_NAMES` in three severity groups: **device-affecting**, **data-integrity**, and **configuration**. The data-integrity section must state that automation exposes no instance-measurement write capability and that synthetic T/S input is confined to io's explicit development simulation boundary.
- `## How write gating works` — `aether mcp` registers only read-only tools; `aether mcp --allow-write` additionally registers the write router. Ungated write tools are absent from `tools/list`, not merely annotated — an AI client cannot call what it cannot see. Contrast with `readOnlyHint`, which is advisory and depends on client behavior.
- `## Reading state correctly` — the three traps: (1) a measurement of IEEE-754 NaN means "temporarily unavailable", never zero and never "the device is off"; (2) channel connectivity lives in a per-channel online status (`channels_status` tool) and is **not** aggregated onto instances — before writing to a device, check its channel is online; (3) alarms are an event stream, not an instance state — an instance with active alarms looks identical to a healthy one in its measurement values.
- `## Operating rules` — a numbered list an AI agent should follow verbatim: 1. Prefer read-only mode; request `--allow-write` only for a task that needs it. 2. Before any device write, check the channel is online and read the current value. 3. Never invent an automation-side measurement correction path. 4. Treat NaN as unknown, not zero. 5. After a write, read back and verify. 6. Configuration deletes (`channels_delete`, `rules_delete`, `alarms_rule_delete`) are not undoable — enumerate and confirm the target first. 7. A skipped action (offline channel) is a report, not an error to retry blindly.

- [ ] **Step 3: Verify** — shared check, plus confirm the write-tool count:

```bash
# every name in WRITE_TOOL_NAMES must appear in the page
sed -n '/const WRITE_TOOL_NAMES/,/];/p' tools/aether/src/mcp.rs \
  | grep -oE '"[a-z_0-9]+"' | tr -d '"' | while read -r t; do
  grep -q "$t" docs/domain/safe-operations.md || echo "MISSING: $t"
done; echo done
```

Expected: no `MISSING:` lines.

- [ ] **Step 4: Commit**

```bash
git add docs/domain/safe-operations.md
git commit -m "docs: add domain/safe-operations, write-gating and AI operating rules"
```

---

### Task 5: `docs/concepts/architecture.md`

**Files:**
- Create: `docs/concepts/architecture.md`

**Frontmatter:**
- title: `System Architecture`
- description: `Seven services, their ports, and how they communicate over shared memory, UDS, Redis, HTTP, and MQTT`

- [ ] **Step 1: Read grounding sources**

- `README.md` (repo root) — service table and ASCII architecture diagram
- `libs/aether-model/src/service_ports.rs` — authoritative port constants
- `libs/common/src/` — grep for `wait_for_dependency` (startup ordering mechanism)
- `docker-compose.yml` — service composition and dependencies

- [ ] **Step 2: Write the page** with these sections:

- `## Services` — table: service, port, role (comsrv 6001 protocol drivers & channels; modsrv 6002 instances & rules; hissrv 6004 historical data with pluggable backends; apigateway 6005 REST/WebSocket/JWT; netsrv 6006 MQTT cloud link; alarmsrv 6007 alarm rules & events; apps 8080 Vue frontend; Redis 6379 data mirror & routing).
- `## Communication paths` — table of path → mechanism → latency class: comsrv→all data via shared memory with async Redis mirror; comsrv↔modsrv via SHM mmap + UDS notifications (both directions, sub-ms); alarmsrv→apigateway/netsrv via HTTP; netsrv→cloud via MQTT; apigateway→browsers via WebSocket; all services→SQLite in-process.
- `## Startup order` — comsrv must start before modsrv (comsrv creates the shared-memory segment and its routing hash; modsrv validates on open and waits on comsrv health via the dependency-wait helper).
- `## Configuration flow` — `config/*.yaml` → `aether sync` → SQLite → services load at startup; services never read YAML directly.
- `## Where state lives` — one line each: live values in SHM (source of truth for the hot path), Redis as an async mirror + routing tables, SQLite for configuration, the history database (PostgreSQL/TimescaleDB) for long-term series.

- [ ] **Step 3: Verify** — shared check with `f=docs/concepts/architecture.md`

- [ ] **Step 4: Commit**

```bash
git add docs/concepts/architecture.md
git commit -m "docs: add concepts/architecture"
```

---

### Task 6: `docs/concepts/data-model.md`

**Files:**
- Create: `docs/concepts/data-model.md`

**Frontmatter:**
- title: `Data Model`
- description: `Products, instances, and T/S/C/A points - and why an instance is a pure thing-model with no status field`

- [ ] **Step 1: Read grounding sources**

- `libs/aether-model/src/types.rs` — `PointType` and `PointRole` definitions (authoritative T/S/C/A semantics — read the actual enum docs, do not guess)
- `libs/aether-model/src/keyspace.rs` — `KeySpaceConfig`, how channel and instance keys are formed
- `libs/aether-model/src/products/README.md` — product JSON structure
- `services/modsrv/src/` — grep for `inst:` key usage to confirm the `inst:{id}:M` / `inst:{id}:A` shapes

- [ ] **Step 2: Write the page** with these sections:

- `## Three layers` — Product (template: what points a device type has) → Instance (a device: product + identity + channel bindings) → Point (a single measurable or actionable quantity).
- `## Point types` — table of the four types with their exact semantics from `types.rs` (T/S on the acquisition side, C/A on the command side; include which service owns writes to each — comsrv owns T/S, modsrv owns C/A).
- `## The purity rule` — an instance holds logical structure plus current values, and nothing else. No `status`, `health`, `online`, or `alarm_state` fields exist on instances, by design. The reasoning: those are properties of *other* subsystems (link congruence table below), and aggregating them onto instances creates stale, conflicting copies.
- `## Four orthogonal datasets` — the table: instance current values (`inst:{id}:M` / `inst:{id}:A`, may be NaN); channel connectivity (per-channel online hash, owned by comsrv); alarm events (alarmsrv's tables, referencing instances by id); routing configuration (static tables from `aether sync`). Each has one writer; none is derived from another.
- `## NaN as a sentinel` — current values use IEEE-754 NaN for "temporarily unavailable"; the value itself is the data, no side-channel quality flag exists. Consumers must handle NaN explicitly.
- `## Consequences for UIs and agents` — to grey out a control button, join instance → routing → channel → online status yourself; the backend deliberately does not pre-join it. Control-write failures surface in the caller's return value (rule executions report `action_skipped`; HTTP returns 503 with a reason), never as instance state.

- [ ] **Step 3: Verify** — shared check with `f=docs/concepts/data-model.md`

- [ ] **Step 4: Commit**

```bash
git add docs/concepts/data-model.md
git commit -m "docs: add concepts/data-model, purity rule and orthogonal datasets"
```

---

### Task 7: `docs/concepts/shared-memory.md`

**Files:**
- Create: `docs/concepts/shared-memory.md`

**Frontmatter:**
- title: `Shared Memory`
- description: `The SHM data plane: slot layout, writer ownership, seqlock reads, generations, and the PointWatch event plane`

- [ ] **Step 1: Read grounding sources**

- `libs/aether-rtdb-shm/src/` — module and type doc comments: `UnifiedWriter`, `UnifiedReader`, `ActionWriter`, header fields (`routing_hash`, `writer_generation`), seqlock read functions (`try_load_consistent` vs `load_consistent`), the notifier/listener pair, `SubscriptionBitmap`, rebuild/swap machinery
- `CHANGELOG.md` — the v0.4.0 entry for PointWatch latency numbers (cite the production P50 if stated)

- [ ] **Step 2: Write the page** with these sections:

- `## Layout` — one file: fixed header + a fixed-size array of 32-byte point slots. The file path resolves per platform (env override → `/dev/shm` on Linux → `/tmp` on macOS; Docker mounts a shared volume).
- `## Writer ownership is type-enforced` — comsrv owns T/S slots (via the writer type created at startup); modsrv owns C/A slots via a restricted writer type that exposes only action-setting methods — writing a telemetry slot from modsrv is a compile error, not a runtime check.
- `## Consistency: seqlock` — readers use sequence-validated reads; the single-attempt variant is for async runtime threads (never spin on a tokio worker), the spinning variant is for dedicated threads and returns nothing rather than torn data when retries are exhausted.
- `## Generations and rebuilds` — `routing_hash` must match between comsrv and modsrv at open; `writer_generation` increments on every create/reconfigure so readers detect stale mappings; reconfiguration swaps in a new per-generation file atomically and peers notice via file-identity watching.
- `## Command notifications` — modsrv writes a C/A slot then sends a small fixed-size notification over a Unix domain socket; comsrv's listener dedupes by producer+sequence and reconnects with backoff. No polling fallback.
- `## The PointWatch event plane` — after each T/S write, comsrv consults a subscription bitmap (a separate mmap maintained by modsrv from rule subscriptions); only subscribed points emit events over a dedicated UDS. Events carry the value, so dead-band checks need no read-back. Delivery into the scheduler is a bounded queue: overload drops events and counts drops rather than blocking the hot path.

- [ ] **Step 3: Verify** — shared check with `f=docs/concepts/shared-memory.md`

- [ ] **Step 4: Commit**

```bash
git add docs/concepts/shared-memory.md
git commit -m "docs: add concepts/shared-memory"
```

---

### Task 8: `docs/concepts/rule-engine.md`

**Files:**
- Create: `docs/concepts/rule-engine.md`

**Frontmatter:**
- title: `Rule Engine`
- description: `Dual-column rule storage, tick and event scheduling, execution, and hot reload`

- [ ] **Step 1: Read grounding sources**

- `libs/aether-rules/src/` — parser/scheduler/executor structure, `flow_column_values()` and the `FlowColumns` type, `RuleScheduler::reload_rules`
- `services/modsrv/src/rule_routes.rs` — the HTTP write path for rules
- `config.template/modsrv/rules/battery_soc_management.json` — a concrete `flow_json` example (already covered in depth by Task 3; here only cite its shape)

- [ ] **Step 2: Write the page** with these sections:

- `## Two columns, one writer` — rules persist as two parallel representations: the full visual-editor JSON (nodes, positions, labels) and a compact execution topology. The invariant: both columns are always produced together by one function; no code path serializes either column independently. Why: a divergence means the UI shows one logic while the engine runs another.
- `## Scheduling` — a single loop multiplexes a periodic tick and the point-watch event stream. Interval rules evaluate on the tick; OnChange rules evaluate on events, after dead-band filtering. Events carry values, so evaluation does not read back.
- `## Execution` — the executor evaluates the flow, writes results to the RTDB, and dispatches actions to the shared-memory command slots with a UDS notify. Offline targets produce a skipped-action result, not a queued retry.
- `## Results and observability` — every execution writes a result record (Redis, 24h TTL) and pushes it over WebSocket for live monitoring.
- `## Hot reload` — a reload endpoint atomically rebuilds the subscription bitmap and the event-dispatch index; rule edits take effect without restarting anything.

- [ ] **Step 3: Verify** — shared check with `f=docs/concepts/rule-engine.md`

- [ ] **Step 4: Commit**

```bash
git add docs/concepts/rule-engine.md
git commit -m "docs: add concepts/rule-engine"
```

---

### Task 9: `docs/concepts/data-flow.md`

**Files:**
- Create: `docs/concepts/data-flow.md`

**Frontmatter:**
- title: `Data Flow`
- description: `Uplink and downlink paths end to end, with latency budgets and the Redis keyspace`

- [ ] **Step 1: Read grounding sources**

- `services/comsrv/src/` — grep for `ShmRedisSync` (the dirty-slot scan + pipeline mirror, TTL refresh, `ReverseSlotIndex`)
- `libs/aether-model/src/keyspace.rs` — key shapes
- `README.md` + `CHANGELOG.md` — latency figures (only cite numbers that appear in these files)

- [ ] **Step 2: Write the page** with these sections:

- `## Uplink (device → consumers)` — a numbered path: device protocol frame → comsrv adapter decodes → SHM T/S slot write (~ns scale) → (a) subscribed points emit PointWatch events to modsrv immediately, (b) a background sync task scans for changed slots on a ~100ms cadence and mirrors them to Redis in pipelined batches, fanning channel points out to instance keys via routing. Include an ASCII diagram.
- `## Downlink (rule/API → device)` — modsrv resolves the action's route → writes the C/A slot → UDS notify → comsrv listener picks it up → protocol adapter writes to the device. Routing configuration comes from the sync'd tables; live command data never transits Redis.
- `## Latency budget` — table with the classes: SHM write ~10ns/point; event path sub-ms (cite the CHANGELOG P50 figure); Redis mirror ~100ms cadence; HTTP hops ~5ms. Label each as measured or design target exactly as the source file does.
- `## Redis keyspace` — table of the key families a consumer will encounter: channel-side current values (`comsrv:{ch}:{T|S}`), instance-side (`inst:{id}:M`, `inst:{id}:A`), routing (`route:m2c` etc.), channel connectivity hash (`comsrv:online`), rule execution results (`rule:{id}:exec`, 24h TTL). Note the mirror is eventually consistent with SHM by up to the sync cadence, and mirrored keys carry a 24h TTL refreshed periodically.

- [ ] **Step 3: Verify** — shared check with `f=docs/concepts/data-flow.md`

- [ ] **Step 4: Commit**

```bash
git add docs/concepts/data-flow.md
git commit -m "docs: add concepts/data-flow"
```

---

### Task 10: `docs/guides/getting-started.md` + `docs/guides/deployment.md`

**Files:**
- Create: `docs/guides/getting-started.md`
- Create: `docs/guides/deployment.md`

**Frontmatter (getting-started):**
- title: `Getting Started`
- description: `Build the workspace, initialize configuration, start services, and verify health`

**Frontmatter (deployment):**
- title: `Deployment`
- description: `Run with Docker Compose or build a self-contained installer for edge devices`

- [ ] **Step 1: Read grounding sources**

- `README.md` — quick start section
- `rust-toolchain.toml` — pinned toolchain version
- `scripts/build-installer.sh` — read the usage/help text at the top of the script for exact arguments
- `docker-compose.yml` — services and volumes
- `.env.example` — environment variables relevant to running

- [ ] **Step 2: Write `getting-started.md`** with these sections:

- `## Prerequisites` — Rust (state the pinned version from `rust-toolchain.toml`), Redis, and optionally Docker.
- `## Build and initialize` — fenced commands: `cargo build --release -p aether`, `aether init`, `aether sync`.
- `## Start and verify` — `aether services start`, `aether doctor`; what a healthy doctor output covers. Table of the ports you should now see listening (from `../concepts/architecture.md`, link it).
- `## First look around` — three commands to prove data flows: list channels, list instances, read a live value (use real `aether` subcommands — verify each exists via `aether --help` before writing it down).
- `## Next steps` — links to `connect-devices.md`, `writing-rules.md`, `ai-assistants.md`.

- [ ] **Step 3: Write `deployment.md`** with these sections:

- `## Docker Compose` — `docker compose up -d`, `docker compose ps`; which services the compose file starts; where the SHM volume is mounted.
- `## Edge installer` — `./scripts/build-installer.sh` with its actual arguments (version/arch/target, `--services=…`, `--enable-swagger`), the cross-compile target (`aarch64-unknown-linux-musl`), and the ship-and-run pattern (`scp` the `.run` file, execute it on the device).
- `## Runtime paths` — where the SHM file lives per platform (env override, `/dev/shm`, `/tmp`), where SQLite and config live.
- `## Service management on device` — `aether services start/stop/refresh`, `aether doctor`.

- [ ] **Step 4: Verify** — shared check for both files

- [ ] **Step 5: Commit**

```bash
git add docs/guides/getting-started.md docs/guides/deployment.md
git commit -m "docs: add guides/getting-started and guides/deployment"
```

---

### Task 11: `docs/guides/connect-devices.md` + `docs/guides/writing-rules.md`

**Files:**
- Create: `docs/guides/connect-devices.md`
- Create: `docs/guides/writing-rules.md`

**Frontmatter (connect-devices):**
- title: `Connect Devices`
- description: `Configure channels, choose protocols, and map device points to instances`

**Frontmatter (writing-rules):**
- title: `Writing Rules`
- description: `Author rules through the HTTP API or the visual flow editor`

- [ ] **Step 1: Read grounding sources**

- `services/comsrv/Cargo.toml` — the `[features]` section: which protocols are in `default`, which imply others (`j1939`→`can`, `mqtt`/`http`→`json-mapping`)
- `services/comsrv/src/protocols/gateway/factory.rs` — which features are additionally OS-gated (`can`/`gpio` are Linux-only)
- `config.template/comsrv/comsrv.yaml` and `config.template/comsrv/README.md` — a real channel config example
- `config.template/modsrv/instances.yaml` — instance definitions binding to products
- `services/modsrv/src/rule_routes.rs` — rule CRUD endpoints (methods + paths)

- [ ] **Step 2: Write `connect-devices.md`** with these sections:

- `## Channels` — a channel is one device connection: protocol + transport parameters + a point table. Show a trimmed real example from the template config.
- `## Protocol availability` — table of all 14 protocols with three columns: protocol, compiled by default (yes/no from the `default` feature list), platform notes (CAN/GPIO Linux-only; Virtual always available — it has no feature gate and exists for testing/simulation). State the rule of thumb: if a channel fails to create, check the feature gate first.
- `## Mapping points to instances` — points on a channel map to instance points via routing; the flow: define the instance (product binding), map channel points, run `aether sync`, verify with the unmapped-points check.
- `## Verifying a connection` — check channel status, watch a live value, what offline looks like.

- [ ] **Step 3: Write `writing-rules.md`** with these sections:

- `## Anatomy of a rule` — id, name, enabled, priority, trigger (interval or on-change with dead-band), and the flow (start node → inputs → decisions → actions). Point to `../domain/control-strategies.md` for the worked strategy example.
- `## Via the visual editor` — the frontend flow editor edits the full visual JSON; saving persists both stored representations together (link `../concepts/rule-engine.md` for why).
- `## Via the HTTP API` — the actual endpoints from `rule_routes.rs` (list them with methods), with one `curl` example creating a minimal rule.
- `## Testing a rule` — there is no dry-run; executing a rule performs real actions. Recommended flow: point the rule at a Virtual-protocol channel first, execute, check the execution record, then re-target production.
- `## Reload` — the reload endpoint applies changes immediately.

- [ ] **Step 4: Verify** — shared check for both files

- [ ] **Step 5: Commit**

```bash
git add docs/guides/connect-devices.md docs/guides/writing-rules.md
git commit -m "docs: add guides/connect-devices and guides/writing-rules"
```

---

### Task 12: `docs/guides/ai-assistants.md`

**Files:**
- Create: `docs/guides/ai-assistants.md`

**Frontmatter:**
- title: `Connect AI Assistants`
- description: `Point Claude or any MCP client at aether mcp, and choose between read-only and write access`

- [ ] **Step 1: Read grounding sources**

- `tools/aether/src/mcp.rs` — module doc, tool inventory
- `tools/aether/src/main.rs` — the `Commands::Mcp` arm: which env vars configure the five service base URLs (`AETHER_COMSRV_URL`, `AETHER_MODSRV_URL`, `AETHER_ALARMSRV_URL`, `AETHER_NETSRV_URL`, `AETHER_HISSRV_URL`) and the `--host` flag

- [ ] **Step 2: Write the page** with these sections:

- `## What you get` — the MCP server exposes the CLI's capabilities as tools (24 read-only; 27 more write tools behind a flag) plus embedded documentation resources (domain knowledge, architecture, this tool reference) so an assistant can learn the system without leaving the session.
- `## Claude Desktop` — the config snippet:

```json
{
  "mcpServers": {
    "aether": {
      "command": "aether",
      "args": ["mcp"]
    }
  }
}
```

- `## Claude Code` — `claude mcp add aether -- aether mcp`
- `## Pointing at a remote system` — the five `AETHER_*_URL` env vars (or `--host`) so the MCP server on your laptop can drive an edge device; show one example with `env` in the Claude Desktop config.
- `## Read-only vs write access` — default is read-only: write tools are not registered and do not appear in `tools/list`. Enabling `--allow-write` is a deliberate act; before doing it, read `../domain/safe-operations.md` (link prominently). One-line rule: give an assistant write access for a task, not as a default.
- `## Resources` — the server also serves `aether://docs/...` resources; clients that support MCP resources can pull domain context directly (list a few example URIs).

- [ ] **Step 3: Verify** — shared check with `f=docs/guides/ai-assistants.md`

- [ ] **Step 4: Commit**

```bash
git add docs/guides/ai-assistants.md
git commit -m "docs: add guides/ai-assistants, MCP client setup and access model"
```

---

### Task 13: `docs/reference/cli.md`

**Files:**
- Create: `docs/reference/cli.md`

**Frontmatter:**
- title: `CLI Reference`
- description: `Every aether command: services, sync, doctor, channels, rules, and more`

- [ ] **Step 1: Generate the ground truth from the binary itself**

```bash
cargo build -p aether --quiet
./target/debug/aether --help
# then for every subcommand listed:
./target/debug/aether <subcommand> --help
```

The clap output is the source of truth. Do not document flags from memory.

- [ ] **Step 2: Write the page**

- `## Global flags` — `--json`, `--verbose`, `--host`, and the env vars they interact with (from the top-level help).
- One `## aether <subcommand>` section per top-level subcommand, in help-output order. Each section: the one-line description from help, a fenced usage block, a table of flags/args where non-trivial, and one realistic example invocation. Nested subcommands (e.g. `channels …`, `net mqtt …`) get `###` subsections.
- `## Exit codes and JSON mode` — `--json` emits a `{success, ...}` envelope on stdout with diagnostics on stderr.

- [ ] **Step 3: Verify** — shared check, plus cross-check no subcommand was missed:

```bash
./target/debug/aether --help | awk '/^Commands:/,/^Options:/' | awk '{print $1}' | grep -v '^$\|Commands:\|Options:\|help' | while read -r c; do
  grep -q "## aether $c" docs/reference/cli.md || echo "MISSING: $c"
done
```

Expected: no `MISSING:` lines.

- [ ] **Step 4: Commit**

```bash
git add docs/reference/cli.md
git commit -m "docs: add reference/cli generated against the real binary help"
```

---

### Task 14: `docs/reference/configuration.md` + `docs/reference/http-api.md`

**Files:**
- Create: `docs/reference/configuration.md`
- Create: `docs/reference/http-api.md`

**Frontmatter (configuration):**
- title: `Configuration Reference`
- description: `YAML configuration schema, the sync pipeline, and environment variables`

**Frontmatter (http-api):**
- title: `HTTP API`
- description: `Response envelope conventions, authentication, and service endpoint overview`

- [ ] **Step 1: Read grounding sources**

- `config.template/` — full tree: `global.yaml`, `comsrv/comsrv.yaml`, `modsrv/{modsrv.yaml,instances.yaml,calculations.yaml,rules/,instances/}`
- `.env.example` — every variable with its comment
- `libs/common/src/api_types.rs` (grep for `SuccessResponse`) — the success envelope shape
- Spec `docs/superpowers/specs/2026-07-09-cli-web-parity-design.md` — endpoint inventory per service, if present; otherwise grep each service's route registrations (`Router::new()` / `.route(` in `services/*/src/`)

- [ ] **Step 2: Write `configuration.md`** with these sections:

- `## The sync pipeline` — YAML in `config/` → `aether sync` → SQLite → services read SQLite at startup. Edits to YAML do nothing until sync'd. `aether init` scaffolds the config directory from the template.
- `## Directory layout` — annotated tree of `config.template/` explaining each file's role (global settings, comsrv channels, modsrv instances/calculations/rules, per-instance directories, custom products dir).
- `## Environment variables` — a table generated from `.env.example`: variable, default, purpose. Every variable in the file appears in the table.

- [ ] **Step 3: Write `http-api.md`** with these sections:

- `## Envelope conventions` — success is uniformly `{ success: true, data, metadata? }`. Errors come in three shapes that coexist (document all three honestly, with an example each): typed errors `{ success: false, error: { code, message, ... } }`, inline handler errors `{ success: false, message }`, and the gateway's error mapping `{ error_code, message, category, retryable, retry_delay_ms }`. A client must branch on shape.
- `## Authentication` — JWT via the API gateway; direct service ports are unauthenticated and intended for intra-host use.
- `## Endpoint overview` — one table per service (comsrv, modsrv, hissrv, apigateway, alarmsrv, netsrv): method, path, one-line purpose. This is an overview, not an exhaustive parameter reference — for interactive exploration, build with `--enable-swagger` and browse the OpenAPI UI.
- `## WebSocket` — the gateway's WebSocket endpoint for live values and rule execution events (ground in `services/apigateway/src/` route registrations).

- [ ] **Step 4: Verify** — shared check for both files, plus:

```bash
# every env var in .env.example is documented
grep -oE "^[A-Z_]+=" .env.example | tr -d '=' | while read -r v; do
  grep -q "$v" docs/reference/configuration.md || echo "MISSING: $v"
done
```

- [ ] **Step 5: Commit**

```bash
git add docs/reference/configuration.md docs/reference/http-api.md
git commit -m "docs: add reference/configuration and reference/http-api"
```

---

### Task 15: `scripts/gen-mcp-docs.sh` + generated `docs/reference/mcp-tools.md`

**Files:**
- Create: `scripts/gen-mcp-docs.sh`
- Create (generated): `docs/reference/mcp-tools.md`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Regenerate docs/reference/mcp-tools.md from the aether binary's own
# tools/list output. Run this after adding or changing MCP tools.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p aether --quiet

REQ='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"gen-mcp-docs","version":"0"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

# The server exits on stdin EOF, which the pipe provides. timeout(1) is a
# belt-and-braces guard where available (GNU coreutils); absent on stock macOS.
if command -v timeout >/dev/null 2>&1; then T="timeout 10"; else T=""; fi

RO_JSON=$(printf '%s\n' "$REQ" | $T ./target/debug/aether mcp 2>/dev/null | tail -1)
ALL_JSON=$(printf '%s\n' "$REQ" | $T ./target/debug/aether mcp --allow-write 2>/dev/null | tail -1)

RO_JSON="$RO_JSON" ALL_JSON="$ALL_JSON" python3 > docs/reference/mcp-tools.md <<'PY'
import json, os, datetime

ro = {t["name"] for t in json.loads(os.environ["RO_JSON"])["result"]["tools"]}
tools = json.loads(os.environ["ALL_JSON"])["result"]["tools"]

groups = {}
for t in tools:
    groups.setdefault(t["name"].split("_", 1)[0], []).append(t)

print("---")
print("title: MCP Tools Reference")
print("description: All MCP tools grouped by domain, with parameters and write-safety markers")
print(f"updated: {datetime.date.today().isoformat()}")
print("---")
print()
print("# MCP Tools Reference")
print()
print(f"Generated by `scripts/gen-mcp-docs.sh` from the server's `tools/list`.")
print(f"{len(tools)} tools total; tools marked **WRITE** are registered only")
print("when the server runs with `--allow-write`. See")
print("[Safe Operations](../domain/safe-operations.md) before enabling writes.")

for group in sorted(groups):
    print(f"\n## {group}\n")
    for t in sorted(groups[group], key=lambda t: t["name"]):
        marker = "read-only" if t["name"] in ro else "**WRITE**"
        print(f"### `{t['name']}` ({marker})\n")
        print((t.get("description") or "").strip() + "\n")
        schema = t.get("inputSchema") or {}
        props = schema.get("properties") or {}
        required = set(schema.get("required") or [])
        if props:
            print("| Parameter | Type | Required | Description |")
            print("|---|---|---|---|")
            for name in sorted(props):
                p = props[name]
                ptype = p.get("type", "-")
                if isinstance(ptype, list):
                    ptype = "/".join(ptype)
                desc = (p.get("description") or "").replace("|", "\\|").replace("\n", " ")
                req = "yes" if name in required else "no"
                print(f"| `{name}` | {ptype} | {req} | {desc} |")
            print()
PY

echo "wrote docs/reference/mcp-tools.md"
```

- [ ] **Step 2: Make it executable and run it**

```bash
chmod +x scripts/gen-mcp-docs.sh
./scripts/gen-mcp-docs.sh
```

Expected: `wrote docs/reference/mcp-tools.md`, and the file contains 51 `### ` tool headings (24 read-only + 27 WRITE).

```bash
grep -c "^### " docs/reference/mcp-tools.md          # expect 51
grep -c "(\*\*WRITE\*\*)" docs/reference/mcp-tools.md  # expect 27
```

- [ ] **Step 3: Check idempotency**

```bash
cp docs/reference/mcp-tools.md /tmp/mcp-tools.before
./scripts/gen-mcp-docs.sh
diff /tmp/mcp-tools.before docs/reference/mcp-tools.md && echo idempotent
```

Expected: `idempotent` (same-day runs produce identical output; only `updated:` varies across days).

- [ ] **Step 4: Commit**

```bash
git add scripts/gen-mcp-docs.sh docs/reference/mcp-tools.md
git commit -m "feat: gen-mcp-docs script and generated MCP tools reference"
```

---

### Task 16: root `llms.txt`

**Files:**
- Create: `llms.txt` (repo root)

- [ ] **Step 1: Write the file** — exactly this content (descriptions are the per-task frontmatter descriptions; if any page task adjusted its description during writing, use the frontmatter as-committed — run `grep -h "^description:" docs/**/**.md` to collect them):

```
# AetherEMS

> AI-native industrial energy management system built in Rust. Multi-protocol
> device acquisition, a sub-millisecond shared-memory data plane, a visual rule
> engine, and an MCP server that lets AI agents query and (with explicit
> authorization) operate the system.

## Domain Knowledge

- [Energy Storage Primer](docs/domain/ess-primer.md): How ESS concepts - PCS, BMS, SOC, grid interface - map onto Aether products, instances, and points
- [Built-in Product Models](docs/domain/product-models.md): The 13 built-in device models, their hierarchy, and the meaning of every measurement and action point
- [Control Strategies as Rules](docs/domain/control-strategies.md): Expressing SOC management, peak shaving, and demand control as executable rule flows
- [Safe Operations for AI Agents](docs/domain/safe-operations.md): Which writes reach real devices, how write gating works, and the operating rules an AI agent must follow

## Concepts

- [System Architecture](docs/concepts/architecture.md): Seven services, their ports, and how they communicate over shared memory, UDS, Redis, HTTP, and MQTT
- [Data Model](docs/concepts/data-model.md): Products, instances, and T/S/C/A points - and why an instance is a pure thing-model with no status field
- [Shared Memory](docs/concepts/shared-memory.md): The SHM data plane: slot layout, writer ownership, seqlock reads, generations, and the PointWatch event plane
- [Rule Engine](docs/concepts/rule-engine.md): Dual-column rule storage, tick and event scheduling, execution, and hot reload
- [Data Flow](docs/concepts/data-flow.md): Uplink and downlink paths end to end, with latency budgets and the Redis keyspace

## Guides

- [Getting Started](docs/guides/getting-started.md): Build the workspace, initialize configuration, start services, and verify health
- [Connect Devices](docs/guides/connect-devices.md): Configure channels, choose protocols, and map device points to instances
- [Writing Rules](docs/guides/writing-rules.md): Author rules through the HTTP API or the visual flow editor
- [Connect AI Assistants](docs/guides/ai-assistants.md): Point Claude or any MCP client at aether mcp, and choose between read-only and write access
- [Deployment](docs/guides/deployment.md): Run with Docker Compose or build a self-contained installer for edge devices

## Reference

- [CLI Reference](docs/reference/cli.md): Every aether command: services, sync, doctor, channels, rules, and more
- [MCP Tools Reference](docs/reference/mcp-tools.md): All MCP tools grouped by domain, with parameters and write-safety markers
- [Configuration Reference](docs/reference/configuration.md): YAML configuration schema, the sync pipeline, and environment variables
- [HTTP API](docs/reference/http-api.md): Response envelope conventions, authentication, and service endpoint overview
```

- [ ] **Step 2: Verify** — every linked file exists and every description matches its page frontmatter:

```bash
grep -oE "\(docs/[a-z-]+/[a-z-]+\.md\)" llms.txt | tr -d '()' | while read -r f; do
  [ -f "$f" ] || echo "MISSING FILE: $f"
  d_index=$(grep -F "($f)" llms.txt | sed 's/.*): //')
  d_page=$(grep "^description:" "$f" | sed 's/^description: //')
  [ "$d_index" = "$d_page" ] || echo "DESC MISMATCH: $f"
done
echo done
```

Expected: only `done`.

- [ ] **Step 3: Commit**

```bash
git add llms.txt
git commit -m "docs: add root llms.txt indexing the docs corpus"
```

---

### Task 17: MCP resources — `mcp_docs.rs` catalog + `mcp.rs` wiring

**Files:**
- Create: `tools/aether/src/mcp_docs.rs`
- Modify: `tools/aether/src/main.rs` (add `mod mcp_docs;` next to `mod mcp;`)
- Modify: `tools/aether/src/mcp.rs` (imports, `get_info`, two new trait methods, one capability test)

All 11 embedded files already exist (Tasks 1–15). Runtime code must not use `unwrap`/`expect` (clippy denies them in `--lib --bins`); tests may.

- [ ] **Step 1: Write `mcp_docs.rs` with failing tests (RED)**

Create `tools/aether/src/mcp_docs.rs`:

```rust
//! Embedded documentation resources served over MCP `resources/*`.
//!
//! Content is compiled into the binary so a deployed edge device serves docs
//! that always match the tool set it ships with. The catalog is the curated
//! subset an already-connected assistant needs: domain know-how, concepts,
//! the tool reference, and the assistant-setup guide. Deployment/getting-
//! started guides are deliberately absent.

pub(crate) struct DocResource {
    pub uri: &'static str,
    pub body: &'static str,
}

pub(crate) const DOC_RESOURCES: &[DocResource] = &[];

/// Extract a scalar field from the leading YAML frontmatter block
/// (`---\nkey: value\n...\n---`). Returns `None` when there is no
/// frontmatter or the key is absent.
pub(crate) fn frontmatter_field<'a>(_body: &'a str, _key: &str) -> Option<&'a str> {
    None
}

/// Programmatic resource name: the last URI path segment.
pub(crate) fn resource_name(uri: &str) -> &str {
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_eleven_unique_doc_uris() {
        assert_eq!(DOC_RESOURCES.len(), 11);
        let mut uris: Vec<_> = DOC_RESOURCES.iter().map(|d| d.uri).collect();
        uris.sort_unstable();
        uris.dedup();
        assert_eq!(uris.len(), 11, "duplicate resource URIs");
        for d in DOC_RESOURCES {
            assert!(d.uri.starts_with("aether://docs/"), "bad uri {}", d.uri);
        }
    }

    #[test]
    fn every_embedded_doc_has_frontmatter_and_substance() {
        for d in DOC_RESOURCES {
            assert!(
                frontmatter_field(d.body, "title").is_some(),
                "{} missing frontmatter title",
                d.uri
            );
            assert!(
                frontmatter_field(d.body, "description").is_some(),
                "{} missing frontmatter description",
                d.uri
            );
            assert!(d.body.len() > 500, "{} is suspiciously short", d.uri);
        }
    }

    #[test]
    fn frontmatter_field_parses_and_rejects() {
        let body = "---\ntitle: Hello\ndescription: World thing\n---\n# Body";
        assert_eq!(frontmatter_field(body, "title"), Some("Hello"));
        assert_eq!(frontmatter_field(body, "description"), Some("World thing"));
        assert_eq!(frontmatter_field(body, "updated"), None);
        assert_eq!(frontmatter_field("# no frontmatter", "title"), None);
        // a key appearing in the body, not the frontmatter, must not match
        assert_eq!(frontmatter_field("---\na: b\n---\ntitle: sneaky", "title"), None);
    }

    #[test]
    fn resource_name_is_last_segment() {
        assert_eq!(resource_name("aether://docs/domain/ess-primer"), "ess-primer");
    }
}
```

Register the module in `tools/aether/src/main.rs` — find the `mod mcp;` line and add below it:

```rust
mod mcp_docs;
```

- [ ] **Step 2: Run tests, confirm RED**

```bash
cargo test -p aether mcp_docs
```

Expected: FAIL — `catalog_has_eleven_unique_doc_uris` (0 != 11), `frontmatter_field_parses_and_rejects` (None != Some), `resource_name_is_last_segment`.

- [ ] **Step 3: Implement (GREEN)**

Replace the three stubs in `mcp_docs.rs`:

```rust
pub(crate) const DOC_RESOURCES: &[DocResource] = &[
    DocResource {
        uri: "aether://docs/domain/ess-primer",
        body: include_str!("../../../docs/domain/ess-primer.md"),
    },
    DocResource {
        uri: "aether://docs/domain/product-models",
        body: include_str!("../../../docs/domain/product-models.md"),
    },
    DocResource {
        uri: "aether://docs/domain/control-strategies",
        body: include_str!("../../../docs/domain/control-strategies.md"),
    },
    DocResource {
        uri: "aether://docs/domain/safe-operations",
        body: include_str!("../../../docs/domain/safe-operations.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/architecture",
        body: include_str!("../../../docs/concepts/architecture.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/data-model",
        body: include_str!("../../../docs/concepts/data-model.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/shared-memory",
        body: include_str!("../../../docs/concepts/shared-memory.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/rule-engine",
        body: include_str!("../../../docs/concepts/rule-engine.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/data-flow",
        body: include_str!("../../../docs/concepts/data-flow.md"),
    },
    DocResource {
        uri: "aether://docs/guides/ai-assistants",
        body: include_str!("../../../docs/guides/ai-assistants.md"),
    },
    DocResource {
        uri: "aether://docs/reference/mcp-tools",
        body: include_str!("../../../docs/reference/mcp-tools.md"),
    },
];

pub(crate) fn frontmatter_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = body.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    for line in rest[..end].lines() {
        if let Some(value) = line.strip_prefix(key).and_then(|v| v.strip_prefix(':')) {
            return Some(value.trim());
        }
    }
    None
}

pub(crate) fn resource_name(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or(uri)
}
```

- [ ] **Step 4: Run tests, confirm GREEN**

```bash
cargo test -p aether mcp_docs
```

Expected: 4 passed.

- [ ] **Step 5: Wire the trait methods in `mcp.rs` with a failing capability test (RED)**

Add to the existing `mod tests` in `tools/aether/src/mcp.rs`:

```rust
#[test]
fn resources_capability_is_advertised_and_catalog_nonempty() {
    let server = AetherMcp::new(&test_urls("http://localhost:1"), false).unwrap();
    let info = server.get_info();
    assert!(
        info.capabilities.resources.is_some(),
        "resources capability missing from get_info"
    );
    assert!(!crate::mcp_docs::DOC_RESOURCES.is_empty());
}
```

Run: `cargo test -p aether resources_capability` — expected: FAIL (`resources` is `None`).

- [ ] **Step 6: Implement the wiring (GREEN)**

In `tools/aether/src/mcp.rs`, extend the imports:

```rust
use rmcp::model::{
    CallToolResult, ContentBlock, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};
```

Replace the `impl ServerHandler for AetherMcp` block (the `#[tool_handler]` attribute stays; it generates the tool methods and leaves the rest to us):

```rust
#[tool_handler(router = self.tool_router)]
impl ServerHandler for AetherMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = crate::mcp_docs::DOC_RESOURCES
            .iter()
            .map(|d| {
                let mut r = Resource::new(d.uri, crate::mcp_docs::resource_name(d.uri))
                    .with_mime_type("text/markdown");
                if let Some(title) = crate::mcp_docs::frontmatter_field(d.body, "title") {
                    r = r.with_title(title);
                }
                if let Some(desc) = crate::mcp_docs::frontmatter_field(d.body, "description") {
                    r = r.with_description(desc);
                }
                r
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let doc = crate::mcp_docs::DOC_RESOURCES
            .iter()
            .find(|d| d.uri == request.uri)
            .ok_or_else(|| {
                ErrorData::resource_not_found(
                    format!("unknown resource: {}", request.uri),
                    None,
                )
            })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: doc.uri.to_string(),
                mime_type: Some("text/markdown".to_string()),
                text: doc.body.to_string(),
                meta: None,
            },
        ]))
    }
}
```

Note: `ResourceContents` is a `#[non_exhaustive]` enum, which restricts *matching*, not variant construction — the literal above compiles. If a future rmcp adds fields, switch to `ResourceContents::text(body, uri)` and accept its `text/plain` mime type.

- [ ] **Step 7: Run the full crate test suite, confirm GREEN**

```bash
cargo test -p aether
```

Expected: all pass, including the 4 `mcp_docs` tests and `resources_capability_is_advertised_and_catalog_nonempty`.

- [ ] **Step 8: End-to-end smoke over real stdio**

```bash
cargo build -p aether --quiet
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"aether://docs/domain/safe-operations"}}' \
  | ./target/debug/aether mcp 2>/dev/null | python3 -c "
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    if msg.get('id') == 2:
        n = len(msg['result']['resources'])
        assert n == 11, f'expected 11 resources, got {n}'
        for r in msg['result']['resources']:
            assert r.get('description'), f\"{r['uri']} has no description\"
    if msg.get('id') == 3:
        text = msg['result']['contents'][0]['text']
        assert 'allow-write' in text, 'safe-operations body missing expected content'
print('OK: resources/list=11 with descriptions, resources/read returns page body')
"
```

Expected: the final `OK:` line. Also verify `--verbose` keeps stdout clean with resources in play:

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}' \
  | ./target/debug/aether --verbose mcp 2>/dev/null | python3 -c "
import json, sys
[json.loads(l) for l in sys.stdin if l.strip()]
print('OK: stdout is pure JSON-RPC')
"
```

- [ ] **Step 9: Quality gate and commit**

```bash
./scripts/quick-check.sh
git add tools/aether/src/mcp_docs.rs tools/aether/src/mcp.rs tools/aether/src/main.rs
git commit -m "feat: serve embedded docs as MCP resources"
```

---

### Task 18: README repositioning

**Files:**
- Modify: `README.md`
- Modify: `README-CN.md`

- [ ] **Step 1: Update `README.md`**

1. Replace the current positioning line ("Industrial IoT energy management system built with Rust. …") with:

```markdown
AI-native energy management system built in Rust. Every capability is exposed three ways: a web console for humans, a CLI for scripts, and an MCP server for AI agents — 51 tools plus embedded energy-storage domain knowledge, with writes gated behind explicit authorization. Multi-protocol acquisition, a sub-millisecond shared-memory data plane, and a visual rule engine run the plant; the MCP layer lets an AI assistant operate it safely.
```

2. Prepend a new first bullet to the `## Features` list:

```markdown
- **AI-Native Operations** — `aether mcp` exposes the full system as MCP tools (read-only by default; device writes require `--allow-write`) and serves embedded domain docs as MCP resources; the repo ships [`llms.txt`](llms.txt) for AI ingestion
```

3. Insert a new section after `## Features` (before `## Architecture`):

```markdown
## AI-Native

AetherEMS treats AI agents as first-class operators:

- **MCP server** — `aether mcp` speaks the Model Context Protocol over stdio. 24 read-only tools cover channels, instances, rules, alarms, history, and routing; 27 write tools register only with `--allow-write`, so an unauthorized agent cannot even see them.
- **Embedded knowledge** — the binary serves the energy-storage domain docs (`aether://docs/...`) as MCP resources, so a connected assistant learns what a PCS is, why instances have no status field, and which writes reach real hardware — without leaving the session.
- **AI-readable docs** — [`llms.txt`](llms.txt) indexes the full corpus under [`docs/`](docs/).

Connect Claude Desktop:

​```json
{
  "mcpServers": {
    "aether": { "command": "aether", "args": ["mcp"] }
  }
}
​```

Or Claude Code: `claude mcp add aether -- aether mcp`. Before enabling `--allow-write`, read [Safe Operations](docs/domain/safe-operations.md).
```

(Remove the zero-width characters around the inner code fence when writing — they exist here only to nest the fence.)

- [ ] **Step 2: Mirror the same three edits in `README-CN.md`** in Chinese: 定位句 (AI 原生的能源管理系统…三种操作面：Web 控制台/CLI/MCP), Features 首条 (**AI 原生操作**…), and an `## AI 原生` section with the same config snippets and a link to `docs/domain/safe-operations.md`.

- [ ] **Step 3: Verify**

```bash
grep -n "AI-native\|AI-Native" README.md | head -5     # headline + feature + section present
grep -n "llms.txt" README.md                            # linked
grep -n "AI 原生" README-CN.md | head -3
```

- [ ] **Step 4: Commit**

```bash
git add README.md README-CN.md
git commit -m "docs: reposition README as AI-native EMS with MCP quick start"
```

---

### Task 19: Final verification (no new code)

- [ ] **Step 1: Corpus integrity**

```bash
# 18 pages exist
find docs/domain docs/concepts docs/guides docs/reference -name "*.md" | wc -l   # expect 18
# no placeholders anywhere in the corpus
grep -rnE "TBD|TODO|FIXME|XXX|placeholder|coming soon" docs/domain docs/concepts docs/guides docs/reference && echo "FIX THESE" || echo clean
# every page has frontmatter
for f in docs/{domain,concepts,guides,reference}/*.md; do
  head -1 "$f" | grep -q '^---$' || echo "NO FRONTMATTER: $f"
done; echo done
# llms.txt still consistent (re-run Task 16 Step 2 check)
```

- [ ] **Step 2: Relative links resolve**

```bash
python3 - <<'PY'
import os, re
bad = []
for sub in ("domain", "concepts", "guides", "reference"):
    d = os.path.join("docs", sub)
    for fn in sorted(os.listdir(d)):
        if not fn.endswith(".md"):
            continue
        p = os.path.join(d, fn)
        with open(p) as fh:
            body = fh.read()
        for link in re.findall(r"\]\(([^)#\s]+\.md)\)", body):
            if link.startswith("http"):
                continue
            target = os.path.normpath(os.path.join(os.path.dirname(p), link))
            if not os.path.isfile(target):
                bad.append(f"BROKEN: {p} -> {link}")
print("\n".join(bad) if bad else "all links resolve")
PY
```

Expected: `all links resolve`.

- [ ] **Step 3: Full quality gate**

```bash
./scripts/quick-check.sh
cargo test -p aether 2>&1 | grep -E "^test result:"
```

Expected: quick-check passes; aether test count = pre-plan count + 5 (4 in `mcp_docs::tests`, 1 capability test in `mcp::tests`).

- [ ] **Step 4: Generator freshness**

```bash
./scripts/gen-mcp-docs.sh && git diff --stat docs/reference/mcp-tools.md
```

Expected: empty diff (or only the `updated:` date line if the calendar day changed — commit it if so).

- [ ] **Step 5: Final tally — no commit unless a fix was needed**

```bash
git log --oneline main@{1}..HEAD 2>/dev/null || git log --oneline -20
git diff HEAD~15..HEAD --stat | tail -3
```

If Steps 1–4 surfaced a real gap, fix it as its own small commit referencing which check caught it.

---

## Spec Coverage

| Spec requirement | Task |
|---|---|
| 4 domain know-how pages grounded in real code/config | Tasks 1–4 |
| 5 concepts pages | Tasks 5–9 |
| 5 guides pages | Tasks 10–12 |
| 4 reference pages (one generated) | Tasks 13–15 |
| Frontmatter (title/description/updated) on every page | Rules section + every page task |
| Root `llms.txt` with per-page descriptions | Task 16 |
| description consistency across frontmatter/llms.txt/resources | Task 16 Step 2 check + Task 17 tests |
| `resources/list` + `resources/read`, 11 curated resources | Task 17 |
| `include_str!` static catalog, no rust-embed | Task 17 |
| Resources unaffected by `--allow-write` (registered always) | Task 17 (catalog is unconditional; smoke test runs without the flag) |
| stdout purity with resources in play | Task 17 Step 8 |
| `gen-mcp-docs.sh` idempotent generator | Task 15 |
| README/README-CN AI-native repositioning | Task 18 |
| Zero overlap with CLAUDE.md / existing Chinese docs | Global rule (no task touches them) |
| quick-check passes | Task 17 Step 9, Task 19 Step 3 |

## Explicitly Not Doing

- No docs site, no `llms-full.txt`, no Chinese mirror of the new corpus (README-CN edits are sync of an existing bilingual pair, not a mirror)
- No CI freshness check for the generated page; regeneration is manual
- No MCP prompts, `resources/subscribe`, or resource templates
- No OpenAPI/clap/schema-driven generation pipelines (v2 direction)
- Not migrating or editing the existing Chinese `docs/*.md` files
