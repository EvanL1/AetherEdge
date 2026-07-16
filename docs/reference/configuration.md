---
title: Configuration Reference
description: YAML configuration schema, the sync pipeline, and environment variables
updated: 2026-07-11
---

# Configuration Reference

Operational configuration lives in YAML, CSV, and JSON files under a `config/`
directory and is imported into SQLite (`aether.db`) by `aether sync`. The one
startup-time exception is `global.yaml`'s `packs` list: automation and
`aether mcp` read that same entry directly so a Pack identity and root cannot
drift between the two processes.

## The sync pipeline

```
config/*.yaml, *.csv, *.json  →  aether sync  →  SQLite (aether.db)  →  services (at startup)
```

Editing a YAML file does nothing by itself. Offline `aether sync` requires the
configuration-owning services to be stopped, writes all desired-state heads in
one transaction, and takes effect on the next supervised service start. Online
channel, instance, routing, and rule mutations instead enter their governed
application commands and reconcile their runtime projections automatically.

`aether sync` (implemented in `tools/aether/src/core/syncer.rs`) processes
three targets inside one site-level SQLite transaction, so a failure in any
target leaves the database untouched:

- **global** — parses `config/global.yaml` into the `service_config` table.
- **aether-io** — parses `config/io/io.yaml` into the `channels` table
  and the per-channel CSV files into the four point tables
  (`telemetry_points`, `signal_points`, `control_points`,
  `adjustment_points`). Duplicate channel names abort the sync.
- **aether-automation** — parses `config/automation/automation.yaml`, `instances.yaml`, and
  `rules/*.json` into the instance and rules tables, imports measurement (`M`)
  entries from `instance_routing.csv`, and validates any external product JSON
  files under `config/automation/products/`. There is no
  standalone calculation-engine sync path — a previously-orphaned
  `calculations.yaml` template, its unused table, and its dead API schema
  types have been removed. Derived quantities are expressed with
  `calculation` nodes inside individual rules instead (see
  [Control Strategies as Rules](../domain/control-strategies.md)).

Before writing, `aether sync` validates all three domains. It then applies
global, IO, and automation configuration in one SQLite transaction, so an
error in a later domain rolls back all earlier changes. By default rows with
no corresponding config file (for example rules created through the HTTP API)
are preserved. With `--force`, managed tables are fully replaced, but
validation is still mandatory. Action (`A`) routing is deliberately outside
this compatibility importer: it selects the physical target of future device
commands and must use the authenticated, confirmed, audited action-routing
application command. An `A` row rolls back the whole sync. `--force` also
refuses to start while any action route exists, so it cannot cascade-delete a
commissioned command target. Delete or migrate those routes through the
governed routing API before removing their instance, channel, control point, or
adjustment point. Measurement routing remains sync-managed.

Two related commands are easy to confuse with sync:

- `aether init` initializes or upgrades the **database schema** only
  (`CREATE TABLE IF NOT EXISTS`, migration-only — it refuses to reset an
  existing database). It does not create or copy any config files.
- The `config/` directory itself is scaffolded at **deploy time**: the
  Docker installer (`scripts/install.sh`) stages `config.template/` alongside
  the binaries and activates it at `<data-dir>/config/` only on a clean host.
  Any existing site configuration makes the fresh-only installer fail before
  it writes. Containers mount the new directory at `/app/config/`; the
  installer does not merge, upgrade, or import operator-owned configuration.
  In a development checkout, `aether setup` plans and activates only the four
  site-authored safe files under `./data/config` and initializes
  `./data/aether.db` after the returned plan ID is explicitly applied. The
  developer must then provide the explicit composition manifest described
  below; setup never guesses which IO features were compiled.

## Directory layout

The repository's `config.template/` directory is the canonical fail-safe
starting point. It contains no commissioned channel, device instance, or
enabled control rule. Domain examples are opt-in; the energy examples live
under `packs/energy/examples/config/`. Annotated:

