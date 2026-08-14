---
title: Configuration Reference
description: YAML configuration schema, protocol channel authority, the sync pipeline, and environment variables
updated: 2026-08-14
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
  [Control Strategies as Rules](../../packs/energy/knowledge/control-strategies.md)).

Before writing, `aether sync` validates all three domains. It then applies
global, IO, and automation configuration in one SQLite transaction, so an
error in a later domain rolls back all earlier changes. By default rows with
no corresponding config file (for example rules created through the HTTP API)
are preserved. With `--force`, managed tables are fully replaced, but
validation is still mandatory. Action (`A`) routing is deliberately outside
this configuration importer: it selects the physical target of future device
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
│   │                           # (modbus_tcp, bacnet_ip, iec101, cjt188,
│   │                           # gb32960, jt808, ...), enabled flag, protocol
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
    ├── instances.yaml          # Empty instance array until commissioning
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
formats. Point CSV files use decimal numeric text for `scale`/`offset` and the
exact lowercase values `true` or `false` for `reverse`; empty numeric cells,
`yes`/`no`, and `1`/`0` boolean spellings are rejected. JSON/YAML point objects
use native numbers and booleans rather than quoted scalar strings.

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
individual service. The closed document records the Aether release, target,
included services, exact `aether-io` protocol features, derived adapters, and
application capabilities under a canonical SHA-256 checksum. Automation and
MCP reject missing, tampered, Aether-release-mismatched, target-mismatched, unknown,
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
| `AETHER_API_URL` | `http://localhost:6005` | API gateway base URL for the `aether` CLI data plane and MCP; the only remote application boundary |
| `AETHER_IO_URL` | `http://127.0.0.1:6001` | Loopback io base URL used by the automation service's io calls; not read by the CLI |
| `AETHER_SHM_PATH` | platform-selected tmpfs path | Canonical authoritative point-state segment shared by io and read-only consumers |
| `AETHER_CHANNEL_HEALTH_SHM_PATH` | sibling `*-health` path | Separate authoritative channel-connectivity segment; normally derived from `AETHER_SHM_PATH` |
| `SHM_WRITER_STALE_AFTER_MS` | `30000` | Maximum writer-heartbeat age accepted by read-side SHM adapters |
| `SHM_IDENTITY_CHECK_INTERVAL_MS` | `250` | Fallback interval for checking whether the canonical SHM inode was replaced; generation fencing handles normal swaps immediately |
| `SHM_TOPOLOGY_REFRESH_INTERVAL_MS` | `1000` (minimum `100`) | Interval used by API, alarm, and automation to reload one SQLite topology snapshot and atomically publish a validated point/health/routing generation |
| `JWT_SECRET_KEY` | unset (required) | Shared 32-byte-or-longer access-JWT signing/verification secret for aether-api plus governed io, automation, and alarm operations; installers generate it and keep it outside configuration assets |
| `AETHER_ACCESS_TOKEN` | unset | Signed access JWT the `aether` CLI data plane and MCP attach to every gateway request. A Viewer token covers queries; governed writes — channel commissioning/lifecycle, device commands, action-routing changes, automation/alarm policy, and MCP's 22 write tools — require an Admin or Engineer token |
| `AETHER_ALARM_BROADCAST_TOKEN` | unset | Separate 32-byte-or-longer service credential shared only by alarm, API, and uplink for authenticated internal alarm delivery; it must differ from the JWT credential |
| `AETHER_CONFIG_PATH` | unset (required by aether-api configuration endpoints) | Exact shared configuration directory. Compose and bare-metal installers set it; API configuration checks/exports fail closed when it is absent or empty. CLI path resolution may still set it through deployment context or `--config-path` |
| `AETHER_DATA_PATH` | unset | Overrides the install-context data directory for the `aether` CLI |
| `AETHER_INSTALL_CONTEXT_PATH` | `/etc/aether/install.yaml` | Overrides the installed layout descriptor; CLI flags and the two path variables take precedence |
| `AETHER_BOOTSTRAP_ADMIN_PASSWORD` | unset | Required only while `users` is empty; installers generate a strong value in their mode-0600 environment file, and it should be removed after the first password change |
| `AETHER_ALLOW_PUBLIC_REGISTRATION` | `false` | Explicit opt-in for anonymous Viewer registration; Admin creation is never available through public registration |
| `AETHER_DATA_PROCESSING_ENABLED` | `false` | Explicitly enables the opt-in Data Processing application and HTTP routes; startup fails closed if enabled configuration is invalid |
| `AETHER_DATA_PROCESSING_CONFIG` | `/app/data/config/data-processing/runtime.yaml` | Strict runtime YAML containing commissioned task, binding, history, covariate, processor, and audit composition; downstream compositions provide processor-specific credential variables named by this file |
| `RUST_LOG` | `info` | Log level for the Rust services; supports filter syntax such as `info,io=debug,automation=trace` |

