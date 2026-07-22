---
title: Configuration Reference
description: YAML configuration schema, the sync pipeline, and environment variables
updated: 2026-07-17
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
| `AETHER_API_URL` | `http://localhost:6005` | API gateway base URL for the `aether` CLI data plane and MCP; the only remote application boundary (ADR-0021) |
| `AETHER_IO_URL` | `http://127.0.0.1:6001` | Loopback io base URL used by the automation service's io calls; not read by the CLI |
| `AETHER_SHM_PATH` | platform-selected tmpfs path | Canonical authoritative point-state segment shared by io and read-only consumers |
| `AETHER_CHANNEL_HEALTH_SHM_PATH` | sibling `*-health` path | Separate authoritative channel-connectivity segment; normally derived from `AETHER_SHM_PATH` |
| `SHM_WRITER_STALE_AFTER_MS` | `30000` | Maximum writer-heartbeat age accepted by read-side SHM adapters |
| `SHM_IDENTITY_CHECK_INTERVAL_MS` | `250` | Fallback interval for checking whether the canonical SHM inode was replaced; generation fencing handles normal swaps immediately |
| `SHM_TOPOLOGY_REFRESH_INTERVAL_MS` | `1000` (minimum `100`) | Interval used by API, alarm, and automation to reload one SQLite topology snapshot and atomically publish a validated point/health/routing generation |
| `JWT_SECRET_KEY` | unset (required) | Shared 32-byte-or-longer access-JWT signing/verification secret for aether-api plus governed io, automation, and alarm operations; installers generate it and keep it outside configuration assets |
| `AETHER_ACCESS_TOKEN` | unset | Signed access JWT the `aether` CLI data plane and MCP attach to every gateway request (ADR-0021). A Viewer token covers queries; governed writes — channel commissioning/lifecycle, device commands, action-routing changes, automation/alarm policy, and MCP's 22 write tools — require an Admin or Engineer token |
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

### Experimental Home Assistant bridge settings

These settings are consumed only by a source-built `aether-io` binary compiled
with the `home-assistant` feature. They do not enable an installer-supported
production integration.

| Variable | Default | Purpose |
|---|---|---|
| `AETHER_HOME_ASSISTANT_ENABLED` | `false` | Explicitly enables the experimental read-only bridge |
| `AETHER_HOME_ASSISTANT_ORIGIN` | unset | Required HTTP(S) site origin without credentials, path, query, or fragment |
| `AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF` | unset | Required `env:VARIABLE_NAME` reference to token material held outside normal configuration |
| `AETHER_GATEWAY_ID` | unset | Required owning edge-gateway identity |
| `AETHER_HOME_ASSISTANT_INTEGRATION_ID` | `home-assistant` | Stable identity for this Home Assistant connection |
| `AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH` | unset | Required absolute path of the exclusively locked, restart-stable topology generation ledger |

`AETHER_HOME_ASSISTANT_ACCESS_TOKEN` is forbidden because it would place
credential material in ordinary configuration. See
[Connect Home Assistant](../guides/home-assistant.md) for the complete
source-build and storage contract.

The following settings activate the separate, default-off read-only CloudLink
publication path. They are accepted only by a binary built with
`home-assistant-cloudlink`.

| Variable | Default | Purpose |
|---|---|---|
| `AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED` | `false` | Explicitly enables publication; Home Assistant enablement alone never does |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL` | unset | Required experimental session origin. Production-mode composition permits only `gateway-signed`; `trusted-connector-broker-attestation` is restricted to explicit development harnesses |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID` | unset | Exact trusted Cloud challenge-verification key identity |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF` | unset | Required `env:VARIABLE_NAME` reference to a canonical unpadded-Base64url 32-byte Ed25519 public key |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID` | unset | Exact Gateway session-signing key identity |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF` | unset | Required `env:VARIABLE_NAME` reference to a canonical unpadded-Base64url 32-byte Ed25519 private seed |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH` | unset | Absolute, distinct path for the bounded process-exclusive challenge replay ledger |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR` | unset | Absolute directory containing the verified `runtime-manifest.json` |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION` | unset | Must equal `aether.cloudlink.integration.v1alpha1`, proving cloud-first rollout confirmation |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH` | unset | Absolute crash-recoverable topology journal path |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH` | unset | Distinct absolute crash-recoverable observation journal path |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_SPOOL_CAPACITY` | `4096` | Per-stream retained-record bound, from 1 through 65536 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST` | unset | TLS MQTT hostname or IP without URI syntax |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT` | unset | Required non-zero MQTT TLS port |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID` | unset | Stable bounded broker client identity |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX` | unset | Safe topic prefix; wildcard characters are rejected |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME` | unset | Required authenticated broker principal |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF` | unset | Required `env:VARIABLE_NAME` broker-password reference |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID` | unset | Non-secret CloudLink connector credential identity |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION` | unset | Required positive credential generation |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH` | unset | Absolute monotonic session-epoch checkpoint path |

`AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD` is forbidden. MQTT PUBACK does
not remove topology or observation records; only a strict CloudLink
application ACK does. Gateway-signed session establishment persists the exact
challenge request before publication, verifies the Cloud challenge over the
profile's canonical signing projection, persists the exact signed hello before
publication, and retries unchanged bytes within the fixed challenge deadline.
Each Gateway-signed heartbeat and durable uplink uses that same Gateway key
over the frozen 13-field RFC 8785 projection. Durable records preserve their
original send and expiry times, kind, stream identity, batch ID, digest, and
payload across restart. A new session changes only session-bound fields and
the resulting signature. Trusted-connector mode is test-only, relies on
external broker attestation, and omits payload authentication rather than
creating a placeholder signature.

The production-mode configuration gate is a deny-by-default safety policy, not
a production-readiness claim: the current unsigned `session-accepted` message
does not cryptographically bind the acceptance to the challenge and client
nonce. Heartbeat and durable application acknowledgements are also unsigned;
a heartbeat acknowledgement carrying `message_authentication` is rejected.
These missing Cloud-to-Edge signing projections keep the handshake
experimental.

The file-backed challenge ledger is currently supported only on Unix. Its
direct parent cannot be group- or other-writable; a newly created parent uses
mode 0700, ledger and lock files use mode 0600, and symbolic-link opens are
rejected. Multiply linked ledger or lock files are also rejected. Completed
records retain only replay identity, expiry, and a digest;
raw request, challenge, and hello transcripts are removed atomically. This is
replay state, not a production key store. The current environment references
are injection points for supervisor-managed secrets; managed enrollment,
hardware-backed private keys, and rotation remain separate lifecycle work.

CloudLink preparation, authentication, or broker failure disables only the
optional cloud extension. The commissioned local Home Assistant snapshot and
state synchronization path remains active; invalid top-level Home Assistant
configuration is reported without terminating the core `aether-io` service.

The following settings activate the experimental governed power-control slice.
The binary must include `home-assistant-integration-control`; all three Home
Assistant, CloudLink, and control enable switches must be explicitly true, and
the Runtime Manifest must declare both Integration protocol tokens.

| Variable | Default | Purpose |
|---|---|---|
| `AETHER_HOME_ASSISTANT_CONTROL_ENABLED` | `false` | Explicitly enables governed control; it never implicitly enables Home Assistant or CloudLink |
| `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION` | unset | Must equal `aether.cloudlink.integration-control.v1alpha1` |
| `AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH` | unset | Absolute process-exclusive persistent job and receipt ledger path |
| `AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH` | unset | Absolute path to the closed deny-by-default local authorization policy |
| `AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH` | unset | Distinct absolute path to the append-only local audit journal |
| `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID` | unset | Exact trusted cloud Ed25519 verification-key identity |
| `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF` | unset | Required `env:VARIABLE_NAME` reference to a canonical unpadded-Base64url 32-byte Ed25519 public key |
| `AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID` | unset | Deprecated alias. Omit in Gateway-signed mode, or set together with the next alias to the exact CloudLink Gateway key identity. An explicit trusted-connector test harness may use it for its independent legacy receipt signer |
| `AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF` | unset | Deprecated alias. Omit in Gateway-signed mode, or set together with the preceding alias to the exact CloudLink Gateway key reference. Test-only trusted-connector use still requires a canonical private seed |
| `AETHER_HOME_ASSISTANT_CONTROL_PROVIDER_TIMEOUT_MS` | `5000` | Provider-call deadline from 1 through 30000 milliseconds |

Ledger, policy, and audit paths must be distinct. On Unix, ledger, audit, and
lock files must have no group/other permission bits; new sensitive files use
mode 0600. Symbolic-link files and direct parent directories are rejected.
MQTT PUBACK never removes a control receipt. See
[Connect Home Assistant](../guides/home-assistant.md#experimental-governed-power-control)
for the policy schema and execution boundary. Production receipt uplinks use
the active CloudLink Gateway session signer; incomplete or mismatched
deprecated aliases fail closed.

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
per-gateway topics are fixed by the experimental CloudLink profile; MQTT 5 remains optional and cannot be
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