```
config.template/
├── global.yaml                 # Shared settings: active Packs, API bind
│                               # host, log level/rotation, rule scheduler
│                               # tick interval (rules.tick_ms, default 100)
├── runtime-manifest.json       # Generated, checksummed build composition;
│                               # never inferred or edited by site setup
├── io/
│   ├── io.yaml                 # Empty channel list until commissioning
│   │                           # (modbus_tcp, modbus_rtu, can, mqtt, http,
│   │                           # di_do, ...), enabled flag, per-protocol
│   │                           # connection parameters, per-channel logging
│   └── <channel-id>/           # (expected by the syncer; not shipped in
│       │                       # the template) One directory per channel,
│       │                       # named by its numeric channel id (e.g. 1/)
│       ├── telemetry.csv       # T (telemetry) point definitions
│       ├── signal.csv          # S (signal) point definitions
│       ├── control.csv         # C (control) point definitions
│       ├── adjustment.csv      # A (adjustment) point definitions
│       └── mapping/            # Protocol register mappings, one CSV per
│                               # point type (telemetry_mapping.csv, ...)
└── automation/
    ├── automation.yaml         # Instance auto-load is disabled by default
    ├── instances.yaml          # Empty instance map until commissioning
    ├── instances/              # Optional per-instance directories, each
    │   └── <name>/instance.yaml  # holding one instance definition
    ├── rules/                  # One JSON file per control rule (Vue Flow
    │   └── *.json              # graph: nodes, edges, priority, enabled)
    └── products/               # (optional, not in the template) Site-owned
                                # product JSON files; when present they may
                                # override models from an active Pack
```

Point-type shorthand: Aether uses T (telemetry), S (signal), C (control), and
A (adjustment) for the four point classes throughout its APIs and file
formats.

The fail-safe default in `global.yaml` is `packs: []`, so a fresh site exposes
zero domain products and no Pack-owned MCP knowledge. An installed Pack is
activated with one identity-bound root:

```yaml
packs:
  - id: energy
    root: /opt/aether/packs/energy
```

The manifest identity must match `id`; compatibility, capability, protocol,
commissioning, and asset confinement checks must all pass. A relative `root`
is resolved from the configuration directory and cannot contain `..`.
If `automation.yaml` sets `products_path`, that site-owned directory is loaded
last and may deliberately override a model from an active Pack. Both runtime
loading and `aether sync` reject symlinks, non-regular/oversized JSON, invalid
JSON, and duplicate product names within one directory.

`runtime-manifest.json` is mandatory beside `global.yaml`. It is generated by
the runtime composition or installer, not authored by a Pack or inferred by an
individual service. The closed v1 document records the Aether release, target,
included services, exact `aether-io` protocol features, derived adapters, and
application capabilities under a canonical SHA-256 checksum. Automation and
MCP reject missing, tampered, version-mismatched, target-mismatched, unknown,
feature-inconsistent, symlinked, non-regular, or oversized manifests before
activating any Pack. For an explicit local development composition, generate
it with:

```bash
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
cargo run -p aether-runtime-catalog --bin aether-runtime-manifest -- \
  generate "$HOST_TARGET" data/config
```

Pass a third comma-separated argument to `generate` for a deliberately trimmed
IO feature set; there is no fallback that assumes all adapters are present.
Use `aether runtime-manifest` (or `--path <artifact>`) to run the same verifier
used by the installers, Automation, and MCP.

## Environment variables

Key variables used by Docker Compose and the services (most optional values are
illustrated in `.env.example`; deployment overrides add required production
gates):