### Service bind addresses

Each service reads its own listen port, and which variable name it reads is not
uniform — three services use `SERVICE_PORT` and three use `API_PORT`. Compose
sets these explicitly; a source or bare-metal deployment that wants anything
other than the defaults must set them per process.

| Variable | Service | Default | Purpose |
|----------|---------|---------|---------|
| `SERVICE_PORT` | io | `6001` | Loopback listen port |
| `SERVICE_PORT` | automation | `6002` | Loopback listen port |
| `API_PORT` | history | `6004` | Loopback listen port |
| `API_PORT` | api | `6005` | The one remote application boundary |
| `API_PORT` | uplink | `6006` | Loopback listen port |
| `SERVICE_PORT` | alarm | `6007` | Loopback listen port |
| `API_HOST` | automation, history, api, uplink, alarm | internal services: `127.0.0.1`; api: `0.0.0.0` | Bind address; only the API gateway may leave loopback |

`aether-io` takes an explicit full listener address through `--bind-address`;
otherwise it combines its loaded loopback host with `SERVICE_PORT`.

### Gateway upstream addresses

`aether-api` resolves each internal service through its own variable. These are
distinct from `AETHER_IO_URL` and friends above, which other services and the
CLI use for their own outbound calls — setting those does not move the gateway.
A wrong or unset value here fails silently: the gateway falls back to the
default port and answers with another instance's data.

| Variable | Default |
|----------|---------|
| `AETHER_IO_SERVICE_URL` | `http://127.0.0.1:6001` |
| `AETHER_AUTOMATION_SERVICE_URL` | `http://127.0.0.1:6002` |
| `AETHER_HISTORY_SERVICE_URL` | `http://127.0.0.1:6004` |
| `AETHER_UPLINK_SERVICE_URL` | `http://127.0.0.1:6006` |
| `AETHER_ALARM_SERVICE_URL` | `http://127.0.0.1:6007` |
| `AETHER_SERVICE_REQUEST_TIMEOUT_SECS` | `60` |

The alarm service makes its own outbound call rather than going through the
gateway, and reads `AETHER_UPLINK_URL` (default `http://localhost:6006`) for it.

### Storage and IPC paths

