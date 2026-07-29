# ADR-0038: Compose IO directly from SQLite configuration

## Status

Accepted and implemented on 2026-08-19.

## Context

`ConfigManager` wrapped one `IoConfig` loaded by `IoSqliteLoader`. It added no
cache invalidation, generation control, validation authority, or alternate
configuration source. Its `get_channel`, `channel_count`, duplicate-ID
validation, and CSV directory validation methods had no production consumer.
The duplicate-ID check was also weaker than the authoritative SQLite primary
key, while the CSV layout belonged to offline commissioning rather than the
runtime service.

The IO crate also exposed compatibility aliases and forwarding modules for
configuration, channel types, protocol types, lifecycle helpers, and the SHM
store. The package is not published, and repository consumers already use the
owning modules. These exports represented no independent capability.

## Decision

1. Delete `ConfigManager`. Startup and offline `--validate` resolve the unified
   site database path once and call `IoSqliteLoader` directly.
2. Pass the loaded channel snapshot to `start_communication_service` rather
   than passing a wrapper around the complete application configuration.
3. Keep full runtime channel point loading in `IoSqliteLoader`; it remains the
   SQLite snapshot authority used by channel composition.
4. Remove the `AppConfig` and `ServiceConfig` aliases. Use the owning
   `common::IoConfig`, `BaseServiceConfig`, and `ApiConfig` types explicitly.
5. Remove the forwarding `channels::traits` module, the uncalled secondary CAN
   metadata converter, and unused channel, protocol-prelude, factory, root
   lifecycle, root manager, root SHM-store, and root error re-exports. Internal
   and integration consumers import from the owning module.
6. Add architecture checks that reject restoration of the wrapper and
   compatibility namespace.

## Consequences

- Runtime configuration has one concrete loader and one site database path.
- Offline CSV validation remains owned by the commissioning CLI; IO does not
  inspect source files after deployment.
- Lifecycle code depends on the channel snapshot it actually consumes.
- Protocol implementations, SHM authority, governed reconciliation, device
  command handling, and all maintained physical protocols are unchanged.