| Variable | Default | Purpose |
|----------|---------|---------|
| `AETHER_BASE_PATH` | `./data` | Base path for site configuration and databases; logs use `AETHER_LOG_PATH` |
| `HOST_UID` | `1000` | User id for container processes; must match the host user to avoid file-permission issues |
| `HOST_GID` | `1000` | Group id for container processes; pairs with `HOST_UID` |
| `DIALOUT_GID` | `20` | Dialout group id for serial-port access (Linux only) |
| `INFLUXDB_URL`, `INFLUXDB_ORG`, `INFLUXDB_BUCKET`, `INFLUXDB_TOKEN`, `INFLUXDB_PASSWORD` | unset | Optional InfluxDB history adapter only; unused by the default runtime |
| `AETHER_IO_URL` | `http://127.0.0.1:6001` | io base URL for the API gateway and the `aether` CLI |
| `AETHER_AUTOMATION_URL` | `http://127.0.0.1:6002` | automation base URL for the API gateway and the `aether` CLI |
| `AETHER_SHM_PATH` | platform-selected tmpfs path | Canonical authoritative point-state segment shared by io and read-only consumers |
| `AETHER_CHANNEL_HEALTH_SHM_PATH` | sibling `*-health` path | Separate authoritative channel-connectivity segment; normally derived from `AETHER_SHM_PATH` |
| `SHM_WRITER_STALE_AFTER_MS` | `30000` | Maximum writer-heartbeat age accepted by read-side SHM adapters |
| `SHM_IDENTITY_CHECK_INTERVAL_MS` | `250` | Fallback interval for checking whether the canonical SHM inode was replaced; generation fencing handles normal swaps immediately |
| `SHM_TOPOLOGY_REFRESH_INTERVAL_MS` | `1000` (minimum `100`) | Interval used by API, alarm, and automation to reload one SQLite topology snapshot and atomically publish a validated point/health/routing generation |
| `JWT_SECRET_KEY` | unset (required) | Shared 32-byte-or-longer access-JWT signing/verification secret for aether-api plus governed io, automation, and alarm operations; installers generate it and keep it outside configuration assets |
| `AETHER_ACCESS_TOKEN` | unset | Signed Admin/Engineer access JWT required by governed CLI channel commissioning/lifecycle, device commands, action-routing changes, and automation/alarm policy operations, including MCP's 22 governed write tools; query commands do not require it on the local interface |
| `AETHER_UPLINK_CONTROL_TOKEN` | unset | Separate 32-byte-or-longer service credential used only for uplink-to-automation device commands; installers generate it and never print it |
| `AETHER_ALLOW_SIMULATION_WRITES` | `false` | Development-only opt-in for io T/S simulation writes into authoritative SHM; keep disabled in production |
| `AETHER_CONFIG_PATH` | unset | Shared configuration directory used by automation and `aether mcp`; CLI path resolution may set it through deployment context or `--config-path` |
| `AETHER_DATA_PATH` | unset | Overrides the install-context data directory for the `aether` CLI |
| `AETHER_INSTALL_CONTEXT_PATH` | `/etc/aether/install.yaml` | Overrides the installed layout descriptor; CLI flags and the two path variables take precedence |
| `AETHER_BOOTSTRAP_ADMIN_PASSWORD` | unset | Required only while `users` is empty; installers generate a strong value in their mode-0600 environment file, and it should be removed after the first password change |
| `AETHER_ALLOW_PUBLIC_REGISTRATION` | `false` | Explicit opt-in for anonymous Viewer registration; Admin creation is never available through public registration |
| `AETHER_DATA_PROCESSING_ENABLED` | `false` | Explicitly enables the opt-in Data Processing application and HTTP routes; startup fails closed if enabled configuration is invalid |
| `AETHER_DATA_PROCESSING_CONFIG` | `/app/data/config/data-processing/runtime.yaml` | Strict runtime YAML containing commissioned task, binding, history, covariate, processor, and audit composition |
| `AETHER_LOAD_FORECASTING_BEARER_TOKEN` | unset | Shared deployment secret used by `aether-api` to authenticate to the Load-Forecasting sidecar; required by the production override |
| `AETHER_LOAD_FORECASTING_REQUIRE_AUTH` | `false` in development | Processor-side startup gate; the production override fixes it to `true` |
| `AETHER_LOAD_FORECASTING_MAX_CONCURRENCY` | `1` | Bounds occupied model execution slots; cancellation does not release a slot until background work actually finishes |
| `AETHER_LOAD_FORECASTING_ARTIFACT_BUNDLES` | unset | Strict JSON array pinning every actual commissioned model/scaler/config artifact; required for production readiness |
| `AETHER_LOAD_FORECASTING_IMAGE` | mutable local development image | Production must use an immutable `@sha256` image reference through the explicit Compose override and preflight validator |
| `AETHER_LOAD_FORECASTING_PORT` | `8989` | Host-loopback published processor port for the Compose sidecar |
| `RUST_LOG` | `info` | Log level for the Rust services; supports filter syntax such as `info,io=debug,automation=trace` |