| Variable | Default | Purpose |
|----------|---------|---------|
| `AETHER_DB_PATH` | `/app/data/aether.db` | Shared SQLite configuration database; io also stores its bounded durable command ledger here |
| `AETHER_HISTORY_DB_PATH` | `aether-history.db` beside `AETHER_DB_PATH` | Embedded historian database |
| `AETHER_CLOUDLINK_SPOOL_PATH` | `/app/data/cloudlink.spool` | Sole durable CloudLink business spool |
| `AETHER_CLOUDLINK_SPOOL_CAPACITY` | `1024` | Maximum retained CloudLink records; lossless alarm admission fails closed at the limit |
| `AETHER_CLOUDLINK_RECEIPT_CAPACITY` | `100000` | Maximum protected identities for acknowledged lossless alarm transitions. Entries are never silently evicted; new alarm admission fails closed when acknowledged receipts plus pending reservations reach the limit |
| `AETHER_CLOUDLINK_SPOOL_MAX_LIVE_BYTES` | `268435456` | Maximum logical bytes retained by records, protected receipts, and data-loss state; accepted range is 65536–17179869184 |
| `AETHER_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES` | `536870912` | Maximum physical journal bytes before admission fails closed; accepted maximum is 34359738368 and this value must exceed the live-byte limit by at least 65536 bytes |
| `AETHER_GATEWAY_IDENTITY_DIR` | `/app/data/uplink/identity` | Claimed Gateway identity and one-time challenge ledger root |
| `AETHER_LOG_DIR` | `/app/logs` | Log directory |
| `AETHER_M2C_SOCKET` | `/tmp/aether-m2c.sock` | Single 104-byte little-endian command socket with durable admission acknowledgements |
| `AETHER_AUTOMATION_POINT_WATCH_SOCKET` | derived from the SHM path | Point-change notification socket for automation |
| `AETHER_API_POINT_WATCH_SOCKET` | derived from the SHM path | Point-change notification socket for the gateway |
| `AETHER_ALARM_POINT_WATCH_SOCKET` | derived from the SHM path | Point-change notification socket for alarm |
| `SHM_SNAPSHOT_PATH` | `data/shm-snapshot.bin` | Periodic point-state snapshot used to restore after restart |
| `SHM_SNAPSHOT_INTERVAL` | `300` | Snapshot period in seconds |
| `SHM_RESTORE_ON_START` | `true` | Set to `false` to start with empty point state instead of restoring the snapshot |
| `CERT_DIR` | `/app/config/cert` | Certificate directory |
| `NETWORK_CONFIG_DIR` | `/etc/systemd/network` | Host network unit directory read by the gateway's network endpoints |

Running two instances on one host means giving the second one its own value for
every path above as well as its own ports — the defaults are machine-global.
Set the command socket path when overriding the derived layout. Both io and
automation must resolve the same path. The command ledger retains at most
100,000 command identities by default and keeps terminal
identities for 24 hours after expiry; these are fixed runtime safety bounds,
not environment-variable tuning knobs in the current composition.

The CloudLink receipt bound is different: only lossless alarm transitions use
it, each pending alarm reserves a slot before HTTP success, and acknowledged
identities have no age-based eviction. Manifest, telemetry, Integration, and
data-loss ACKs do not consume it. Monitor `acknowledged_receipts`,
`pending_receipt_reservations`, and `protected_receipt_slots` in Uplink health
and size both the receipt and byte bounds for the Gateway's commissioned lifetime. Health also reports
`ordinary_pending_records`, `system_pending_records`, `current_live_bytes`,
`ordinary_max_live_bytes`, `max_live_bytes`, `journal_bytes`,
`max_journal_bytes`, and cumulative `quota_rejections`. Never delete a live
ledger for space reclamation; archive it only while decommissioning the Gateway
identity after the Alarm outbox is empty.

### Timing and session lifetimes

| Variable | Default | Purpose |
|----------|---------|---------|
| `DATA_FETCH_INTERVAL` | `5` in alarm, `1` in api | Poll period in seconds. The two services read the same name with different defaults and different meanings |
| `ALARM_NOTIFICATION_OUTBOX_POLL_INTERVAL_MS` | `250` | Durable alarm-notification dispatcher poll interval |
| `ALARM_NOTIFICATION_RETRY_INITIAL_MS` | `500` | Initial delay before retrying a failed notification destination |
| `ALARM_NOTIFICATION_RETRY_MAX_MS` | `30000` | Maximum exponential retry delay for a failed notification destination |
| `ALARM_NOTIFICATION_SHUTDOWN_DRAIN_MS` | `2000` | Maximum best-effort alarm outbox drain time during shutdown |
| `AETHER_IO_RECONCILIATION_INTERVAL_MS` | `2000` | Interval at which io reconciles desired against applied channel state |
| `POINT_WATCH_DEBOUNCE_MS` | `25` | Minimum gap between point-change notifications |
| `ACCESS_TOKEN_EXPIRE_MINUTES` | `30` | Access JWT lifetime |
| `REFRESH_TOKEN_EXPIRE_DAYS` | `7` | Refresh token lifetime |

