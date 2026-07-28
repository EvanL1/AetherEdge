# aether-routing

Storage-agnostic typed routing for the AetherEdge runtime.

The crate owns immutable C2M/M2C routing snapshots and the IO-owned typed C2C
index. It validates logical routes against the physical point manifest and
publishes deterministic generation digests. It does not depend on SQLx, execute
SQL, know local-store table names, parse configuration files, or transport live
data between processes.

The default distribution obtains commissioned route definitions through the
private `aether-store-local` adapter. Other compositions may build the same
`RoutingSnapshot` from another persistence adapter without changing routing or
service behavior.

```bash
cargo test -p aether-routing
cargo test -p aether-store-local --features sqlite-routing \
  --test sqlite_routing_contract --test sqlite_physical_topology_contract
```
