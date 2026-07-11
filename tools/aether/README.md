# Aether CLI

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)

Unified management tool for the [Aether](https://github.com/EvanL1/Aether)
AI-native, industry-neutral IoT edge kernel. Energy management is an optional
domain pack rather than a CLI or runtime prerequisite.

## Installation

### One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/EvanL1/Aether/main/tools/aether/install.sh | bash
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

# Remote machine
aether --host 192.168.30.21 channels list

# Interactive dashboard
aether --host 192.168.30.21 top
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

### Channels (aether-io)

| Command | Description |
|---------|-------------|
| `aether channels list` | List all communication channels |
| `aether channels status <id>` | Channel runtime status and statistics |
| `aether channels write <id> --type T\|S ...` | Inject supervised simulation telemetry |
| `aether channels reload` | Hot-reload channel configuration |
| `aether channels health` | Service health check |
| `aether models instances action ...` | Send the only supported external device command; requires `AETHER_ACCESS_TOKEN` from an Admin/Engineer session |

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
| `aether models products list` | List built-in product types |
| `aether models instances list` | List device instances |
| `aether models instances create <product> <name>` | Create device instance |
| `aether models instances get <name>` | Instance details |
| `aether models instances delete <name>` | Delete instance |

### Rules (aether-automation)

| Command | Description |
|---------|-------------|
| `aether rules list` | List business rules |
| `aether rules get <id>` | Rule details with flow definition |
| `aether rules enable <id>` | Enable rule |
| `aether rules disable <id>` | Disable rule |
| `aether rules execute <id>` | Execute rule (real execution — no dry-run) |

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
aether --host 192.168.30.21 top    # Remote
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
| `--host <IP>` | Target remote machine (overrides localhost) |
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
