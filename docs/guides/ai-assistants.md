---
title: Connect AI Assistants
description: Point Claude or any MCP client at aether mcp, and choose between read-only and write access
updated: 2026-07-10
---

# Connect AI Assistants

The `aether` CLI doubles as an MCP (Model Context Protocol) server: `aether mcp`
runs over stdio and exposes the system's capabilities as tools, so Claude — or
any MCP client — can inspect channels, query history, read alarms, and (when
explicitly allowed) operate the system. This page covers client setup, pointing
the server at a remote installation, and the read-only/write access model.

## What you get

`aether mcp` registers 48 tools in two tiers:

- **23 read-only tools**, always registered — listing and inspecting channels
  and their point mappings (`channels_list`, `channels_status`,
  `channels_points`), alarms and alarm rules (`alarms_list`, `alarms_stats`),
  control rules (`rules_list`, `rules_get`),
  routing, historical data (`history_query`, `history_latest`), product models
  and device instances (`models_products`, `models_instances`), channel
  templates, and cloud-link status (`net_mqtt_status`, `net_cert_info`).
- **25 write tools**, registered only when the server is started with
  `--allow-write` — device writes, channel/rule/alarm-rule configuration, and
  MQTT/certificate management. See
  [Read-only vs write access](#read-only-vs-write-access) below.

Each tool wraps one CLI client call against the same service HTTP APIs the
`aether` command line uses. Results come back as structured content; a failed
or unreachable service comes back as readable error text rather than an opaque
protocol error.

The server also serves the documentation you are reading now as MCP
[resources](#resources), so an assistant can learn the domain — what a PCS is,
which writes reach real hardware — without leaving the session.

One flag note: the CLI's global `--json` flag is ignored for `mcp` (the server
always speaks MCP's own JSON-RPC protocol) and prints a warning if passed.

## Claude Desktop

Add to `claude_desktop_config.json` (the `aether` binary must be on `PATH`,
or use an absolute path):

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

## Claude Code

```bash
claude mcp add aether -- aether mcp
```

For a session that needs write access (see the access model below):

```bash
claude mcp add aether -- aether mcp --allow-write
```

## Pointing at a remote system

The MCP server does not have to run on the edge device. Every tool talks to
the Aether services over HTTP, so `aether mcp` on a laptop can drive a remote
installation. Two mechanisms, both resolved at server startup:

- **`--host <hostname>`** rewrites the host for all five service URLs while
  keeping the default ports — the quick path when all services run on one box:

  ```bash
  aether mcp --host 192.168.1.50
  ```

- **Five environment variables** set each service URL independently, useful
  when ports or hosts differ per service:

  | Environment variable | Service | Tools served | Default |
  |----------------------|---------|--------------|---------|
  | `AETHER_IO_URL` | io | channels, points, templates | `http://localhost:6001` |
  | `AETHER_AUTOMATION_URL` | automation | rules, routing, models/instances | `http://localhost:6002` |
  | `AETHER_ALARM_URL` | alarm | alarms | `http://localhost:6007` |
  | `AETHER_UPLINK_URL` | uplink | MQTT, certificates | `http://localhost:6006` |
  | `AETHER_HISTORY_URL` | history | history | `http://localhost:6004` |

Precedence: `--host` wins — when it is passed, the environment variables are
not consulted. When neither is set, everything defaults to `localhost`.

In the Claude Desktop config, use the `env` block:

```json
{
  "mcpServers": {
    "aether-site-a": {
      "command": "aether",
      "args": ["mcp"],
      "env": {
        "AETHER_IO_URL": "http://192.168.1.50:6001",
        "AETHER_AUTOMATION_URL": "http://192.168.1.50:6002",
        "AETHER_ALARM_URL": "http://192.168.1.50:6007",
        "AETHER_UPLINK_URL": "http://192.168.1.50:6006",
        "AETHER_HISTORY_URL": "http://192.168.1.50:6004"
      }
    }
  }
}
```

## Read-only vs write access

By default, `aether mcp` is read-only. This is not an advisory annotation:
without `--allow-write`, the 25 write tools are never registered and do not
appear in the `tools/list` response at all. A client cannot call — or even
see — what is not registered, so the guarantee holds regardless of how the
client is configured or how the model behaves.

Starting the server with `--allow-write` is a deliberate act. Some of the
tools it unlocks move real hardware — power setpoints, breakers, generator
start/stop — and one of them overwrites live telemetry. **Before enabling it,
read [Safe Operations for AI Agents](../domain/safe-operations.md)**, which
classifies every write tool by severity and states the operating rules an
agent must follow.

The one-line rule: **give an assistant write access for a task, not as a
default.** Register the write-enabled server for the session that needs it,
and drop back to read-only afterward.

## Resources

Beyond tools, the server serves an embedded, curated subset of this
documentation as MCP resources under `aether://docs/...` — read-only, and
available in both modes. Clients that support MCP resources can pull domain
context directly instead of relying on the model's prior knowledge:

- `aether://docs/domain/ess-primer` — energy-storage concepts (PCS, BMS, SOC)
- `aether://docs/domain/safe-operations` — the safety contract for agents
- `aether://docs/concepts/architecture` — the seven services and how they talk
- `aether://docs/concepts/data-model` — instances, channels, points
- `aether://docs/reference/mcp-tools` — the full tool reference

## Related pages

- [Safe Operations for AI Agents](../domain/safe-operations.md) — read this before `--allow-write`
- [System Architecture](../concepts/architecture.md) — the services behind the tools
- [MCP Tools Reference](../reference/mcp-tools.md) — every tool with its parameters
- [Getting Started](getting-started.md) — build, initialize, and start the stack the tools talk to
