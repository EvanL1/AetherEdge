# ADR-0042: Compose protocol adapters through a static registry

## Status

Accepted and implemented on 2026-08-20.

## Context

The IO runtime already used the object-safe `ChannelRuntime` boundary to run
heterogeneous physical protocols. Protocol selection was nevertheless repeated
in several concrete locations: channel validation maintained a feature-gated
protocol whitelist, `ChannelManager` matched protocol strings and called one
method per adapter, and factory helpers performed a second layer of concrete
construction. Adding one physical protocol therefore required editing generic
lifecycle and desired-state code in addition to adding the adapter.

AetherEdge must support additional communication protocols without making the
production runtime dynamically extensible. Production IO remains Rust-only,
loads no scripts or shared libraries, and runs only adapters explicitly chosen
by the binary composition. Adding an adapter is a build-time operation and
requires rebuilding and redistributing `aether-io` with a matching signed
runtime manifest.

## Decision

1. Introduce a service-local `ProtocolAdapterFactory` boundary. Each concrete
   adapter factory owns its stable protocol identifier, parameter validation,
   point-mapping interpretation, and construction of a `ChannelRuntime` plus
   its scheduling interval.
2. Compose the exact factory set in `compiled_protocol_registry`. Cargo
   features and target gates select the entries. Duplicate identifiers fail
   registry construction.
3. Inject the compiled registry into the production `ChannelManager` from the
   `aether-io` composition root. The manager resolves, validates, and builds a
   protocol through the registry and contains no concrete protocol branches.
4. Use the same injected registry for governed desired-state validation, so a
   channel cannot commit a protocol that the running binary cannot construct.
5. Keep channel identity, desired state, point types, lifecycle, command
   governance, logging, reconciliation, and SHM writes outside protocol
   factories. Factories receive only a read-only runtime configuration
   generation and return a protocol runtime; they receive no SQLite pool or SHM
   writer.
6. Keep the signed runtime manifest as the external catalog of a distribution's
   compiled capabilities. The in-process registry is executable composition,
   not a new HTTP discovery endpoint or independently mutable authority.
7. Do not add dynamic registration, runtime plugin discovery, `inventory`,
   shared-library loading, scripts, subprocesses, or an in-process simulation
   adapter. Adding a protocol requires Rust implementation, explicit static
   registration, tests, manifest alignment, and a rebuilt binary.

## Consequences

- A new protocol does not require another branch in `ChannelManager` or the
  channel mutator; generic lifecycle and safety behavior are reused.
- The supported set remains open to new statically linked adapters but closed
  for the lifetime of a running binary.
- Missing or duplicate protocol registrations fail closed instead of falling
  through to another adapter.
- Adapter-specific configuration remains typed and local to its adapter rather
  than expanding the industry-neutral domain or application contracts.
- Downstream distributions may statically compose additional pure-Rust
  adapters, but those adapters cannot bypass manager-owned command governance
  or write SHM directly.

## Verification

- A protocol-registry contract test compares registered identifiers with the
  active Cargo features and target gates.
- Unit tests reject duplicate registration and an unavailable protocol.
- Architecture checks require registry-based validation/building, explicit
  production injection, absence of concrete manager branches, and absence of
  dynamic loading mechanisms.
- IO all-feature tests continue to exercise every maintained concrete adapter.
