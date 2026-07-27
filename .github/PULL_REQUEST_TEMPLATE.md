# Pull request

## Outcome

<!-- What problem does this solve, and what user-visible outcome changes? -->

## Scope

<!-- List affected crates/services/interfaces and intentionally excluded work. -->

## Architecture and safety

- [ ] The default runtime still works without Redis, PostgreSQL, or another
  external service.
- [ ] SHM remains the sole authority for live point state; external stores are
  optional mirrors or sinks.
- [ ] Process isolation and independent restart behavior are preserved.
- [ ] Core crates depend on ports, not concrete storage, protocol, or web
  implementations.
- [ ] Device commands remain deny-by-default, permission checked, confirmation
  aware, and audited.
- [ ] Deterministic acquisition and safety behavior does not depend on an AI client.
- [ ] Energy- or vendor-specific behavior stays in an optional pack or
  downstream integration where applicable.
- [ ] Not applicable items are explained below rather than silently ignored.

<!-- Explain non-applicable items and intentional boundary changes. -->

## Verification

<!-- Check commands actually run. Explain failures and skipped checks. -->

- [ ] Focused affected check(s)
- [ ] Changed YAML and shell files were parsed or linted where applicable.
- [ ] Architecture boundaries were checked when dependency direction,
      composition, or live-state authority changed.
- [ ] External-service tests are explicitly marked and excluded from the
      default local path.

Full workspace verification is provided by PR CI. Do not duplicate the full
CI suite locally unless preparing a release or investigating a CI-only failure.

Commands and results:

```text
List only the focused commands actually run locally.
```

## Tests, documentation, and migration

- [ ] Behavior tests were added or updated before implementation.
- [ ] New port implementations include conformance tests.
- [ ] Public documentation and examples were updated where needed.
- [ ] An ADR was added or updated for dependency direction, live-state
  authority, process topology, AI safety, or another durable decision.
- [ ] Breaking changes and operator migration steps are documented.

<!-- Link related issues/ADRs and describe any remaining risk or follow-up. -->
