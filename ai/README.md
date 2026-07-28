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

Optional forecasting, covariates, and other derived-data processors are not
kernel capabilities. They belong to downstream Packs such as
[`EvanL1/AetherEMS`](https://github.com/EvanL1/AetherEMS) and may consume only
published SDK/application boundaries. They cannot acquire SHM, configuration,
history-storage, credentials, or device-control authority from static docs.

Tool-specific configuration should be a thin adapter over these files. It must
not become a second source of architectural truth.

The Rust descriptors and YAML policy are maintained as two checked
representations of one contract. A transport still exposes only operations it
explicitly registers from that catalog.
