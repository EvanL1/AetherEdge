# aether-ports

Small, object-safe capability interfaces for Aether edge extensions.

The crate separates authoritative live reads, acquisition-owned writes,
device command dispatch, audit, history, mirroring, durable outbox, uplink
publishing, I/O channel commissioning, and request-driven data processing.
`ChannelMutator` keeps durable desired configuration authoritative and reports
the rebuildable runtime projection, resulting revision, and reconciliation
state without choosing a wire encoding. `HistoryQuery` and
`CovariateSource` accept bounded logical windows and return source provenance;
`DataProcessor` receives a complete `DataProcessingRequest` and has no callback
into Aether data sources. It deliberately does not expose a generic database,
cache, model, or script-runner API. Hosts choose concrete adapters at the
composition boundary.

`HistoryQuery` bounds event time but does not implicitly promise bitemporal or
source-epoch history; an implementation must declare stronger point-in-time
semantics explicitly. Likewise, artifact chronology is not a history-port
responsibility.

Errors carry recovery semantics so callers can distinguish unavailable,
transient, rejected, invalid-data, and permanent failures.

```bash
cargo test -p aether-ports
```

Licensed under either MIT or Apache-2.0, at your option.