### Experimental CloudLink MQTT settings

The current `aether-uplink` production composition stays in deprecated
`legacy` mode. The experimental `aether-cloudlink-mqtt` embedding API exposes
the explicit `legacy`, `cloudlink-v1`, and `dual` migration values; it does not
silently enable CloudLink in an existing installation. The first real-broker
vertical slice is the opt-in test harness below. These variables are read only
when `AETHER_CLOUDLINK_RUN_INTEGRATION=1`:

| Variable | Default | Purpose |
|---|---|---|
| `AETHER_CLOUDLINK_RUN_INTEGRATION` | unset | Set exactly `1` to run the external-broker harness |
| `AETHER_CLOUDLINK_BROKER_HOST` | `127.0.0.1` | User-selected MQTT broker hostname/IP |
| `AETHER_CLOUDLINK_BROKER_PORT` | `1883` | User-selected broker port |
| `AETHER_CLOUDLINK_BROKER_USERNAME` | unset | Optional broker username |
| `AETHER_CLOUDLINK_BROKER_PASSWORD` | unset | Optional write-only broker password; never printed or serialized |
| `AETHER_CLOUDLINK_BROKER_TLS` | unset | Set `1` to use platform TLS roots |
| `AETHER_CLOUDLINK_BROKER_CA` | unset | Custom PEM CA path; selects custom TLS when present |
| `AETHER_CLOUDLINK_BROKER_CLIENT_CERT` | unset | Optional mTLS client certificate, configured with the key |
| `AETHER_CLOUDLINK_BROKER_CLIENT_KEY` | unset | Optional mTLS PKCS#8 private key, configured with the certificate |
| `AETHERCLOUD_ROOT` | unset | Optional read-only path used by joint orchestration outside this edge-only harness; the test does not modify or start it |

Plaintext is accepted only by the explicit development harness. Production
validation requires TLS. MQTT v3.1.1, QoS 1, non-retained messages, and exact
per-gateway topics are fixed by ADR-0017; MQTT 5 remains optional and cannot be
required for correctness.

For MCP writes, `--allow-write` only registers the 22-tool write allowlist. The
bridge sends `AETHER_ACCESS_TOKEN` as an `Authorization: Bearer` credential and
adds an `X-Request-ID`; every invocation still requires `confirmed: true`.
Preserve returned request/command IDs and do not automatically retry a timeout
or an incomplete audit/publication result. Channel mutations also return a
desired-state revision and may succeed with a degraded runtime projection;
inspect `request_id`, `resulting_revision`, and `reconciliation_required`
instead of retrying automatically.

### Data Processing and historian storage changes

The Data Processing runtime's `history.path` must name the SQLite file that
the running historian actually writes. Values under
`history_config.storage_*` are persisted desired settings. In particular,
`PUT /hisApi/storage` saves them but does not reconnect the active backend, so
matching those rows is not sufficient proof of the live writer. Change storage
only with Data Processing disabled; reconnect or restart `aether-history`,
verify its active backend/health and a commissioned sentinel series, then
restart `aether-api` with the matching runtime path.

The API also needs independent read-only OS permission to the historian
database/WAL/SHM directory. Keep that path separate from the API's writable
configuration/audit database. SQLite `mode=ro` over the base Compose
`/app/data:rw` mount is not a completed production permission boundary.

## Related pages

- [Getting Started](../guides/getting-started.md) — first setup and startup walkthrough
- [Connect Devices](../guides/connect-devices.md) — channel and point configuration in practice
- [Writing Rules](../guides/writing-rules.md) — the rule JSON that lives under `automation/rules/`
- [HTTP API](http-api.md) — the runtime API the synced configuration feeds
- [System Architecture](../concepts/architecture.md) — where each service fits
