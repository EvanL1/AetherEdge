# Aether SunSpec extension

Optional compile-time SunSpec model catalog and point-expansion support for
AetherEdge Modbus compositions.

The default AetherEdge runtime does not compile this crate. Composition roots
opt in through the `aether-io/sunspec` feature. The extension contains no
transport implementation; discovery uses the Modbus adapter selected by
`aether-io`.

Vendored model JSON and its upstream Apache-2.0 license are under `models/`.
