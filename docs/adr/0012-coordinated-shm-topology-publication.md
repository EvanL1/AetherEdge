# ADR-0012: Coordinate point and health SHM topology publication

## Status

Accepted and implemented on 2026-07-13. The ADR-0002 legacy SHM aggregate was
removed after the compatibility matrix and rolling-version contracts passed.

## Context

The IO process publishes live point state and channel health in two distinct
shared-memory files. Keeping those planes separate prevents connectivity from
changing the point layout or masquerading as a point value, but it also means
that two independent atomic renames do not form one atomic transaction.

IO already derives both manifests from one SQLite read transaction. It then
publishes the point generation followed by the health generation. Each file
has its own `writer_generation` and manifest hash, so a reader can reject an
obvious layout mismatch, but it cannot prove that two otherwise valid files
were committed by the same IO topology publication. An IO crash between the
renames can leave one canonical path on the new topology and the other on the
old topology.

`writer_generation` cannot serve as the cross-plane identity. It identifies
one mmap writer incarnation and is intentionally generated independently for
each file. Logical measurement/action routing also cannot serve as that
identity: route-only changes do not require either physical SHM file to be
rebuilt.

## Decision

1. The point and channel-health files remain separate authoritative SHM
   planes, with independent `writer_generation` values and manifest hashes.
2. The final eight bytes reserved in the v4 physical header carry an opaque
   `publication_epoch`. Epoch zero means an uncoordinated single-plane file;
   production IO dual-plane publication always uses a non-zero epoch. Reusing
   reserved bytes does not change the 64-byte layout or slot offsets, so the
   physical format version remains v4 during the rolling migration.
3. One IO topology projection allocates one epoch and writes it into both
   staged files. The epoch changes on every dual-plane publication, including
   IO restart and recovery from a partial publication. It does not change for
   routing-only or protocol-mapping-only configuration changes. Allocation
   happens while holding the topology publication lease and advances above the
   durable commit witness and both currently observable plane headers. The
   commit path independently rejects reuse of the current durable epoch, so a
   process-local clock or counter is never an epoch authority.
4. IO publishes a versioned topology commit record at a deterministic sidecar
   path derived from the canonical point path. The record contains the epoch,
   both manifest hashes, both slot counts, and both writer generations. It is
   written to a private staging file, flushed, and atomically renamed only
   after both canonical SHM renames and canonical reopens succeed.
5. A coordinated reader acquires the existing point and health authority
   leases in deterministic path order, opens both planes, reads the commit
   record, and accepts the pair only when every recorded identity matches both
   headers and both expected manifests. The published service view pins the
   epoch and both writer generations; a later reopen may not silently attach
   either source to a different identity. A missing, malformed, stale, or
   mismatched record is a retryable topology-transition failure. Composition
   hashes that disagree before physical IO is consulted remain permanent
   configuration errors.
6. The commit sidecar is a publication witness, not a data authority. SHM
   remains authoritative for live values, and SQLite remains authoritative for
   desired physical topology. Deleting or corrupting the witness makes the
   dual-plane view unavailable until IO republishes it; no reader falls back to
   Redis, SQLite values, or an uncoordinated pair.
7. A route-only change creates a new service-local logical topology generation
   pinned to the existing committed physical epoch. A protocol-mapping-only
   change reconciles the affected IO channel runtime and does not rebuild SHM
   when the physical point manifest is unchanged.
8. Existing v4 readers ignore the formerly reserved field and the new sidecar,
   so the supported rolling-upgrade order is IO first, followed by peripheral
   readers. This is binary compatibility only: an old reader does not gain the
   new cross-plane proof during the migration window. Coordinated readers do
   not accept epoch-zero files, so reader-first rollout or IO downgrade fails
   safely. Test-only and diagnostic single-plane constructors may retain epoch
   zero, but they cannot be composed into a production dual-plane read
   generation.
9. Publication code uses RAII cleanup for staging SHM and commit files and
   retains one composition-level exclusive topology lease across both
   per-plane replacement leases, canonical revalidation, and commit-record
   rename. Coordinated readers acquire the matching shared topology lease
   before their deterministically ordered per-plane leases. A process exit
   releases all authority locks automatically. Publication failures never
   report the desired topology as committed; reconciliation allocates a fresh
   epoch and republishes both planes rather than completing an ambiguous old
   transaction.
