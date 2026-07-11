# ADR-0004: Use capability-oriented canonical service names

## Status

Accepted and implemented on 2026-07-10.

## Context

The production processes used abbreviated implementation-era names that did
not communicate their responsibilities to operators, library users, or AI
clients. The names also leaked independently into Cargo packages, executables,
containers, systemd units, configuration namespaces, health payloads, and
local event paths.

## Decision

The six public runtime identities are:

| Canonical name | Responsibility | Source/config role directory |
|---|---|---|
| `aether-io` | Device protocols, acquisition, and device command dispatch | `io` |
| `aether-automation` | Instances, rules, and action orchestration | `automation` |
| `aether-alarm` | Alarm rules, state, and lifecycle | `alarm` |
| `aether-history` | Historical sampling, queries, and storage adapters | `history` |
| `aether-api` | Management REST API, WebSocket, and authentication | `api` |
| `aether-uplink` | MQTT/cloud connectivity and offline forwarding | `uplink` |

The canonical name is used without aliases for Cargo package and binary
names, Compose services and containers, systemd units, CLI service selectors,
runtime `ServiceInfo`, health payloads, and SQLite `service_config` namespaces.
Environment variables use the matching `AETHER_IO_*`,
`AETHER_AUTOMATION_*`, `AETHER_ALARM_*`, `AETHER_HISTORY_*`, `AETHER_API_*`,
and `AETHER_UPLINK_*` prefixes.

Concise role names remain only where they describe repository layout or a data
address rather than a runnable service. For example, source lives under
`services/io`, configuration under `config/io`, and channel mirror addresses
use the `io:` role namespace.

No deprecated executables, unit aliases, Compose aliases, environment aliases,
or dual SQLite namespaces are provided. The aether-automation PointWatch
bitmap and UDS path are also explicitly named for that consumer instead of
using an implicit default.

## Consequences

- An upgrade is intentionally breaking and must stop the complete process set,
  replace every binary/unit/Compose definition together, run `aether sync`,
  and restart the six processes.
- External supervision, dashboards, log collectors, and scripts must use the
  canonical names in the table above.
- Process isolation, ports, HTTP paths, SHM authority, and write ownership do
  not change.
- CI rejects retired identifiers in active edge-kernel paths and verifies the
  canonical Cargo, Compose, and systemd identities with
  `scripts/check-service-names.sh`.
