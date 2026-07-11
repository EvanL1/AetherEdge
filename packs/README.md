# Domain packs

Domain packs contain declarative models, protocol mappings, rules, knowledge,
and AI evaluations for one industry. They cannot add Rust dependencies to the
edge kernel.

An extension adds executable capability; a pack adds domain knowledge. For
example, a Modbus driver is an extension, while a PCS register mapping and SOC
control rule belong to the energy pack.

Official distributions may package one or more packs over a compatible Aether
release. AetherEMS is the reference energy distribution; see
[ADR-0007](../docs/adr/0007-aether-core-and-ems-distribution.md).
