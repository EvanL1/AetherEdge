---
title: Connect AI Assistants
description: Point Claude or any MCP client at aether mcp, and choose between read-only and write access
updated: 2026-07-22
---

# Connect AI Assistants

The `aether` CLI doubles as an MCP (Model Context Protocol) server: `aether mcp`
runs over stdio and exposes the system's capabilities as tools, so Claude — or
any MCP client — can inspect channels, query history, read alarms, and (when
explicitly allowed) operate the system. This page covers client setup, pointing
the server at a remote installation, and the read-only/write access model.

## What you get

The production MCP catalog has 45 tools in two tiers:

- **23 read-only tools**, always registered — listing and inspecting channels
  and their point mappings (`channels_list`, `channels_status`,
  `channels_points`), alarms and alarm rules (`alarms_list`, `alarms_stats`),
  control rules (`rules_list`, `rules_get`),
  routing, historical data (`history_query`, `history_latest`), product models
  and device instances (`models_products`, `models_instances`), channel
  templates, and cloud-link status (`net_mqtt_status`, `net_cert_info`).
- **22 governed write tools**, registered only when the server is started
  with `--allow-write`: `channels_create`, `channels_update`,
  `channels_delete`, `channels_enable`, `channels_disable`, and
  `channels_reconcile`;
  `models_instances_action`; `rules_execute`;
  `rules_create`, `rules_update`, `rules_delete`, `rules_enable`, and
  `rules_disable`; `alarms_rule_create`, `alarms_rule_update`,
  `alarms_rule_delete`, `alarms_rule_enable`, and `alarms_rule_disable`;
  `alarms_resolve`; and `routing_action_upsert`, `routing_action_delete`, and
  `routing_action_set_enabled`. They map respectively to the governed
  `io.channel.manage`, `io.channel.reconcile`, `device.write_point`, `automation.rule.execute`,
  `automation.rule.manage`, `alarm.rule.manage`, `alarm.alert.resolve`, and
  `automation.routing.manage` application capabilities. Every write requires
  a signed identity, `confirmed: true`, application authorization, and
  mandatory audit. See [Read-only vs write access](#read-only-vs-write-access)
  below.

Each tool wraps one CLI client call against the authenticated API gateway
(`aether-api:6005`) — the same remote application boundary every other client
uses (ADR-0021). Set `AETHER_ACCESS_TOKEN` for the session: the gateway
authenticates reads as well as writes. A Viewer token is enough for the
read-only tier; obtain one from `POST /api/v1/auth/login`. Results come back
as structured content; a failed or unreachable service comes back as readable
error text rather than an opaque protocol error.

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
the single API gateway (`aether-api:6005`), so one address configures
everything (ADR-0021). Resolution order at server startup:

1. **`--host <hostname>`** targets the gateway on that host with the default
   port: `aether mcp --host 192.168.1.50` resolves to
   `http://192.168.1.50:6005`.
2. **`AETHER_API_URL`** overrides the full base URL — scheme, host, and port —
   when `--host` is not passed.
3. Neither set: `http://localhost:6005`.

The transport guard rejects any token-carrying request over non-loopback
plaintext HTTP, so a remote gateway needs one of:

- **SSH stdio pipe** — run the server on the edge host; the gateway stays on
  loopback and nothing is exposed:

  ```bash
  claude mcp add aether -- ssh user@gateway aether mcp
  ```

- **SSH port-forward** — `ssh -L 6005:localhost:6005 user@gateway`, then run
  `aether mcp` locally against the default loopback URL.
- **One HTTPS ingress** in front of `6005` for standing remote access:
  `AETHER_API_URL=https://edge.example.test`.

In the Claude Desktop config, a remote write-enabled server looks like this
(replace the hostname with your ingress endpoint):

```json
{
  "mcpServers": {
    "aether-site-a": {
      "command": "aether",
      "args": ["mcp", "--allow-write"],
      "env": {
        "AETHER_API_URL": "https://edge.example.test",
        "AETHER_ACCESS_TOKEN": "<SIGNED_ADMIN_OR_ENGINEER_TOKEN>"
      }
    }
  }
}
```

## Read-only vs write access

By default, `aether mcp` is read-only. This is not an advisory annotation:
without `--allow-write`, the 22 write tools are never registered and do not
appear in the `tools/list` response at all. A client cannot call — or even
see — what is not registered, so the guarantee holds regardless of how the
client is configured or how the model behaves.

Starting the server with `--allow-write` is a deliberate act, but the flag is
only a registration gate. It is not confirmation for any command. The MCP
caller must still pass `confirmed: true` on every invocation, and the
application rejects unauthorized or unauditable requests before dispatch.
The MCP bridge reads `AETHER_ACCESS_TOKEN`, sends it to the service as an
`Authorization: Bearer` credential, and generates an `X-Request-ID` for each
governed request. It refuses to attach that credential to non-loopback
plaintext HTTP; remote writes require a certificate-validated HTTPS ingress.
Preserve the returned `request_id` and any `command_id`: timeouts and incomplete
audit or publication responses are not safe automatic retry signals.
A successful device-command response means the local command plane accepted
the command; it does not prove that the physical device executed it. A routing
response means the physical target was persisted and published; it does not
execute a device command. **Before
enabling writes, read [Safe Operations for Applications and Agents](safe-operations.md).**
If a command response reports `audit.status="incomplete"`, the command was
already accepted: retain its `request_id`/`command_id` and do not retry it.
Channel mutation success can also report a degraded runtime projection. Retain
its `request_id` and `resulting_revision`, inspect
`reconciliation_required`, and do not automatically retry the non-idempotent
commissioning command.

Channel simulation/point-batch and uplink configuration/certificate operations
remain outside MCP. Channel CRUD/lifecycle, rule CRUD/lifecycle, alarm-rule
CRUD/lifecycle, and alert resolution are present only because their schemas
and application capabilities are explicitly mapped in the 22-tool write
allowlist. No existing wrapper is promoted to an AI tool merely because
`--allow-write` is present.

The one-line rule: **give an assistant write access for a task, not as a
default.** Register the write-enabled server for the session that needs it,
and drop back to read-only afterward.

## Resources

Beyond tools, the server serves a curated subset of this documentation as
read-only MCP resources in both modes. Kernel pages are embedded; Pack
knowledge appears only when that validated Pack is active in `global.yaml`.
Clients that support MCP resources can pull context directly instead of relying
on the model's prior knowledge:

- `aether://packs/energy/knowledge/ess-primer` — energy-storage concepts when the Energy Pack is active
- `aether://packs/energy/knowledge/safe-operations` — the Energy Pack safety contract
- `aether://docs/concepts/architecture` — the seven services and how they talk
- `aether://docs/concepts/data-model` — instances, channels, points
- `aether://docs/reference/mcp-tools` — the full tool reference

## Related pages

- [Safe Operations for Applications and Agents](safe-operations.md) — read this before `--allow-write`
- [System Architecture](../concepts/architecture.md) — the services behind the tools
- [MCP Tools Reference](../reference/mcp-tools.md) — every tool with its parameters
- [Getting Started](getting-started.md) — build, initialize, and start the stack the tools talk to