10. Service topology supervisors compare the committed physical epoch in
    addition to SQLite digests and manifest hashes. An IO restart with an
    unchanged layout still publishes a replacement service generation; point
    and health sources never reconnect independently across that boundary.

## Failure semantics

- A crash before either SHM rename normally leaves the previous commit record
  and both previous canonical files valid. If the process exits after marking
  a retained generation unstable but before its rename, the previous canonical
  file remains intentionally fenced; coordinated readers fail closed and the
  restarted IO process repairs it with a fresh epoch.
- A crash after exactly one SHM rename leaves the previous commit record, which
  cannot match the mixed canonical pair. Coordinated readers fail retryably.
- A crash after both SHM renames but before commit-record rename leaves two
  uncommitted canonical files. Coordinated readers fail retryably.
- A crash after commit-record rename leaves a complete, self-validating
  publication that readers may accept even before the restarted IO process has
  reconstructed its process-local handles.
- An unchanged projection is a true no-op only when the current commit record
  still proves both canonical planes. Otherwise reconciliation republishes both
  planes with a fresh epoch.

## Compatibility and removal criteria

The rolling compatibility matrix is deliberately asymmetric:

| IO writer | Peripheral reader | Result | Supported use |
|---|---|---|---|
| legacy v4 | legacy v4 | Reads one plane at a time; no common-epoch proof | Pre-migration baseline only |
| epoch-aware v4 | legacy v4 | Reads unchanged 64-byte v4 layout and ignores the former reserved bytes | Temporary IO-first rolling window |
| legacy v4 | coordinated reader | Rejected because epoch/commit proof is absent | Safe failure; reader-first rollout is forbidden |
| epoch-aware v4 | coordinated reader | Accepts only a matching committed point/health pair | Steady state |

`legacy_v4_reader_accepts_new_io_segment_during_io_first_rolling_upgrade`
is a frozen old-reader fixture: it knows only the former reserved bytes and
the v4 slot offset. `coordinated_reader_requires_a_committed_common_publication_epoch`
proves the inverse combination fails closed. Together they are the rolling
upgrade conformance gate; the cross-process stress suite covers replacement
and crash behavior after both sides are epoch-aware.

- The physical header remains 64 bytes and v4 readers continue to validate the
  same magic, version, slot offsets, and manifest hashes.
- Golden tests cover old-reader/new-IO operation during the supported IO-first
  rolling order.
- Epoch-zero single-plane constructors remain explicitly named diagnostic or
  test surfaces. Remove any implicit production use before declaring the
  migration complete.
- Compatibility routing caches and legacy layout indexes were removed after
  production readers moved to committed physical epochs and the old/new
  rolling conformance matrix passed. The architecture gate rejects restoring
  either the retired SHM aggregate or Automation's mutable routing projection.

## Consequences

### Positive

- A reader can prove that point and health came from one IO publication rather
  than inferring it from two independent hashes.
- Partial publication and restart recovery become observable and testable.
- Route-only and protocol-mapping-only changes no longer imply unnecessary SHM
  rebuilds.
- No external service or new live-state authority is introduced.

### Negative

- IO must manage a third small canonical file and republish both planes after
  any ambiguous partial failure.
- New coordinated readers require IO to have completed the epoch migration.
- The supported rolling-upgrade order is constrained to IO first.

## Verification

```bash
cargo test -p aether-dataplane --test mmap_validation
cargo test -p aether-shm-bridge --test read_topology_generation_contract
cargo test -p aether-shm-bridge --test topology_process_stress
cargo test -p aether-io --test shm_topology_projector_contract
cargo test -p aether-io --test automatic_reconciliation_contract
cargo test -p aether-shm-bridge --test runtime_generation_contract \
  --test channel_health_writer_handle_contract
cargo test -p aether-history --all-features
cargo test -p aether-uplink --all-features
./scripts/check-architecture.sh
```

The explicit ignored soak case is scheduled by
`.github/workflows/topology-soak.yml`; it repeats cross-process topology
switches, writer-process restarts, and concurrent read validation for thousands
of publication cycles. For every restart gap, the parent waits until a child
writer has published only the point plane, kills that process before health or
commit publication, proves both candidate topologies fail closed, and then
proves the next committed generation recovers. The Linux gate also caps each
reader's open descriptors and RSS, while every platform caps publication files
and rejects leftover staging objects.
