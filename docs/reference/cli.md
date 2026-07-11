---
title: CLI Reference
description: Every aether command - services, sync, doctor, channels, rules, and more
updated: 2026-07-10
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

## aether sync

Sync all configuration to SQLite database.

```
Usage: aether sync [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-n, --dry-run` | Validate only, don't write to database (dry run) |
| `-f, --force` | Replace managed rows after successful validation instead of preserving unmatched rows |
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

Reload all channel configurations.

```
Usage: aether channels reload [OPTIONS]
```

```bash
aether channels reload
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
Usage: aether channels create [OPTIONS] --name <NAME> --protocol <PROTOCOL> --params <PARAMS>
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Channel name (must be unique) |
| `--protocol <PROTOCOL>` | Protocol type (`modbus_tcp`, `modbus_rtu`, `virtual`, `di_do`, `can`) |
| `--params <PARAMS>` | Protocol parameters as JSON string (e.g. `'{"host":"192.168.1.10","port":502}'`) |
| `--description <DESCRIPTION>` | Channel description |
| `--enabled <ENABLED>` | Start channel immediately (default: true) [possible values: `true`, `false`] |
| `--id <ID>` | Override channel ID (auto-assigned if omitted) |

```bash
aether channels create --name pcs-main --protocol modbus_tcp \
  --params '{"host":"192.168.1.10","port":502}'
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

```bash
aether channels update 1001 --description "PCS main feed"
```

### channels delete

Delete a channel and cascade-remove its points, mappings, and routing.

```
Usage: aether channels delete [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip confirmation prompt |

```bash
aether channels delete 1001 --force
```

### channels enable

Enable a channel.

```
Usage: aether channels enable [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels enable 1001
```

### channels disable

Disable a channel.

```
Usage: aether channels disable [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels disable 1001
```

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

Show all built-in products from `aether-model`.

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

Show detailed information about a built-in product.

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

Execute a control action on an instance (writes to the device).
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

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether models instances action 9 --point-id 1 --value 50
```

### models instances measurement

Set a measurement value on an instance. This writes into the measurement
hash that is normally fed by live device data, so treat it as a test and
simulation tool rather than a routine operation.

```
Usage: aether models instances measurement [OPTIONS] --point-id <POINT_ID> --value <VALUE> <INSTANCE_ID>
```

| Flag | Description |
|------|-------------|
| `--point-id <POINT_ID>` | Point ID: numeric (`"101"`) or semantic name |
| `--value <VALUE>` | Value to set |

```bash
aether models instances measurement 9 --point-id 101 --value 3.14
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

```bash
aether rules enable 3
```

### rules disable

Disable a business rule.

```
Usage: aether rules disable [OPTIONS] <RULE_ID>
```

```bash
aether rules disable 3
```

### rules execute

Execute a rule (evaluate and execute if conditions met).

```
Usage: aether rules execute [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Force execution even if conditions not met. Accepted by the CLI but currently ignored server-side: automation's execute handler takes no request body |

```bash
aether rules execute 3 --force
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

```bash
aether rules create --name night-charge --description "Charge during off-peak hours"
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

```bash
aether rules update 3 --flow-json flow.json
```

### rules delete

Delete a business rule.

```
Usage: aether rules delete [OPTIONS] <RULE_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip confirmation prompt |

```bash
aether rules delete 3 --force
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

```bash
aether routing create 9 --point-type m --point-id 101 \
  --channel-id 1001 --four-remote t --channel-point-id 101
```

### routing batch

Batch upsert routing from JSON file or stdin.

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

```bash
aether routing delete-instance bat-01 --force
```

### routing delete-channel

Delete all routing for a channel.

```
Usage: aether routing delete-channel [OPTIONS] <CHANNEL_ID>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip confirmation |

```bash
aether routing delete-channel 1001 --force
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

```
Usage: aether alarms rule-create [OPTIONS] --file <FILE>
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to a JSON file matching alarm's `CreateRuleRequest` |

```bash
aether alarms rule-create --file alarm-rule.json
```

### alarms rule-update

Update an alarm rule from a JSON file (only present fields change).

```
Usage: aether alarms rule-update [OPTIONS] --file <FILE> <ID>
```

| Flag | Description |
|------|-------------|
| `--file <FILE>` | Path to a JSON file matching alarm's `UpdateRuleRequest` |

```bash
aether alarms rule-update 7 --file alarm-rule-patch.json
```

### alarms rule-delete

Delete an alarm rule.

```
Usage: aether alarms rule-delete [OPTIONS] <ID>
```

```bash
aether alarms rule-delete 7
```

### alarms rule-enable

Enable an alarm rule.

```
Usage: aether alarms rule-enable [OPTIONS] <ID>
```

```bash
aether alarms rule-enable 7
```

### alarms rule-disable

Disable an alarm rule.

```
Usage: aether alarms rule-disable [OPTIONS] <ID>
```

```bash
aether alarms rule-disable 7
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
| `--allow-write` | Register write tools (channel writes, rule changes, config changes). Without this flag, only read-only tools appear in `tools/list` |

```bash
aether mcp --allow-write
```

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
