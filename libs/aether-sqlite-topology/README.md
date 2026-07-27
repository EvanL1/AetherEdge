# aether-sqlite-topology

Private service-composition adapter that reads one authoritative SQLite
transaction and derives coherent SHM point/health manifests plus logical
measurement and action routes.

The package exists so local storage does not select SHM implementation types
and rule libraries do not own concrete subscription bitmaps. Only service
composition roots should depend on it.

```bash
cargo test -p aether-sqlite-topology
```
