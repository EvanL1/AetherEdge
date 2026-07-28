---
title: CLI Reference
description: Current offline commissioning, application, Pack, SHM, and MCP commands
updated: 2026-07-28
---

# CLI Reference

`aether` is the offline commissioning and authenticated application client. It
covers configuration (`sync`, `status`, `init`, `export`), application groups,
Pack/runtime artifacts, and MCP. Host setup, process
supervision, logs, aggregate diagnostics, and TUI dashboards use installer or
standard operating-system tooling instead.

```
Usage: aether [OPTIONS] <COMMAND>
```

Use `aether <command> --help` for the same information at the terminal.

Online mutation commands are exposed only when they map to a governed
application capability with explicit authentication, confirmation, audit, and
revision semantics. Physical point topology, channel templates, uplink
configuration, certificates, and simulation input remain deployment concerns:
use commissioned configuration, host-managed certificate files, and the
external protocol simulator. Validate configuration with `aether sync
--dry-run`, stop the runtime owners, then apply with `aether sync --confirmed`
where applicable. Their former direct HTTP compatibility subcommands are
intentionally absent.

## Global flags

These flags are accepted by every command:

| Flag | Description |
|------|-------------|
| `-v, --verbose` | Enable verbose logging |
| `--no-color` | Disable colored output |
| `--json` | Output as JSON (suppresses banner and color; for scripts and AI agents) |
| `--host <HOST>` | Target host for remote operations (overrides localhost default) |
| `-c, --config-path <CONFIG_PATH>` | Configuration directory; overrides environment and installed layout |
| `--db-path <DB_PATH>` | Database directory; overrides environment and installed layout |
| `-h, --help` | Print help |
| `-V, --version` | Print version |

