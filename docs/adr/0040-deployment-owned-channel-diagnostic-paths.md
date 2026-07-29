# ADR-0040: Keep channel diagnostic paths deployment-owned

## Status

Accepted and implemented on 2026-08-20.

## Context

`ChannelLoggingConfig.file` was accepted by the IO HTTP DTO, copied through
`ChannelLoggingPolicy`, and persisted in the channel JSON document. The runtime
never read it. Channel composition always constructed `ChannelFileLogHandler`
from the deployment-level `AETHER_LOG_DIR` (default `/app/logs`), the fixed
`io/channels` namespace, a sanitized channel directory, and a daily filename.

Consequently, an API request could appear to select `/tmp/device.log` while the
runtime continued writing to the deployment-owned diagnostic directory. This
was a false capability and an unsafe contract to implement literally: arbitrary
paths could escape mounted storage, bypass retention, collide across channels,
or target files the service should not own.

## Decision

1. Remove `file` from `common::io_config::ChannelLoggingConfig` and from the
   transport-neutral `ChannelLoggingPolicy` port contract.
2. Remove the HTTP conversion, SQLite serialization, accessors, builders,
   OpenAPI field, and tests that advertised a caller-selected file.
3. Keep `enabled` and `level` as governed channel desired-state properties.
4. Keep the diagnostic destination deployment-owned:

   ```text
   ${AETHER_LOG_DIR:-/app/logs}/io/channels/<sanitized-channel-name>/<date>.log
   ```

5. Reject unknown channel logging properties, including the retired `file`
   property. AetherEdge is pre-release, so no compatibility reader or migration
   is retained for a capability that never had runtime effect.
6. Add an architecture check that rejects restoration of a caller-selected
   logging path.

## Consequences

- The channel API no longer claims or silently ignores a configuration value
  that has no runtime effect.
- API callers cannot request arbitrary filesystem writes.
- Deployment manifests and host policy remain responsible for mounts,
  permissions, capture, retention, and backup.
- Per-channel protocol evidence, daily files, configured verbosity, and
  automatic reconciliation are unchanged.
