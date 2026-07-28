# aether-testkit

Reusable conformance checks and deterministic test doubles for Aether adapter
authors.

The retained suites verify live-state round trips and ordered batch reads,
durable-outbox FIFO and acknowledgement behavior, and CloudLink transport
evidence without inventing a cloud application acknowledgement.

```bash
cargo test -p aether-testkit
```

Licensed under either MIT or Apache-2.0, at your option.
