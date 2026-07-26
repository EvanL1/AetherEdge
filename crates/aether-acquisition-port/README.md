# aether-acquisition-port

Owner-only write capability for physical telemetry and status acquisition.

This crate intentionally sits beside the general read/application ports. Only
`aether-shm-bridge` depends on it directly and implements
`AcquisitionStateWriter`; IO receives that concrete adapter at its composition
root. HTTP, CLI, MCP, AI, and the other service processes depend on
`aether-ports` and therefore cannot acquire the writer trait accidentally.
Cargo-metadata architecture tests enforce that dependency allowlist.

A writer validates the complete `AcquiredPointSample` batch before the first
SHM mutation and rejects unknown addresses or points owned by another writer.

```bash
cargo test -p aether-acquisition-port
```

Licensed under either MIT or Apache-2.0, at your option.
