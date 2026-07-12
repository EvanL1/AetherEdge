---
title: CLI Reference
description: Every aether command - services, sync, doctor, channels, rules, and more
updated: 2026-07-12
---

# CLI Reference

`aether` (version 0.5.0) is the unified management tool for Aether. It covers
configuration management (`setup`, `sync`, `status`, `init`, `export`) and service
operations (`channels`, `models`, `rules`, `services`, `logs`, and more).
Every section below is generated from the binary's own `--help` output.

```
Usage: aether [OPTIONS] <COMMAND>
```

Use `aether <command> --help` for the same information at the terminal.

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

## aether setup

Plan or apply the conservative first-run configuration. With no subcommand,
`setup` is identical to `setup plan` and is persistently read-only.

```
Usage: aether setup [COMMAND]

Commands:
  plan   Recompute and print the read-only setup plan
  apply  Apply an unchanged safe plan after explicit confirmation
```

```bash
# Human-readable, read-only plan
aether setup

# Structured plan for an AI agent or script
aether --json setup

# The only persistent setup operation
aether setup apply --plan-id <PLAN_ID>
```

The SHA-256 plan ID binds the target paths, safe-file fingerprints, detected
extra files, and SQLite state. Apply recomputes it before writing and rejects
a stale ID. Site states are:

| State | Meaning | Apply behavior |
|-------|---------|----------------|
| `fresh` | No configuration or local database exists | Creates only the four safe empty files and local SQLite state |
| `safe_partial` | An exact subset of the safe files/database exists | Preserves existing files and creates only missing safe state |
| `safe_ready` | Safe empty configuration is already synchronized | Successful no-op |
| `existing` | A complete custom or commissioned site was detected | Refused; zero writes |
| `blocked` | A partial custom, unreadable, or unrecognized site was detected | Refused; zero writes and explicit blockers |

Even a successful apply reports `ready: false`: it never starts services,
enables devices or rules, performs physical control, or installs a domain
pack. Continue with `aether services start` and `aether doctor`; device
commissioning is a separate audited operation.

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

### channels reload

Reconcile every channel runtime from authoritative desired state. The command
name is retained for compatibility, but it calls the canonical governed
`POST /api/channels/reconcile` endpoint rather than the legacy reload route.

```
Usage: aether channels reload [OPTIONS] --confirmed
```

| Flag | Description |
|------|-------------|
| `--confirmed` | Explicitly confirm this high-risk runtime reconciliation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels reload --confirmed
```

The receipt reports the sanitized desired-state observation and runtime
projection for each channel, plus `degraded_count`,
`reconciliation_required`, and terminal `completion_audit`. Preserve its UUID
`request_id`; this operation can reconnect protocol sessions and is
non-idempotent, so never retry it automatically, including after an incomplete
terminal audit.

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
| `--protocol <PROTOCOL>` | Protocol type (`modbus_tcp`, `modbus_rtu`, `virtual`, `di_do`, `can`) |
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
| `--expected-revision <EXPECTED_REVISION>` | Optional desired-state compare-and-set guard; must be at least 1 |
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
| `--expected-revision <EXPECTED_REVISION>` | Optional desired-state compare-and-set guard; must be at least 1 |
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
| `--expected-revision <EXPECTED_REVISION>` | Optional desired-state compare-and-set guard; must be at least 1 |
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
| `--expected-revision <EXPECTED_REVISION>` | Optional desired-state compare-and-set guard; must be at least 1 |
| `--confirmed` | Explicitly confirm this high-risk lifecycle mutation; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels disable 1001 --confirmed
```

The five channel commissioning and lifecycle mutations call the governed
`io.channel.manage` application boundary. Success may report a degraded
runtime projection after desired state has committed. Preserve `request_id`,
inspect `resulting_revision` and `reconciliation_required`, and do not
automatically retry the non-idempotent command. `channels reload` is the sixth
governed channel command and maps separately to `io.channel.reconcile` while
requiring the same `io.channel.manage` permission, explicit confirmation,
Bearer token, UUID request ID, and audit policy.

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

### channels write

Inject a simulated telemetry or signal value into the acquisition SHM plane.
This command accepts only T/S points; real C/A device commands must use
`aether models instances action` so routing, confirmation, and audit cannot be
bypassed.

```
Usage: aether channels write [OPTIONS] --type <POINT_TYPE> --id <ID> --value <VALUE> <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--type <POINT_TYPE>` | Simulation point type: `T` \| `S` |
| `--id <ID>` | Point ID (numeric or semantic) |
| `--value <VALUE>` | Value to write |

