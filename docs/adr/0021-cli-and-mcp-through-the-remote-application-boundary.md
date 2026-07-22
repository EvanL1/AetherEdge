# ADR-0021: CLI and MCP consume the remote application boundary

## Status

Accepted.

## Context

The architecture states that remote applications enter only through the
authenticated `aether-api:6005` boundary and that the internal IO, automation,
history, uplink, and alarm ports stay on loopback. The API gateway already
enforces that boundary: JWT authentication on every proxied route, an
Engineer/Admin role gate plus a mandatory `x-aether-confirmed: true` header on
governed mutations, a forwarded-header allowlist, and internal-admin path
blocking, with reverse proxies for all five capability domains under
`/api/v1/{io,automation,history,uplink,alarm}/`.

The `aether` CLI and its MCP server did not use that boundary. Their domain
clients called the five internal service ports directly and resolved five
independent base URLs (`AETHER_IO_URL`, `AETHER_AUTOMATION_URL`,
`AETHER_ALARM_URL`, `AETHER_UPLINK_URL`, `AETHER_HISTORY_URL`, or `--host`
rewriting all five). Pointing MCP at a remote system therefore required either
exposing internal loopback-only ports on the network — contradicting the
boundary — or deploying five separate HTTPS ingresses.

## Decision

1. The CLI data plane — every domain client used by CLI subcommands and by
   `aether mcp` — sends requests only to the API gateway. One base URL replaces
   five: `--host <h>` resolves to `http://<h>:6005`, the single
   `AETHER_API_URL` environment variable overrides it, and the default is
   `http://localhost:6005`. The five per-service URL variables are removed.
2. Domain clients use gateway-native paths: `/api/v1/io/api/...` and
   `/api/v1/automation/api/...` pass through unchanged, while the history,
   uplink, and alarm clients drop their service-local `hisApi/`, `netApi/`, and
   `alarmApi/` prefixes, which the gateway prepends when forwarding.
3. Every data-plane request is authenticated. Clients attach the
   `AETHER_ACCESS_TOKEN` Bearer token to reads as well as writes, and the
   existing transport guard (no Bearer token over non-loopback plaintext HTTP)
   applies to every token-carrying request. Reads without a token fail with the
   gateway's 401, including on-device; read-only sessions use a Viewer token.
4. The MCP write surface is unchanged: `--allow-write` remains a registration
   gate, every write still requires `confirmed: true`, and the
   write-tool-to-capability mapping stays fixed and test-enforced.
   The gateway treats every non-GET/HEAD request as a governed mutation and
   requires the `x-aether-confirmed: true` header. Where an operation carries
   explicit confirmation (governed writes), the header is attached only after
   that validation; for service-level unguarded mutations with no confirmation
   parameter (instance CRUD, measurement routing, uplink operations), the CLI
   invocation itself is the operator's confirmation and the client attaches
   the header unconditionally. Introducing explicit confirmation for those
   operations is a separate application-contract decision.
5. Remote access needs exactly one path: an HTTPS ingress in front of `6005`,
   an SSH port-forward to loopback `6005`, or running `aether mcp` on the edge
   host over an SSH stdio pipe.

## Consequences

- The boundary statement and the shipped tools now agree; no guide instructs
  operators to expose internal service ports.
- Remote MCP configuration collapses from five URLs to one.
- Local tokenless reads stop working; the getting-started and AI-assistant
  guides document obtaining a token from `/api/v1/auth/login`.
- The `aether-api` process becomes a runtime dependency of the CLI data plane
  and of MCP. `aether setup`, `aether services`, and `aether doctor` remain
  independent of it.
- A native MCP streamable-HTTP endpoint served by `aether-api` itself — letting
  clients that cannot spawn a local process connect with one URL — is the
  planned second phase and will be recorded as its own application surface
  when built.

## Compatibility

This is a pre-1.0 breaking change to CLI/MCP configuration, not to any service
API. Existing MCP client configurations that set the five service URL variables
must switch to `AETHER_API_URL` (or `--host`). The internal service APIs are
unchanged and continue to serve the gateway and on-device diagnostics.
