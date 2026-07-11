# aether-testkit

Reusable conformance checks for Aether extension authors.

The current suites verify live-state round trips and ordered batch reads, plus
FIFO and acknowledgement behavior for durable outboxes. Extension tests call
the checks against their concrete port implementations so capability
semantics remain consistent across local and external adapters.

```bash
cargo test -p aether-testkit
```

Licensed under either MIT or Apache-2.0, at your option.