```bash
aether channels write 1001 --type T --id 3 --value 42.5
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

### channels points add

Add a point to a channel. Positional arguments: `<CHANNEL_ID>` `<POINT_TYPE>`
(T telemetry, S signal, C control, A adjustment) `<POINT_ID>`.

```
Usage: aether channels points add [OPTIONS] --name <NAME> <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Signal name |
| `--unit <UNIT>` | Unit (e.g., V, A, kW) |
| `--scale <SCALE>` | Scale factor |
| `--description <DESCRIPTION>` | Description |
| `--data-type <DATA_TYPE>` | Data type (default: float32 for T/A, bool for S/C) |

```bash
aether channels points add 1001 T 101 --name voltage --unit V --scale 0.1
```

### channels points update

Update a point's attributes.

```
Usage: aether channels points update [OPTIONS] <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Signal name |
| `--unit <UNIT>` | Unit |
| `--scale <SCALE>` | Scale factor |
| `--description <DESCRIPTION>` | Description |

```bash
aether channels points update 1001 T 101 --scale 0.01
```

### channels points remove

Remove a point from a channel.

```
Usage: aether channels points remove [OPTIONS] <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Force deletion without confirmation |

```bash
aether channels points remove 1001 T 101 --force
```

### channels points batch

Batch create/update/delete points from a JSON file.

```
Usage: aether channels points batch [OPTIONS] --file <FILE> <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to a JSON file: `{"create":[],"update":[],"delete":[]}` |

```bash
aether channels points batch 1001 --file points.json
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

### models instances create

Create a new device instance from a product template. Positional arguments:
`<PRODUCT>` `<NAME>`.

```
Usage: aether models instances create [OPTIONS] <PRODUCT> <NAME>
```

| Flag | Description |
|------|-------------|
| `-p, --props <PROPS>` | Properties in `key=value` format |

```bash
aether models instances create battery bat-01 --props capacity=100
```

### models instances get

Show detailed information about an instance.

```
Usage: aether models instances get [OPTIONS] <NAME>
```

```bash
aether models instances get bat-01
```

### models instances update

Update instance properties.

```
Usage: aether models instances update [OPTIONS] <NAME>
```

| Flag | Description |
|------|-------------|
| `-p, --props <PROPS>` | Properties to update in `key=value` format |

```bash
aether models instances update bat-01 --props capacity=120
```

### models instances delete

Delete a device instance.

The command fails closed while the instance owns a physical action route;
delete or migrate that route with the governed routing command first.

```
Usage: aether models instances delete [OPTIONS] <NAME>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Force deletion without confirmation |

```bash
aether models instances delete bat-01 --force
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
The older `routing create --point-type a ... --confirmed` form remains a
compatibility alias for enabled upsert.

### routing create

Create a single routing entry for an instance.

```
Usage: aether routing create [OPTIONS] --point-type <POINT_TYPE> --point-id <POINT_ID> --channel-id <CHANNEL_ID> --four-remote <FOUR_REMOTE> --channel-point-id <CHANNEL_POINT_ID> <INSTANCE_ID>
```

| Flag | Description |
|------|-------------|
| `-t, --point-type <POINT_TYPE>` | Point type: `m` (measurement) or `a` (action) |
| `-p, --point-id <POINT_ID>` | Instance point ID |
| `--channel-id <CHANNEL_ID>` | Channel ID |
| `-r, --four-remote <FOUR_REMOTE>` | Four-remote type: `t` (telemetry), `s` (signal), `c` (control), `a` (adjustment) |
| `-P, --channel-point-id <CHANNEL_POINT_ID>` | Channel point ID |
| `--confirmed` | Explicitly confirm an action route that changes a physical command target; requires `AETHER_ACCESS_TOKEN` |

```bash
aether routing create 9 --point-type m --point-id 101 \
  --channel-id 1001 --four-remote t --channel-point-id 101

AETHER_ACCESS_TOKEN='<signed access JWT>' aether routing create 9 \
  --point-type a --point-id 1 --channel-id 1001 \
  --four-remote c --channel-point-id 7 --confirmed
```

### routing batch

Batch upsert routing from JSON file or stdin.

The compatibility batch accepts measurement entries only. Action entries fail
closed until a governed batch application command is available; create action
routes one at a time with `routing create ... --point-type a --confirmed`.