### CloudLink MQTT settings

CloudLink is the only uplink protocol compiled into the production service. It
owns one MQTT topic tree, one durable spool, and one schema set. It is disabled by default so the
six-process edge distribution runs without an external broker. Setting
`AETHER_CLOUDLINK_ENABLED=true` fails closed unless the claimed Gateway
identity, Cloud verification key, broker endpoint, and credential binding are
all present. There is no generic MQTT fallback or runtime configuration API.

| Variable | Default | Purpose |
|---|---|---|
| `AETHER_CLOUDLINK_ENABLED` | `false` | Set exactly `true` to enable the CloudLink session |
| `AETHER_CLOUDLINK_BROKER_HOST` | unset | Required broker hostname/IP when enabled |
| `AETHER_CLOUDLINK_BROKER_PORT` | `8883` | TLS broker port |
| `AETHER_CLOUDLINK_TOPIC_PREFIX` | `aether` | Closed CloudLink topic-tree prefix |
| `AETHER_CLOUDLINK_KEEP_ALIVE_SECS` | `30` | MQTT keepalive, strictly bounded to 5–3600 seconds |
| `AETHER_CLOUDLINK_RECONNECT_DELAY_SECS` | `5` | Delay between broker reconnect attempts, bounded to 1–3600 seconds |
| `AETHER_CLOUDLINK_REQUEST_CAPACITY` | `64` | Shared bounded delivery-window and MQTT request-channel capacity, bounded to 1–4096 |
| `AETHER_CLOUDLINK_CHALLENGE_LEDGER_CAPACITY` | `64` | Restart-safe challenge replay entries, bounded to 1–256 |
| `AETHER_CLOUDLINK_CHALLENGE_REQUEST_TTL_MS` | `60000` | Locally generated challenge-request lifetime, bounded to 1000–600000 ms |
| `AETHER_CLOUDLINK_TELEMETRY_INTERVAL_SECS` | `30` | Telemetry sampling cadence, bounded to 1–86400 seconds; durable delivery also has an independent one-second safety pump |
| `AETHER_CLOUDLINK_BROKER_USERNAME` | unset | Optional broker username |
| `AETHER_CLOUDLINK_BROKER_PASSWORD` | unset | Optional write-only broker password; never printed or serialized |
| `AETHER_CLOUDLINK_BROKER_CA` | unset | Optional custom PEM CA path; system roots are used when absent |
| `AETHER_CLOUDLINK_BROKER_CLIENT_CERT` | unset | Optional mTLS client certificate; must be paired with CA and key |
| `AETHER_CLOUDLINK_BROKER_CLIENT_KEY` | unset | Optional mTLS PKCS#8 private key; must be paired with CA and certificate |
| `AETHER_CLOUDLINK_CLOUD_KEY_ID` | unset | Required Cloud Ed25519 verification-key identity |
| `AETHER_CLOUDLINK_CLOUD_VERIFYING_KEY` | unset | Required unpadded-base64url Cloud Ed25519 public key |
| `AETHER_CLOUDLINK_CREDENTIAL_ID` | unset | Required commissioned Cloud credential identity |
| `AETHER_CLOUDLINK_CREDENTIAL_GENERATION` | unset | Required positive credential generation |
| `AETHER_RUNTIME_MANIFEST_PATH` | `/app/config/runtime-manifest.json` | Closed local Runtime Manifest reported after session establishment |

Production always uses TLS. MQTT v3.1.1, QoS 1, non-retained messages, and
exact per-gateway topics are fixed by CloudLink; MQTT 5 is not required for
correctness.

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
- [Protocol Adapter Reference](protocol-adapters.md) — exact feature gates, runtime IDs, parameters, and mappings
- [Writing Rules](../guides/writing-rules.md) — the rule JSON that lives under `automation/rules/`
- [HTTP API](http-api.md) — the runtime API the synced configuration feeds
- [System Architecture](../concepts/architecture.md) — where each service fits
