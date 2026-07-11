# ADR-0003: Keep production services isolated over SHM and per-consumer events

## Status

Accepted and implemented for the peripheral read paths on 2026-07-10.

## Context

Aether runs on embedded Linux gateways where protocol drivers, rule execution,
history I/O, cloud networking, and management traffic have different failure
and latency characteristics. Collapsing them into one process would allow a
driver fault, allocator pressure, blocked database call, or cloud-library bug
to stop acquisition and control together.

Redis previously doubled as a live-value relay. That made four otherwise local
consumers fail to start or operate when Redis was absent, even though the
authoritative values already existed in the shared-memory data plane.

## Decision

1. The production runtime remains six independently supervised Rust service
   processes: aether-io, aether-automation, aether-alarm, aether-history,
   aether-api, and aether-uplink.
2. SQLite is the configuration/discovery source. SHM is the current-value
   source. A consumer never discovers configured points by scanning a cache.
3. PointWatch is a hint plane, not a value plane. Every event consumer owns a
   separate mmap subscription bitmap and UDS socket, and re-reads SHM after a
   hint. Dropped events are repaired by periodic polling/reconciliation.
4. Channel connectivity uses a separate health SHM segment so it cannot alter
   the measurement layout or masquerade as a point value.
5. Redis is an optional compatibility `StateMirror`. PostgreSQL/TimescaleDB are
   optional history adapters. Neither is required by aether-alarm, aether-api,
   aether-history, or aether-uplink.
6. aether-history defaults to an embedded SQLite history database. aether-uplink commits MQTT
   payloads to a bounded local durable outbox before network submission.
7. Command consumers do not write SHM directly. For example, aether-uplink routes
   cloud writes through aether-automation/aether-io command APIs, preserving validation,
   offline gates, dispatch ownership, and audit boundaries.
8. A single-process composition is permitted for tests and simulation only.
   It must use the same ports and cannot replace the production service form.

## Failure semantics

- A missing or restarting SHM writer is a retryable read-time condition; the
  peripheral service can still start and expose health/configuration APIs.
- Writer startup publishes both point and health segments as private
  generations followed by atomic rename. Existing readers keep a valid old
  mmap until their inode check reopens the canonical path; consumer bitmaps
  are reopened rather than truncated across aether-io restarts.
- A layout mismatch, invalid logical address, or non-finite command is a
  permanent/configuration error and is not silently redirected to Redis.
- Events may be dropped under bounded backpressure. Consumers retain periodic
  reconciliation, so correctness does not depend on lossless UDS delivery.
- Each consumer's bitmap/socket is isolated. A slow or crashed service cannot
  steal another service's events or overwrite its subscriptions.
- External history/uplink failures cannot block aether-io's acquisition loop.

## Consequences

The runtime keeps more processes than a monolith, but gains independent
restart, memory limits, fault containment, and clearer ownership. Shared
libraries hold wire formats, ports, and validation so process isolation does
not imply duplicated architecture. Redis/PostgreSQL integrations remain
available to deployments that need remote observability or larger history,
without contaminating the default edge dependency graph.