```
Usage: aether routing batch [OPTIONS] --file <FILE> <INSTANCE_ID>
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to JSON file with routing entries (use `-` for stdin) |

```bash
aether routing batch 9 --file routing.json
```

### routing delete-instance

Delete all routing for an instance. Takes the instance name, not the numeric
ID.

```
Usage: aether routing delete-instance [OPTIONS] <INSTANCE_NAME>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip confirmation |
| `--confirmed` | Confirm deletion of physical action routes; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing delete-instance bat-01 --force --confirmed
```

### routing delete-channel

Delete all routing for a channel.

```
Usage: aether routing delete-channel [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip confirmation |
| `--confirmed` | Confirm deletion of physical action routes; requires `AETHER_ACCESS_TOKEN` |

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing delete-channel 1001 --force --confirmed
```

## aether services

Start, stop, and manage Aether services. All service arguments are optional;
omitting them targets all services.

```
Usage: aether services [OPTIONS] <COMMAND>
```

### services start

Start one or more Aether services.

```
Usage: aether services start [OPTIONS] [SERVICES]...
```

```bash
aether services start aether-io aether-automation
```

### services stop

Stop one or more Aether services.

```
Usage: aether services stop [OPTIONS] [SERVICES]...
```

```bash
aether services stop
```

### services restart

Restart one or more Aether services.

```
Usage: aether services restart [OPTIONS] [SERVICES]...
```

```bash
aether services restart aether-io
```

### services status

Display status of Aether services.

```
Usage: aether services status [OPTIONS] [SERVICES]...
```

```bash
aether services status --json
```

### services logs

View logs for Aether services.

```
Usage: aether services logs [OPTIONS] <SERVICE>
```

| Flag | Description |
|------|-------------|
| `-f, --follow` | Follow log output |
| `-n, --tail <TAIL>` | Number of lines to show from the end (default: 100) |

```bash
aether services logs aether-io --follow --tail 200
```

### services reload

Reload configurations for services.

```
Usage: aether services reload [OPTIONS] [SERVICES]...
```

```bash
aether services reload aether-automation
```

### services build

Build Docker images for services.

```
Usage: aether services build [OPTIONS] [SERVICES]...
```

```bash
aether services build aether-io
```

### services pull

Pull latest Docker images.

```
Usage: aether services pull [OPTIONS]
```

```bash
aether services pull
```

### services clean

Clean up Docker volumes and networks.

```
Usage: aether services clean [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--volumes` | Also remove volumes (long form only; `-v` is the global verbose flag) |

```bash
aether services clean --volumes
```

### services refresh

Force recreate containers with latest images.

```
Usage: aether services refresh [OPTIONS] [SERVICES]...
```

| Flag | Description |
|------|-------------|
| `-p, --pull` | Also pull latest images before recreating |
| `-s, --smart` | Use smart mode (only recreate if an image changed; stateful extensions remain explicit) |

```bash
aether services refresh --pull --smart
```

## aether logs

Log level control and log file viewer.

```
Usage: aether logs [OPTIONS] <COMMAND>
```

### logs level

Set log level for a service. Positional arguments: `<SERVICE>` (io,
automation, all) and `<LEVEL>` (trace, debug, info, warn, error) or a full filter
spec such as `"info,io=debug"`.

```
Usage: aether logs level [OPTIONS] <SERVICE> <LEVEL>
```

```bash
aether logs level all debug
```

### logs get

Get current log level for a service (aether-io, aether-automation, all).

```
Usage: aether logs get [OPTIONS] <SERVICE>
```

```bash
aether logs get aether-io
```

### logs list

List log files on disk (default: today). The service filter is optional.

```
Usage: aether logs list [OPTIONS] [SERVICE]
```

| Flag | Description |
|------|-------------|
| `-d, --date <DATE>` | Date in `YYYYMMDD` format (default: today) |

```bash
aether logs list aether-io --date 20260709
```

### logs view

View recent lines from a service log file (aether-io, aether-automation,
aether-history, aether-uplink,
alarm, api).

```
Usage: aether logs view [OPTIONS] <SERVICE>
```

| Flag | Description |
|------|-------------|
| `-n, --lines <LINES>` | Number of lines from end (default: 50) |
| `--api` | Show API access log instead of main log |
| `-g, --grep <GREP>` | Filter lines containing this pattern (case-insensitive) |

```bash
aether logs view aether-io -n 100 --grep ERROR
```

### logs tail

Tail a service log file in real-time.

```
Usage: aether logs tail [OPTIONS] <SERVICE>
```

| Flag | Description |
|------|-------------|
| `--api` | Show API access log instead of main log |
| `-g, --grep <GREP>` | Filter lines containing this pattern (case-insensitive) |

```bash
aether logs tail aether-automation --grep ERROR
```

### logs ui

Open interactive log viewer with scroll, search, and follow.

```
Usage: aether logs ui [OPTIONS] <SERVICE>
```

| Flag | Description |
|------|-------------|
| `--api` | Show API access log instead of main log |

```bash
aether logs ui aether-io
```

## aether shm

Zero-latency shared memory CLI (like mysql-cli). The subcommand is optional;
running bare `aether shm` opens the shared-memory file directly for an
interactive session (it fails if the SHM file does not exist yet).

```
Usage: aether shm [OPTIONS] [COMMAND]
```

### shm get

Get point value. Key format: `inst:<id>:M|A:<point_id>` or
`ch:<id>:T|S|C|A:<point_id>`.

```
Usage: aether shm get [OPTIONS] <KEY>
```

```bash
aether shm get inst:9:M:101
```

### shm info

Show shared memory statistics.

```
Usage: aether shm info [OPTIONS]
```

```bash
aether shm info --json
```

### shm watch

Watch key for changes (real-time monitoring).

```
Usage: aether shm watch [OPTIONS] <KEY>
```

| Flag | Description |
|------|-------------|
| `-i, --interval-ms <INTERVAL_MS>` | Polling interval in milliseconds (default: 500) |

```bash
aether shm watch ch:1001:T:101 --interval-ms 200
```

### shm top

Real-time TUI dashboard (like htop).

```
Usage: aether shm top [OPTIONS]
```

```bash
aether shm top
```

## aether doctor

Check system health and diagnose issues. For this command, `-v, --verbose`
shows detailed information (response times, etc.).

```
Usage: aether doctor [OPTIONS]
```

```bash
aether doctor --verbose
```

## aether templates

Manage channel configuration templates.

```
Usage: aether templates [OPTIONS] <COMMAND>
```

### templates list

List all channel templates.

```
Usage: aether templates list [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-p, --protocol <PROTOCOL>` | Filter by protocol type |

```bash
aether templates list --protocol modbus_tcp
```

### templates get

Show detailed information about a template.

```
Usage: aether templates get [OPTIONS] <ID>
```

```bash
aether templates get 3
```

### templates snapshot

Snapshot a channel's configuration as a reusable template.

```
Usage: aether templates snapshot [OPTIONS] --name <NAME> <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `-n, --name <NAME>` | Template name |
| `-d, --description <DESCRIPTION>` | Template description |

```bash
aether templates snapshot 1001 --name pcs-modbus-template
```

### templates apply

Apply a template to a target channel.

```
Usage: aether templates apply [OPTIONS] <TEMPLATE_ID> <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `--clear` | Clear existing points before applying |
| `--slave-id <SLAVE_ID>` | Override slave ID for Modbus |

```bash
aether templates apply 3 1002 --clear --slave-id 2
```

### templates delete

Delete a channel template.

```
Usage: aether templates delete [OPTIONS] <ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Force deletion without confirmation |

```bash
aether templates delete 3 --force
```

## aether alarms

Manage alarm rules (create/update/delete/enable/disable); query alerts,
events, and statistics.

```
Usage: aether alarms [OPTIONS] <COMMAND>
```

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

### net mqtt config-set

Replace uplink configuration from a JSON file (full `NetConfig` object).

```
Usage: aether net mqtt config-set [OPTIONS] --file <FILE>
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to a JSON file containing the complete `NetConfig` object |

```bash
aether net mqtt config-set --file netconfig.json
```

### net mqtt reconnect

Reconnect the MQTT client.

```
Usage: aether net mqtt reconnect [OPTIONS]
```

```bash
aether net mqtt reconnect
```

### net mqtt disconnect

Disconnect the MQTT client.

```
Usage: aether net mqtt disconnect [OPTIONS]
```

```bash
aether net mqtt disconnect
```

### net cert info

Show installed TLS certificate info.

```
Usage: aether net cert info [OPTIONS]
```

```bash
aether net cert info
```

### net cert delete

Delete a TLS certificate by type.

```
Usage: aether net cert delete [OPTIONS] <CERT_TYPE>
```

`<CERT_TYPE>` possible values: `ca_cert`, `client_cert`, `client_key`.

```bash
aether net cert delete client_cert
```

### net cert upload

Upload a TLS certificate file (max 1 MB). Accepted extensions: `.pem` `.crt`
`.key` `.cer` `.p12` `.pfx`.

```
Usage: aether net cert upload [OPTIONS] --type <CERT_TYPE> <FILE>
```

| Flag | Description |
|------|-------------|
| `--type <CERT_TYPE>` | Certificate role [possible values: `ca_cert`, `client_cert`, `client_key`] |

```bash
aether net cert upload ca.pem --type ca_cert
```

## aether history

Query historical sensor data (latest values, time-range queries).

```
Usage: aether history [OPTIONS] <COMMAND>
```

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

## aether top

Interactive TUI dashboard for real-time monitoring. No command-specific
flags.

```
Usage: aether top [OPTIONS]
```

```bash
aether top
```

## aether mcp

Run an MCP server exposing `aether`'s capabilities as tools. The server speaks
MCP JSON-RPC over stdio; the global `--json` flag does not change its output.

```
Usage: aether mcp [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--allow-write` | Add the 22 governed write tools to the 23 always-registered read-only tools. It is only a registration gate; each invocation still requires `confirmed: true` |

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
