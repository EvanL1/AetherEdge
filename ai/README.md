# AI-native assets

This directory contains vendor-neutral assets used by coding agents and by AI
operators of an Aether gateway.

- `catalog.yaml` tells an agent where a component lives and how to verify it.
- `invariants.md` lists rules that must survive every refactor.
- `safety-policy.yaml` is the source policy for generated capability metadata.
- `runbooks/` contains deterministic change procedures.
- `evals/` contains behavior and safety cases for AI-facing interfaces.

Tool-specific configuration should be a thin adapter over these files. It must
not become a second source of architectural truth.
