# aether-postgres-history

Optional PostgreSQL implementation of Aether's append-only history capability.

The extension owns its SQL schema and parameterized writes. It depends only on
the public domain and port crates and is not part of the default SDK or edge
composition. Live state remains authoritative SHM; PostgreSQL stores history
only when the host explicitly selects this adapter.

```bash
cargo test -p aether-postgres-history
```

Licensed under either MIT or Apache-2.0, at your option.
