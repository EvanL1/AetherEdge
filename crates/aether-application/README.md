# aether-application

Transport-neutral command and query use cases for the Aether edge kernel.

`EdgeApplication` and the governed channel, routing, rule, alarm, alert, and
device-control applications are shared by HTTP, CLI, MCP, and deterministic
runtime callers. Infrastructure choices remain outside this crate.

Every non-idempotent mutation authorizes and persists an `Attempted` audit event
before invoking its port. A terminal-audit failure after acceptance is returned
as non-retryable accepted degradation, never as an error that could execute the
operation twice.

```bash
cargo test -p aether-application
```

Licensed under either MIT or Apache-2.0, at your option.
