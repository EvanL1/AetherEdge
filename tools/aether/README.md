# Aether CLI and MCP

`aether` is the commissioning and application client for AetherEdge. It keeps
one concrete authenticated HTTP client for CLI and MCP and reserves direct
SQLite access for explicit offline operations.

## Install

```bash
cargo install --path tools/aether
```

A packaged deployment installs the same binary with the six Rust services.
Docker Compose or systemd owns process supervision; journalctl, container logs,
and the operator's observability stack own logs.

## Safe offline bootstrap

Stop runtime owners before applying desired configuration:

```bash
aether init
aether sync --dry-run
aether sync --confirmed
aether status --detailed
aether export --output ./backup
```

`sync --force` is a full replacement and still requires `--confirmed`.

## Authenticated application operations

Set a signed gateway token, then use query and governed command groups:

```bash
export AETHER_ACCESS_TOKEN='<signed JWT>'
aether channels list --json
aether models instances list --json
aether rules list --json
aether history health --json
aether alarms stats --json
aether net mqtt status --json
```

Consequential channel, rule, alarm, action-routing, and device operations
require the documented role, explicit `--confirmed`, application revision where
applicable, and durable audit. Do not retry an accepted non-idempotent operation
from a timeout alone.

## Other retained commands

- `aether runtime-manifest` verifies the feature-exact composition artifact.
- `aether packs build|install` handles data-only Pack artifacts.
- `aether templates list|get` inspects channel templates.
- `aether shm get|info|watch` provides one-shot local read diagnostics.
- `aether mcp` starts the default read-only MCP server.
- `aether mcp --allow-write` registers the bounded governed write set for one
  session; each invocation still confirms separately.

Host setup planners, service wrappers, aggregate doctor, log viewers, and TUI
dashboards are intentionally absent. Use the installer, Docker Compose or
systemd, service `/health` endpoints, and standard logging tools.

## Endpoint selection

1. `--host <host>` selects `http://<host>:6005`.
2. `AETHER_API_URL` overrides the complete gateway origin.
3. The default is `http://localhost:6005`.

The token is sent only in `Authorization: Bearer`; HTTPS is required for a
non-loopback host.

See [CLI reference](../../docs/reference/cli.md) and
[MCP reference](../../docs/reference/mcp-tools.md).
