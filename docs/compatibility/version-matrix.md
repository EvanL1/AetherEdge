# Product version compatibility

This matrix distinguishes released compatibility evidence from planned product
combinations. A green local test never upgrades an experimental public contract
to production status.

## Current tested baseline

| AetherEdge | AetherContracts | AetherCloud | Status | Evidence |
| --- | --- | --- | --- | --- |
| `v0.0.1` with the alpha.3 consumer | `v0.1.0-alpha.3` | Current unreleased alpha.3 consumer | Experimental integration baseline | Identical complete-consumer locks, 53 exact imports, no pending imports, and 25 shared fixture outcomes |
| Future AetherEdge release | Future production contract release | Future production CloudLink release | Planned | Requires joint authentication, signed acknowledgement, crash durability, conformance, rollback, and elapsed support-window evidence |

The first row is distribution and fixture evidence. It is not production
transport, authentication, state-machine, or durability conformance.

## Release rule

Every future product release should publish a compatibility row that pins exact
versions or commits and links to its executable evidence. Floating branches,
`latest`, and implied compatibility are not accepted evidence.
