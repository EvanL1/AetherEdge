# Aether CLI

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)

Unified management tool for the [AetherIot](https://github.com/EvanL1/AetherIot)
AI-native, industry-neutral IoT edge kernel. Energy management is an optional
domain pack rather than a CLI or runtime prerequisite.

## Installation

### One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/EvanL1/AetherIot/main/tools/aether/install.sh | bash
```

Auto-detects a published platform artifact and installs to `~/.local/bin`.
Unsupported OS/architecture pairs fail before downloading anything.
This installs the management client only; it does not install the six-process
edge runtime, Docker images, or a Compose file. Use the release `.run`
installer on an edge host, or the repository deployment guide, before running
service lifecycle commands.

### Bun / npm (cross-platform including Windows)

```bash
bun install -g @aether/aether
# or
npm install -g @aether/aether
```

### From Source

```bash
cargo install --path tools/aether
```

## Quick Start

```bash
# Persistently read-only first-run plan
aether setup

# Apply only the unchanged safe plan ID printed above
aether setup apply --plan-id <PLAN_ID>

# After installing a runtime package, start and verify its composition
aether services start
aether doctor

# Local operations
aether channels list
aether models instances list
aether rules list

# Remote read-only inspection
aether --host 192.0.2.10 channels list

# Interactive dashboard
aether --host 192.0.2.10 top
```

`aether setup` can prepare a safe local configuration/database workspace with
the standalone CLI. `aether services start` requires an installed systemd or
Docker Compose runtime and fails if that composition is not present.

## Commands

### Configuration

| Command | Description |
|---------|-------------|
| `aether setup` | Generate a read-only, AI-friendly first-run plan |
| `aether setup apply --plan-id <ID>` | Apply only an unchanged safe empty-site plan |
| `aether init` | Initialize SQLite database schema |
| `aether sync` | Sync YAML/CSV config to database |
| `aether sync --dry-run` | Validate config without writing |
| `aether export` | Export config from database to files |
| `aether status` | Show configuration status |
| `aether doctor` | Full system health check |
| `aether runtime-manifest [--path <FILE>]` | Verify and inspect feature-exact runtime metadata |
| `aether packs build ...` | Build a data-only Pack artifact for one exact runtime manifest |
| `aether packs install --artifact <DIR>` | Verify, publish, and atomically activate a Pack artifact |

### Channels (aether-io)

| Command | Description |
|---------|-------------|
| `aether channels list` | List all communication channels |
| `aether channels status <id>` | Channel runtime status and statistics |
| `aether channels create ... --confirmed` | Create a channel disabled by default; requires `AETHER_ACCESS_TOKEN` |
| `aether channels update <id> ... --expected-revision <rev> --confirmed` | Update desired channel configuration with mandatory compare-and-set |
| `aether channels enable\|disable <id> --expected-revision <rev> --confirmed` | Change desired runtime lifecycle with mandatory compare-and-set |
| `aether channels delete <id> --expected-revision <rev> --confirmed` | Delete a channel with mandatory compare-and-set; `--force` only skips the prompt and action-route references fail with a conflict |
| `aether channels write <id> --type T\|S ...` | Inject supervised simulation telemetry |
| `aether channels reload --confirmed` | Reconcile all channel runtimes through `io.channel.reconcile`; requires `AETHER_ACCESS_TOKEN` and must not be retried automatically |
| `aether channels health` | Service health check |
| `aether models instances action ... --confirmed` | Submit the only supported external device command to the local command plane; requires explicit confirmation and `AETHER_ACCESS_TOKEN` from an Admin/Engineer session |

### Templates (aether-io)

| Command | Description |
|---------|-------------|
| `aether templates list` | List channel configuration templates |
| `aether templates get <id>` | Template details |
| `aether templates snapshot <ch_id>` | Snapshot channel as reusable template |
| `aether templates apply <tpl_id> <ch_id>` | Apply template to target channel |
| `aether templates delete <id>` | Delete template |

### Models (aether-automation)

| Command | Description |
|---------|-------------|
| `aether models products list` | List active Pack and site product types |
| `aether models instances list` | List device instances |
| `aether models instances create <product> <name>` | Create device instance |
| `aether models instances get <name>` | Instance details |
| `aether models instances delete <name>` | Delete instance |

### Rules (aether-automation)

| Command | Description |
|---------|-------------|
| `aether rules list` | List business rules |
| `aether rules get <id>` | Rule details with flow definition |
| `aether rules enable <id> --confirmed` | Enable rule; requires `AETHER_ACCESS_TOKEN` |
| `aether rules disable <id> --confirmed` | Disable rule; requires `AETHER_ACCESS_TOKEN` |
| `aether rules create --name <name> --confirmed` | Create a disabled rule; requires `AETHER_ACCESS_TOKEN` |
| `aether rules update <id> ... --confirmed` | Change rule policy; requires `AETHER_ACCESS_TOKEN` |
| `aether rules delete <id> --confirmed` | Delete a rule; `--force` only skips the prompt |
| `aether rules execute <id> --confirmed` | Evaluate a rule and submit selected actions to the local command plane; requires explicit confirmation and `AETHER_ACCESS_TOKEN` |
| `aether routing action upsert/delete/enable/disable ... --confirmed` | Govern one physical C/A command route; requires `AETHER_ACCESS_TOKEN` |

The production MCP catalog contains 45 tools: 23 read-only tools are always
registered, while `aether mcp --allow-write` adds exactly 22 governed writes:

- channel commissioning: `channels_create`, `channels_update`,
  `channels_delete`, `channels_enable`, `channels_disable`, plus
  `channels_reconcile` for explicitly confirmed runtime convergence;
- device and execution commands: `models_instances_action`, `rules_execute`;
- rule CRUD and lifecycle: `rules_create`, `rules_update`, `rules_delete`,
  `rules_enable`, `rules_disable`;
- alarm-rule CRUD and lifecycle: `alarms_rule_create`, `alarms_rule_update`,
  `alarms_rule_delete`, `alarms_rule_enable`, `alarms_rule_disable`, plus
  `alarms_resolve` for manual alert resolution;
- action-route governance: `routing_action_upsert`, `routing_action_delete`,
  `routing_action_set_enabled`.

`--allow-write` is only a registration gate; every invocation still requires
`confirmed: true`, application authorization, and mandatory audit. The MCP
bridge reads `AETHER_ACCESS_TOKEN`, sends it as an `Authorization: Bearer`
credential, and adds an `X-Request-ID` to each governed HTTP request. Retain
returned `request_id`/`command_id` values and never automatically retry a write
whose timeout, audit, or publication result is incomplete. Routing mutations
change future physical command targets but do not execute a device command. A
successful device-command response means local acceptance, not confirmed
physical-device execution. Channel mutation success may be a degraded runtime
projection; inspect `request_id`, `resulting_revision`, and
`reconciliation_required` before any separately confirmed follow-up, and do
not automatically retry it.

Bearer credentials are allowed over loopback HTTP for on-device operation,
but are never attached to non-loopback plaintext HTTP requests. Remote
protected writes must configure the relevant `AETHER_*_URL` variables with a
certificate-validated HTTPS ingress. `--host` constructs plaintext service
URLs on the default ports, so use it only for remote read-only inspection.

### Live data (SHM)

| Command | Description |
|---------|-------------|
| `aether shm get <key>` | Read one authoritative SHM value |
| `aether shm watch <key>` | Watch one SHM value for changes |
| `aether shm info` | Show SHM layout and writer health |
| `aether shm top` | Open the local SHM dashboard |
| `aether models instances data <id>` | Read instance values through the SHM-backed API |

### Infrastructure

| Command | Description |
|---------|-------------|
| `aether services start` | Start Docker services |
| `aether services stop` | Stop services |
| `aether services status` | Service status |
| `aether services logs <svc>` | View service logs |
| `aether logs level <svc> <level>` | Dynamic log level adjustment |
| `aether shm top` | Local shared memory TUI monitor |

### Interactive Dashboard

```bash
aether top                          # Local
aether --host 192.0.2.10 top    # Remote
```

| Key | Action |
|-----|--------|
| `←` `→` / `Tab` | Switch views (Channels / Instances / Rules) |
| `↑` `↓` / `j` `k` | Navigate within list |
| `Enter` | Drill into detail (points, live data, routing) |
| `Esc` | Back to parent view |
| `1` `2` `3` | Jump to view directly |
| `z` | Toggle hide zero values |
| `r` | Force refresh |
| `q` | Quit |

## Global Flags

| Flag | Description |
|------|-------------|
| `--host <IP>` | Target a remote machine over direct HTTP for read-only inspection; protected writes require HTTPS service URLs |
| `--json` | Structured JSON output for scripts and AI agents |
| `--verbose` | Enable debug logging |
| `--no-color` | Disable colored output |
| `--config-path <path>` | Override config directory |
| `--db-path <path>` | Override database directory |

## JSON Output

All commands support `--json` for structured output:

```bash
aether --json channels list
# {"success": true, "data": [...]}

aether --json models instances data 9
# {"success": true, "data": {"measurements": {...}, "actions": {...}}}
```

Set `AETHER_JSON=1` to enable JSON by default:

```bash
export AETHER_JSON=1
aether channels list    # Outputs JSON without --json flag
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AETHER_JSON` | Force JSON output | — |
| `AETHER_IO_URL` | Io HTTP URL | `http://localhost:6001` |
| `AETHER_AUTOMATION_URL` | Automation HTTP URL | `http://localhost:6002` |
| `AETHER_CONFIG_PATH` | Config directory path | Auto-detect |
| `AETHER_DATA_PATH` | Data directory path | Auto-detect |

## Platform Support

| Platform | Architecture | Status |
|----------|-------------|--------|
| Linux | x86_64, aarch64 | Supported |
| macOS | Apple Silicon (aarch64) | Supported |
| Windows | x86_64 | Supported artifact; installer requires Git Bash/MSYS2 |
| WSL | x86_64, aarch64 | Supported (uses Linux artifact) |
| macOS Intel | x86_64 | Not published; build from source |
| Windows on ARM | aarch64 | Not published; build from source |

## License

MIT — see [LICENSE](../../LICENSE)
