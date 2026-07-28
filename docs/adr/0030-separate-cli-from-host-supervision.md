# ADR-0030: Separate the CLI from host supervision and terminal dashboards

## Status

Accepted and implemented on 2026-07-28. Partially supersedes ADR-0006 and
ADR-0021 where they assigned setup, service supervision, diagnostics, logs, or
terminal dashboards to the `aether` binary.

## Context

The CLI accumulated parallel wrappers around Docker Compose, systemd,
journalctl, service health endpoints, filesystem setup, logs, and several TUI
dashboards. These paths duplicated host tooling and the installers, selected
host-specific mechanisms inside the application client, and added more than six
thousand lines to the production binary.

They were not shared by MCP or HTTP and were unrelated to governed command and
query use cases. Keeping them contradicted the requirement that CLI, MCP, and
HTTP share one application API rather than duplicate operational behavior.

## Decision

1. `aether` retains offline `init`, `sync`, `status`, and `export`; authenticated
   application queries and governed commands; runtime-manifest and Pack
   operations; SHM one-shot reads; and MCP.
2. First installation and safe-empty configuration activation remain installer
   responsibilities. Offline commissioning uses `sync --dry-run` followed by
   `sync --confirmed` while runtime owners are stopped.
3. Docker Compose or systemd owns start, stop, restart, and status. Journalctl,
   container logs, and the operator's observability stack own logs.
4. Service `/health`, authenticated query routes, and distribution checks own
   health evidence. The CLI does not aggregate another health model.
5. Interactive top/log/SHM dashboards are downstream console concerns and are
   removed with their terminal dependencies.
6. Architecture and CLI tests reject restoration of the retired modules and
   commands.

## Consequences

The deployable six-process runtime and installer remain unchanged. Operators use
standard host supervisors and health endpoints rather than compatibility
wrappers. Automation receives simpler, stable JSON/HTTP commands without an
interactive terminal mode. Existing scripts that called the retired convenience
commands must switch to Docker Compose, systemctl/journalctl, or REST health
queries.
