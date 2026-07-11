# Add an extension

1. Identify the smallest existing port that expresses the required capability.
2. If no port fits, add a behavior test to `aether-ports` before changing the
   trait. Do not add vendor-specific methods.
3. Create the implementation under `extensions/` with all vendor dependencies
   local to that crate.
4. Run the matching `aether-testkit` conformance suite.
5. Add an example composition without changing SDK default features.
6. Update `ai/catalog.yaml` and generated capability/dependency indexes.
7. Run `./scripts/check-architecture.sh` and the workspace checks.