With `--json`, results are written to stdout as a `{success, ...}` envelope
(see [Exit codes and JSON mode](#exit-codes-and-json-mode)) and diagnostics go
to stderr. The `mcp` command is the exception: it speaks MCP JSON-RPC over
stdio, so `--json` does not change its output. The help output declares no
environment variables; host and path defaults come from the flags above.

## aether runtime-manifest

Verify the composition-provided runtime metadata before installing a Pack or
starting services. With no `--path`, the command reads
`<config-path>/runtime-manifest.json` and also requires its target OS and
architecture to match the current process. An explicit artifact path verifies
schema, Aether version, known capabilities/features, exact feature-derived
protocols, and checksum without binding a staged artifact to the verifier host.

```bash
aether runtime-manifest
aether --json runtime-manifest --path ./runtime-manifest.json
```

There is no full-distribution fallback: a missing, tampered, or incompatible
manifest is an error even when `packs: []`.

## aether packs

Build or install a Pack-only artifact. These are local filesystem operations;
`--host` is ignored.

```text
Usage: aether packs [OPTIONS] <COMMAND>

Commands:
  build    Build a data-only Pack bundle bound to one Kernel runtime manifest
  install  Verify, publish, and atomically activate a Pack bundle
```

```bash
aether packs build \
  --pack-root ./packs/example \
  --runtime-manifest ./runtime-manifest.json \
  --output ./example.bundle

aether packs install --artifact ./example.bundle
```

`build` validates `pack.yaml` against the supplied, checksummed runtime
manifest and refuses Kernel/build directories, source files, executables,
symlinks, and unbounded payloads. `install` requires the installed Kernel's
version, target, and full runtime-manifest digest to match, publishes to
`<data-path>/packs/<id>/<version>`, and atomically updates `global.yaml` only
after validating the complete candidate active Pack set. It does not start
services or commission devices.

## aether sync

Sync all configuration to SQLite database.

```
Usage: aether sync [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-n, --dry-run` | Validate only, don't write to database (dry run) |
| `-f, --force` | Replace sync-managed rows after successful validation; refused while any governed action route exists |
| `-d, --detailed` | Show detailed progress for each item |
| `--check` | Check database consistency (duplicates, references) |

```bash
aether sync --dry-run
```

## aether status

Show current configuration status.

```
Usage: aether status [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-d, --detailed` | Show detailed status |

```bash
aether status --detailed
```

## aether init

Initialize database schema (migration-only, safe upgrade). No command-specific
flags.

```
Usage: aether init [OPTIONS]
```

```bash
aether init
```

## aether export

Export configuration from SQLite to YAML/CSV.

```
Usage: aether export [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-O, --output <OUTPUT>` | Output directory (default: `config/`) |
| `-d, --detailed` | Show detailed export progress |

```bash
aether export -O /tmp/config-backup
```

## aether channels

Manage communication channels and protocols.

```
Usage: aether channels [OPTIONS] <COMMAND>
```

Subcommands: `list`, `status`, `control`, `adjust`, `reload`, `health`,
`create`, `update`, `delete`, `enable`, `disable`, `mappings`,
`unmapped-points`, `write`, `points`.

### channels list

List all configured communication channels.

```
Usage: aether channels list [OPTIONS]
```

```bash
aether channels list --json
```

### channels status

Get status of a specific channel.

```
Usage: aether channels status [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels status 1001
```

### channels health

Check communication service health.

```
Usage: aether channels health [OPTIONS]
```

```bash
aether channels health --json
```

### channels create

Create a new communication channel.

```
Usage: aether channels create [OPTIONS] --name <NAME> --protocol <PROTOCOL> --params <PARAMS> --confirmed
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Channel name (must be unique) |
| `--protocol <PROTOCOL>` | Protocol type (`modbus_tcp`, `modbus_rtu`, `di_do`, `can`) |
| `--params <PARAMS>` | Protocol parameters as JSON string (e.g. `'{"host":"192.168.1.10","port":502}'`) |
| `--description <DESCRIPTION>` | Channel description |
| `--enabled <ENABLED>` | Start channel immediately (default: false) [possible values: `true`, `false`] |
| `--id <ID>` | Override channel ID (auto-assigned if omitted) |
| `--confirmed` | Explicitly confirm this high-risk commissioning mutation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels create \
  --name pcs-main --protocol modbus_tcp \
  --params '{"host":"192.168.1.10","port":502}' --confirmed
```

### channels update

Update an existing channel's configuration.

```
Usage: aether channels update [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | New channel name |
| `--params <PARAMS>` | Updated protocol parameters as JSON string |
| `--description <DESCRIPTION>` | Updated description |
| `--expected-revision <EXPECTED_REVISION>` | Required desired-state compare-and-set guard from the latest channel read; must be at least 1 |
| `--confirmed` | Explicitly confirm this high-risk commissioning mutation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels update 1001 \
  --description "PCS main feed" --expected-revision 7 --confirmed
```

### channels delete

Delete a channel and its measurement-owned points, mappings, and routing.
The command fails closed while a physical action route targets the channel;
delete or migrate that route with the governed routing command first.

```
Usage: aether channels delete [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip the interactive prompt only; it never replaces `--confirmed` |
| `--expected-revision <EXPECTED_REVISION>` | Required desired-state compare-and-set guard from the latest channel read; must be at least 1 |
| `--confirmed` | Explicitly confirm this high-risk commissioning mutation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels delete 1001 \
  --force --expected-revision 7 --confirmed
```

### channels enable

Enable a channel.

```
Usage: aether channels enable [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--expected-revision <EXPECTED_REVISION>` | Required desired-state compare-and-set guard from the latest channel read; must be at least 1 |
| `--confirmed` | Explicitly confirm this high-risk lifecycle mutation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels enable 1001 \
  --expected-revision 7 --confirmed
```

### channels disable

Disable a channel.

```
Usage: aether channels disable [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--expected-revision <EXPECTED_REVISION>` | Required desired-state compare-and-set guard from the latest channel read; must be at least 1 |
| `--confirmed` | Explicitly confirm this high-risk lifecycle mutation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels disable 1001 \
  --expected-revision 7 --confirmed
```

The five channel commissioning and lifecycle mutations call the governed
`io.channel.manage` application boundary. Success may report a degraded
runtime projection after desired state has committed. Preserve `request_id`,
inspect `resulting_revision` and `reconciliation_required`, and do not
automatically retry the non-idempotent command. Update, delete, enable, and
disable require the revision returned by the latest channel read and fail
before HTTP when it is absent. Explicit runtime reconciliation remains the
separate `io.channel.reconcile` application capability exposed through the
HTTP/MCP boundary.

### channels mappings

Show a channel's point mappings.

```
Usage: aether channels mappings [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels mappings 1001
```

### channels unmapped-points

List points on a channel with no protocol address mapping.

```
Usage: aether channels unmapped-points [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels unmapped-points 1001
```

### channels points list

List points (grouped by T/S/C/A).

```
Usage: aether channels points list [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--type <TYPE>` | Filter by point type: `T`, `S`, `C`, or `A` |

```bash
aether channels points list 1001 --type T
```

### channels points mapping

Show the instance mapping for a single point.

```
Usage: aether channels points mapping [OPTIONS] <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

```bash
aether channels points mapping 1001 T 101
```

## aether models

Manage product templates and device instances. Two subcommand groups:
`products` and `instances`.

```
Usage: aether models [OPTIONS] <COMMAND>
```

### models products list

Show products selected by validated active Packs and site configuration.

```
Usage: aether models products list [OPTIONS]
```

```bash
aether models products list --json
```

### models products available

List product definitions in the `products/` directory.

```
Usage: aether models products available [OPTIONS]
```

```bash
aether models products available
```

### models products get

Show detailed information about a selected product.

```
Usage: aether models products get [OPTIONS] <NAME>
```

```bash
aether models products get battery
```

### models instances list

Show all device instances.

```
Usage: aether models instances list [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-p, --product <PRODUCT>` | Filter by product type |

```bash
aether models instances list --product battery
```

### models instances get

Show detailed information about an instance.

```
Usage: aether models instances get [OPTIONS] <NAME>
```

```bash
aether models instances get bat-01
```

### models instances data

Get realtime measurement and action point data from the authoritative SHM plane.

```
Usage: aether models instances data [OPTIONS] <INSTANCE_ID>
```

| Flag | Description |
|------|-------------|
| `-t, --point-type <POINT_TYPE>` | Point type filter (M for measurements, A for actions, both if not specified) |

```bash
aether models instances data 9 --point-type M
```

### models instances action

Submit a confirmed control action to the local command plane. A successful
response does not prove that the physical device executed it; read back the
corresponding measurement to verify the outcome.
If the returned `audit.status` is `incomplete`, retain `request_id` and
`command_id`; the action was already accepted and must not be retried.
Set `AETHER_ACCESS_TOKEN` to a current Admin or Engineer access token before
running this command; forged actor/role headers and local-port access do not
grant device-control permission.

```
Usage: aether models instances action [OPTIONS] --point-id <POINT_ID> --value <VALUE> <INSTANCE_ID>
```

| Flag | Description |
|------|-------------|
| `--point-id <POINT_ID>` | Numeric action point ID encoded as a string, e.g. `"1"` |
| `--value <VALUE>` | Value to write |
| `--confirmed` | Explicitly confirm this high-risk device command |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether models instances action 9 --point-id 1 --value 50 --confirmed
```

## aether rules

Manage and execute business rules.

```
Usage: aether rules [OPTIONS] <COMMAND>
```

### rules list

List all configured business rules.

```
Usage: aether rules list [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--enabled` | Show only enabled rules |

```bash
aether rules list --enabled
```

### rules get

Show detailed information about a rule.

```
Usage: aether rules get [OPTIONS] <RULE_ID>
```

```bash
aether rules get 3
```

### rules enable

Enable a business rule.

```
Usage: aether rules enable [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `--confirmed` | Explicitly confirm this high-risk rule-policy mutation |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether rules enable 3 --confirmed
```

### rules disable

Disable a business rule.

```
Usage: aether rules disable [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `--confirmed` | Explicitly confirm this high-risk rule-policy mutation |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether rules disable 3 --confirmed
```

### rules execute

Execute a rule (evaluate and execute if conditions met).
If the returned `audit.status` is `incomplete`, retain `request_id`; execution
already completed and must not be retried.

```
Usage: aether rules execute [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `--confirmed` | Explicitly confirm that the rule may dispatch real device commands |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules execute 3 --confirmed
```

### rules create

Create a new business rule.

```
Usage: aether rules create [OPTIONS] --name <NAME>
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Rule name |
| `--description <DESCRIPTION>` | Rule description |
| `--confirmed` | Explicitly confirm this high-risk rule-policy mutation |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules create --name night-charge --description "Charge during off-peak hours" --confirmed
```

### rules update

Update rule metadata and/or flow logic.

```
Usage: aether rules update [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | New rule name |
| `--description <DESCRIPTION>` | New description |
| `--enabled <ENABLED>` | Enable or disable the rule [possible values: `true`, `false`] |
| `--priority <PRIORITY>` | Rule priority (lower = higher priority) |
| `--cooldown-ms <COOLDOWN_MS>` | Cooldown between executions in milliseconds |
| `--flow-json <FLOW_JSON>` | Path to Vue Flow JSON file (use `-` for stdin) |
| `--confirmed` | Explicitly confirm this high-risk rule-policy mutation |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules update 3 --flow-json flow.json --confirmed
```

### rules delete

Delete a business rule.

```
Usage: aether rules delete [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip confirmation prompt |
| `--confirmed` | Required safety confirmation; `--force` does not replace it |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules delete 3 --force --confirmed
```

## aether routing

Manage channel-to-instance point routing.

```
Usage: aether routing [OPTIONS] <COMMAND>
```

### routing list

List routing configurations.

```
Usage: aether routing list [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-i, --instance <INSTANCE>` | Filter by instance ID |
| `--channel <CHANNEL>` | Filter by channel ID |

```bash
aether routing list --instance 9
```

### routing action

Governed single-route commands for physical C/A destinations. Every operation
requires `AETHER_ACCESS_TOKEN` and `--confirmed`; changing a route does not
execute a device command.

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether routing action upsert \
  9 1 --channel-id 1001 --channel-type c --channel-point-id 7 --confirmed

AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing action delete 9 1 --confirmed

AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing action enable 9 1 --confirmed

AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing action disable 9 1 --confirmed
```

`upsert` accepts `--disabled` to commission a route without activating it.
Measurement routing remains part of the explicitly confirmed offline
configuration import until it has the same governed online revision contract.

## aether alarms

### alarms list

List currently active alerts.

```
Usage: aether alarms list [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--channel <CHANNEL>` | Filter by channel ID |
| `--level <LEVEL>` | Filter by warning level (1=low, 2=medium, 3=high) |
| `--keyword <KEYWORD>` | Keyword search (rule name, channel, point) |
| `--page <PAGE>` | Page number, 1-based (default: 1) |
| `--size <SIZE>` | Page size (default: 50) |

```bash
aether alarms list --level 3
```

### alarms get

Get details of a specific active alert.

```
Usage: aether alarms get [OPTIONS] <ID>
```

```bash
aether alarms get 42
```

### alarms resolve

Manually clear one active alert indication. If the underlying condition still
holds, the monitor will create a new alert on a later evaluation.

```
Usage: aether alarms resolve [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms resolve 42 --confirmed
```

### alarms rules

List alarm rules.

```
Usage: aether alarms rules [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--channel <CHANNEL>` | Filter by channel ID |
| `--enabled` | Show only enabled rules |
| `--level <LEVEL>` | Filter by warning level (1=low, 2=medium, 3=high) |
| `--keyword <KEYWORD>` | Keyword search |
| `--page <PAGE>` | Page number, 1-based (default: 1) |
| `--size <SIZE>` | Page size (default: 50) |

```bash
aether alarms rules --enabled
```

### alarms rule-get

Get details of a specific alarm rule.

```
Usage: aether alarms rule-get [OPTIONS] <ID>
```

```bash
aether alarms rule-get 7
```

### alarms events

List historical alert events.

```
Usage: aether alarms events [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--rule <RULE>` | Filter by rule ID |
| `--event-type <EVENT_TYPE>` | Filter by event type: `trigger` or `recovery` |
| `--level <LEVEL>` | Filter by warning level (1=low, 2=medium, 3=high) |
| `--keyword <KEYWORD>` | Keyword search |
| `--page <PAGE>` | Page number, 1-based (default: 1) |
| `--size <SIZE>` | Page size (default: 50) |

```bash
aether alarms events --level 3 --event-type trigger
```

### alarms stats

Show alert count and rule statistics.

```
Usage: aether alarms stats [OPTIONS]
```

```bash
aether alarms stats --json
```

### alarms monitor

Show alarm monitor loop status.

```
Usage: aether alarms monitor [OPTIONS]
```

```bash
aether alarms monitor
```

### alarms rule-create

Create an alarm rule from a JSON file.

Alarm-rule creation, update, deletion, enablement, disablement, and manual alert
resolution are governed high-risk policy commands. Set `AETHER_ACCESS_TOKEN` to
a current Admin or Engineer access JWT and pass `--confirmed`; query commands
remain token-free on the local interface.

```
Usage: aether alarms rule-create [OPTIONS] --file <FILE> --confirmed
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to a JSON file matching alarm's `CreateRuleRequest` |
| `--confirmed` | Explicitly confirm the alarm-policy mutation |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-create --file alarm-rule.json --confirmed
```

### alarms rule-update

Update an alarm rule from a JSON file (only present fields change).

```
Usage: aether alarms rule-update [OPTIONS] --file <FILE> --confirmed <ID>
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to a JSON file matching alarm's `UpdateRuleRequest` |
| `--confirmed` | Explicitly confirm the alarm-policy mutation |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-update 7 --file alarm-rule-patch.json --confirmed
```

### alarms rule-delete

Delete an alarm rule.

```
Usage: aether alarms rule-delete [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-delete 7 --confirmed
```

### alarms rule-enable

Enable an alarm rule.

```
Usage: aether alarms rule-enable [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-enable 7 --confirmed
```

### alarms rule-disable

Disable an alarm rule.

```
Usage: aether alarms rule-disable [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-disable 7 --confirmed
```

## aether net

Manage MQTT connection, uplink config, and TLS certificates. Two subcommand
groups: `mqtt` and `cert`.

```
Usage: aether net [OPTIONS] <COMMAND>
```

### net mqtt status

Show MQTT connection status.

```
Usage: aether net mqtt status [OPTIONS]
```

```bash
aether net mqtt status --json
```

### net mqtt config

Show the current uplink configuration.

```
Usage: aether net mqtt config [OPTIONS]
```

```bash
aether net mqtt config
```

### net cert info

Show installed TLS certificate info.

```
Usage: aether net cert info [OPTIONS]
```

```bash
aether net cert info
```

## aether history

### history latest

Get the latest historical value for a point. Positional arguments:
`<SERIES_KEY>` (e.g. `inst:9:M` or `io:1001:T`) and `<POINT_ID>`.

```
Usage: aether history latest [OPTIONS] <SERIES_KEY> <POINT_ID>
```

```bash
aether history latest inst:9:M 101
```

### history query

Query historical data for a point.

```
Usage: aether history query [OPTIONS] <SERIES_KEY> <POINT_ID>
```

| Flag | Description |
|------|-------------|
| `--from <FROM>` | Start time (ISO 8601, e.g. `2026-05-12T00:00:00Z`, or relative like `-1h`) |
| `--to <TO>` | End time (ISO 8601, defaults to now) |
| `--page <PAGE>` | Page number, 1-based (default: 1) |
| `--size <SIZE>` | Page size, max rows per page (default: 100) |

```bash
aether history query inst:9:M 101 --from 2026-05-01T00:00:00Z
```

### history channels

List channels known to history.

```
Usage: aether history channels [OPTIONS]
```

```bash
aether history channels
```

### history metrics

Show historical storage metrics (row counts, data range, etc.).

```
Usage: aether history metrics [OPTIONS]
```

```bash
aether history metrics --json
```

### history health

Check history service health.

```
Usage: aether history health [OPTIONS]
```

```bash
aether history health
```

### history batch

Batch query historical data for multiple points in one request (max 20
series).

```
Usage: aether history batch [OPTIONS] --from <FROM>
```

| Flag | Description |
|------|-------------|
| `--series <KEY,POINT_ID>` | Series to query, format `series_key,point_id` (repeatable, max 20) |
| `--from <FROM>` | Start time (ISO 8601, e.g. `2026-05-01T00:00:00Z`) |
| `--to <TO>` | End time (ISO 8601, defaults to now) |
| `--limit <LIMIT>` | Max data points returned per series (default 1000, max 5000) |

```bash
aether history batch --series inst:9:M,101 --series inst:9:M,102 \
  --from 2026-05-01T00:00:00Z --limit 500
```

## aether mcp

Run an MCP server exposing `aether`'s capabilities as tools. The server speaks
MCP JSON-RPC over stdio; the global `--json` flag does not change its output.

```
Usage: aether mcp [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--allow-write` | Add the 22 governed write tools to the 22 always-registered read-only tools. It is only a registration gate; each invocation still requires `confirmed: true` |

```bash
aether mcp --allow-write
```

The 22 writes are channel CRUD/lifecycle (`channels_create`,
`channels_update`, `channels_delete`, `channels_enable`, `channels_disable`,
`channels_reconcile`);
`models_instances_action`, `rules_execute`; rule CRUD and
lifecycle (`rules_create`, `rules_update`, `rules_delete`, `rules_enable`,
`rules_disable`); alarm-rule CRUD and lifecycle (`alarms_rule_create`,
`alarms_rule_update`, `alarms_rule_delete`, `alarms_rule_enable`,
`alarms_rule_disable`); manual alert resolution (`alarms_resolve`); and
action-route governance (`routing_action_upsert`, `routing_action_delete`,
`routing_action_set_enabled`). The write-enabled catalog therefore has 45
tools in total.

The MCP bridge reads `AETHER_ACCESS_TOKEN`, sends it as an
`Authorization: Bearer` credential, and generates an `X-Request-ID` for each
governed HTTP request. Keep returned `request_id`/`command_id` values and do
not automatically retry writes after a timeout or an incomplete audit or
publication response; inspect state and audit records first. Channel mutation
success may contain a degraded runtime projection; use its `request_id`,
`resulting_revision`, and `reconciliation_required` rather than retrying.

See [AI Assistants](../guides/ai-assistants.md) for connecting MCP clients.

## Exit codes and JSON mode

Observed behavior of `aether` 0.4.0:

- **Exit 0** — the operation succeeded.
- **Exit 1** — the operation failed (for example, a target service is
  unreachable). In plain mode the error is printed as `Error: <message>`.
- **Exit 2** — command-line usage error (unknown subcommand or flag); clap
  prints the error and a usage hint to stderr.

With `--json`, results go to stdout as a single envelope and diagnostics go
to stderr:

```json
{ "success": true, "data": { "...": "..." } }
```

On failure the envelope carries the error message instead, and the process
exits with code 1:

```json
{ "success": false, "error": "error sending request for url (...): tcp connect error: Connection refused" }
```

`--json` also suppresses the banner and colored output, which makes it the
recommended mode for scripts and AI agents. The `mcp` command ignores it, as
noted above.

## Related pages

- [Getting Started](../guides/getting-started.md) — build, initialize, and
  start Aether
- [AI Assistants](../guides/ai-assistants.md) — drive the CLI and MCP server
  from an AI agent
- [System Architecture](../concepts/architecture.md) — the services these commands
  manage
