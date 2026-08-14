# AI evaluations

AI evaluations are scenario fixtures that verify capability discovery,
authorization, confirmation, audit, and failure behavior. They complement Rust
unit tests; they do not replace deterministic runtime tests.

Every high-risk command requires at least these cases:

- denied without permission;
- denied without required confirmation;
- accepted with permission and confirmation;
- audited on success and rejection;
- no direct state mutation when the adapter fails.

[`data-processing.yaml`](data-processing.yaml) records the Data Processing
query scenarios for permission, confirmation, mandatory audit, non-idempotent
execution, untrusted results, failure isolation, and the no-control boundary.
There is no separate eval runner: each scenario names the existing deterministic
Rust test and command that provide its evidence.
