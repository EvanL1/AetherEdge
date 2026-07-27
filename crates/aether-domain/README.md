# aether-domain

Industry-neutral, `no_std` domain types for the Aether edge kernel.

This crate defines point addresses and samples, strongly typed identifiers,
quality states, timestamps, validated control commands, and the Aether Data
Processing contract. The processing model separates application-side
`ProcessTaskRequest` from a complete processor-side `DataProcessingRequest`,
and treats `ProcessingResult` as untrusted until accepted as `DerivedData`.

The crate remains `no_std`; owned processing frames use `alloc` collections.
It has no async runtime, database, network, service, model framework, or
hardware dependency.

Use it when implementing an Aether host, downstream integration, protocol
adapter, or firmware component that needs to exchange stable edge-domain
values.

```bash
cargo test -p aether-domain
cargo tree -p aether-domain --edges normal
```

Licensed under either MIT or Apache-2.0, at your option.
