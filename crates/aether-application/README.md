# aether-application

Transport-neutral command and query use cases for the Aether edge kernel.

`EdgeApplication` is shared by CLI, MCP, and optional network transports. It
authorizes point reads, validates device commands, requires an audit sink, and
dispatches control through capability ports. Infrastructure choices stay
outside this crate, so its default graph contains no Redis, PostgreSQL, SQLx,
MQTT client, or web framework.

```bash
cargo test -p aether-application
```

Licensed under either MIT or Apache-2.0, at your option.
