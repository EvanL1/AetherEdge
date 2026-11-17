# Product version compatibility

This matrix distinguishes released compatibility evidence from planned product
combinations. A green local test never upgrades an experimental public contract
to production status.

## Current tested baseline

| AetherEdge | AetherContracts | AetherCloud | Status | Evidence |
| --- | --- | --- | --- | --- |
| `v0.0.1` with the alpha.3 consumer | `v0.1.0-alpha.3` | Current unreleased alpha.3 consumer | Experimental integration baseline | Identical complete-consumer locks, 53 exact imports, no pending imports, and 25 shared fixture outcomes |
| Unreleased AetherEdge after `v0.0.1` with the alpha.4 consumer | `v0.1.0-alpha.4` | Current unreleased alpha.4 consumer | Experimental distribution baseline | Identical complete-consumer locks, 101 exact imports, and no pending imports; the Edge codec executes the 27 pinned CloudLink core fixtures. No dual-product alpha.4 evidence exists |
| `v0.0.1` legacy MQTT path | Not required for legacy wire | Existing legacy ingestion | Compatibility default | Existing product behavior; CloudLink does not silently reinterpret legacy topics |
| Future AetherEdge release | Future production contract release | Future production CloudLink release | Planned | Requires joint authentication, signed acknowledgement, crash durability, conformance, rollback, and elapsed support-window evidence |

The first two rows are distribution and fixture evidence. They are not
production transport, authentication, state-machine, or durability conformance.

## Naming compatibility

| Surface | Migration behavior |
| --- | --- |
| GitHub repository | `EvanL1/AetherIot` becomes `EvanL1/AetherEdge`; consumers should update remotes even while GitHub redirects old URLs |
| Rust SDK package | `aether-edge-sdk` remains unchanged |
| Rust import | `aether_sdk` remains unchanged |
| CLI and service binaries | `aether` and `aether-*` remain unchanged |
| Installer | `AetherEdge-<arch>-<version>.run` remains unchanged |
| Configuration and environment keys | Existing `aether` and `AETHER_*` identifiers remain unchanged |
| CloudLink and contract identifiers | Remain unchanged |
| AetherContracts alpha.3 artifacts | Remain byte-for-byte immutable and may retain the historical AetherIot name |

## Release rule

Every future product release should publish a compatibility row that pins exact
versions or commits and links to its executable evidence. Floating branches,
`latest`, and implied compatibility are not accepted evidence.
