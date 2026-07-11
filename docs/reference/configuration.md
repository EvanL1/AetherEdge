---
title: Configuration Reference
description: YAML configuration schema, the sync pipeline, and environment variables
updated: 2026-07-10
---

# Configuration Reference

Aether services never read YAML directly. All configuration lives in YAML,
CSV, and JSON files under a `config/` directory, is imported into a SQLite
database (`aether.db`) by the `aether sync` CLI command, and is read from
SQLite when a service starts. This page documents the pipeline, the file
layout, and the environment variables recognized by the Docker deployment and
the CLI.

## The sync pipeline

```
config/*.yaml, *.csv, *.json  →  aether sync  →  SQLite (aether.db)  →  services (at startup)
```

Editing a YAML file does nothing by itself. The change takes effect only
after `aether sync` writes it into SQLite and the affected service reloads —
either by restarting or through a reload endpoint such as io's
`POST /api/channels/reload` or automation's `POST /api/scheduler/reload`.

`aether sync` (implemented in `tools/aether/src/core/syncer.rs`) processes
three targets inside one site-level SQLite transaction, so a failure in any
target leaves the database untouched:

- **global** — parses `config/global.yaml` into the `service_config` table.
- **aether-io** — parses `config/io/io.yaml` into the `channels` table
  and the per-channel CSV files into the four point tables
  (`telemetry_points`, `signal_points`, `control_points`,
  `adjustment_points`). Duplicate channel names abort the sync.
- **aether-automation** — parses `config/automation/automation.yaml`, `instances.yaml`, and
  `rules/*.json` into the instance and rules tables, and validates any
  external product JSON files under `config/automation/products/`. There is no
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
validation is still mandatory.

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
  In a development checkout, `aether setup` plans and activates the exact
  safe template under `./data/config` and initializes `./data/aether.db` only
  after the returned plan ID is explicitly applied.

## Directory layout

The repository's `config.template/` directory is the canonical fail-safe
starting point. It contains no commissioned channel, device instance, or
enabled control rule. Domain examples are opt-in; the energy examples live
under `packs/energy/examples/config/`. Annotated:

```
config.template/
├── global.yaml                 # Shared settings: API bind
│                               # host, log level/rotation, rule scheduler
│                               # tick interval (rules.tick_ms, default 100)
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
    └── products/               # (optional, not in the template) Custom
                                # product JSON files; when present they
                                # override the built-in product library
                                # compiled into aether-model
```

Point-type shorthand: Aether uses T (telemetry), S (signal), C (control), and
A (adjustment) for the four point classes throughout its APIs and file
formats.

Products deserve a note: the product library (device-type templates) is
compiled into the `aether-model` crate, so a fresh install needs no product
files at all. If `automation.yaml` sets `products_path` and that directory
exists, automation loads the JSON files in it as overrides at startup
(`services/automation/src/bootstrap.rs`), and `aether sync` validates them and
reports per-file errors.

## Environment variables

From `.env.example`, used by Docker Compose and the services:

| Variable | Default | Purpose |
|----------|---------|---------|
| `AETHER_BASE_PATH` | `./data` | Base path for site configuration and databases; logs use `AETHER_LOG_PATH` |
| `HOST_UID` | `1000` | User id for container processes; must match the host user to avoid file-permission issues |
| `HOST_GID` | `1000` | Group id for container processes; pairs with `HOST_UID` |
| `DIALOUT_GID` | `20` | Dialout group id for serial-port access (Linux only) |
| `INFLUXDB_URL`, `INFLUXDB_ORG`, `INFLUXDB_BUCKET`, `INFLUXDB_TOKEN`, `INFLUXDB_PASSWORD` | unset | Optional InfluxDB history adapter only; unused by the default runtime |
| `AETHER_IO_URL` | `http://127.0.0.1:6001` | io base URL for the API gateway and the `aether` CLI |
| `AETHER_AUTOMATION_URL` | `http://127.0.0.1:6002` | automation base URL for the API gateway and the `aether` CLI |
| `AETHER_ACCESS_TOKEN` | unset | Signed Admin/Engineer access JWT required by CLI and MCP device-control commands; query commands do not require it on the local interface |
| `AETHER_UPLINK_CONTROL_TOKEN` | unset | Separate 32-byte-or-longer service credential used only for uplink-to-automation device commands; installers generate it and never print it |
| `AETHER_ALLOW_SIMULATION_WRITES` | `false` | Development-only opt-in for io T/S simulation writes into authoritative SHM; keep disabled in production |
| `AETHER_CONFIG_PATH` | unset | Overrides the install-context config directory for the `aether` CLI |
| `AETHER_DATA_PATH` | unset | Overrides the install-context data directory for the `aether` CLI |
| `AETHER_INSTALL_CONTEXT_PATH` | `/etc/aether/install.yaml` | Overrides the installed layout descriptor; CLI flags and the two path variables take precedence |
| `AETHER_BOOTSTRAP_ADMIN_PASSWORD` | unset | Required only while `users` is empty; installers generate a strong value in their mode-0600 environment file, and it should be removed after the first password change |
| `AETHER_ALLOW_PUBLIC_REGISTRATION` | `false` | Explicit opt-in for anonymous Viewer registration; Admin creation is never available through public registration |
| `RUST_LOG` | `info` | Log level for the Rust services; supports filter syntax such as `info,io=debug,automation=trace` |

## Related pages

- [Getting Started](../guides/getting-started.md) — first setup and startup walkthrough
- [Connect Devices](../guides/connect-devices.md) — channel and point configuration in practice
- [Writing Rules](../guides/writing-rules.md) — the rule JSON that lives under `automation/rules/`
- [HTTP API](http-api.md) — the runtime API the synced configuration feeds
- [System Architecture](../concepts/architecture.md) — where each service fits
