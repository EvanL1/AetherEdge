# aether-testkit

Reusable conformance checks and deterministic test doubles for Aether
extension authors.

The suites verify live-state round trips and ordered batch reads, FIFO and
acknowledgement behavior for durable outboxes, bounded/provenance-preserving
`HistoryQuery` projections, and exact descriptor/request/result correlation
for request-driven `DataProcessor` implementations. Processor conformance also
checks finite ordered forecast output and requires `unavailable` responses to
contain no derived output.

`MemoryCloudLinkTransport::pair` is the transport-neutral fake binding for
session/replay tests. It emits transport-published evidence for durable sends
but never invents a cloud application ACK, so tests must create the receipt
explicitly.

`ScriptedDataProcessor` is a queue-driven `DataProcessor` test double. It
reports a configurable health result, consumes queued `ProcessingResult` or
`PortError` values in FIFO order, and retains the complete
`DataProcessingRequest` values it received. Application, adapter, and example
tests can therefore prove both the exact frame sent to a processor and the
result/error path without a model runtime or network service.

Extension tests call the conformance helpers against concrete adapters so
capability semantics remain consistent across local and external boundaries.

```bash
cargo test -p aether-testkit
```

Licensed under either MIT or Apache-2.0, at your option.
