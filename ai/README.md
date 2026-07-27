# AI-native assets

This directory contains vendor-neutral assets used by coding agents and by AI
operators of an Aether gateway.

- `catalog.yaml` tells an agent where a component lives and how to verify it.
- `invariants.md` lists rules that must survive every refactor.
- `safety-policy.yaml` is the machine-readable mirror of the typed Rust
  capability catalog. A contract test requires exact capability-set, kind,
  risk, permission, idempotency, confirmation, and audit-policy equality.
- `runbooks/` contains deterministic change procedures.
- `evals/` contains declarative AI-facing scenarios tied to deterministic test
  evidence. It does not introduce a separate eval runner.

Data-processing changes start with
[`runbooks/add-data-processor.md`](runbooks/add-data-processor.md). It preserves
the boundary in which Aether assembles governed data and an optional processor
returns non-authoritative `DerivedData` without direct access to SHM, history
storage, configuration, or device control.

The landed v1 external surface is the authenticated
`/api/v1/data-processing/*` HTTP API. CLI and MCP bindings are not implemented
yet; when added, they must remain thin callers of the same application use
cases. `data_processing.process` is non-idempotent and requires durable audit.

Agents must treat `as_of` as an event-time frame boundary, not a claim of
point-in-time backtest safety. The current historian lacks ingestion/source
epochs and artifact provenance lacks training/availability cuts. Frozen inputs
or stronger adapters/contracts are required before calling historical results
leakage-safe; persisted historian settings also require reconnect/restart and
sentinel verification before they describe the active writer.

The implementation map is:

| Boundary | Entry point |
|---|---|
| Application orchestration | [`aether-application::data_processing`](../crates/aether-application/src/data_processing.rs) |
| Strict v1 codec | [`crates/aether-data-processing`](../crates/aether-data-processing/README.md) |
| Machine-readable contracts | [`contracts/data-processing`](../contracts/data-processing/README.md) |
| AI-facing eval scenarios | [`evals/data-processing.yaml`](evals/data-processing.yaml) |
| API-owned HTTP adapter | [`services/api/adapters/http-data-processor`](../services/api/adapters/http-data-processor/README.md) |
| Downstream Load-Forecasting processor | [`EvanL1/AetherEMS`](https://github.com/EvanL1/AetherEMS/tree/main/processors/load-forecasting) |
| AetherEMS tasks and fixtures | [`packs/energy/data-processing`](../packs/energy/data-processing/README.md) |

Tool-specific configuration should be a thin adapter over these files. It must
not become a second source of architectural truth.

The Rust descriptors and YAML policy are maintained as two checked
representations of one contract. A transport still exposes only operations it
explicitly registers from that catalog.
