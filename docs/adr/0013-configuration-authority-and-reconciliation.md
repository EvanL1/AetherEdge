# ADR-0013: Assign configuration authority and reconcile applied projections

## Status

Accepted and implemented on 2026-07-13.

## Context

Aether has several representations of configuration because its six services
are independent processes. SQLite rows describe a commissioned site, Pack and
runtime manifests describe immutable release artifacts, in-memory protocol
objects execute IO, service generations accelerate logical lookup, and SHM carries
live point and health state. Treating two representations as co-authoritative
creates restart-only behavior and permits a successful API response to hide a
stale runtime.

Physical protocol mapping and logical measurement/action routing are different
facts. A register or JSON path belongs to the IO adapter. An instance-to-point
route belongs to application semantics. Neither belongs in the SHM header, and
peripheral services must not query protocol mappings to reconstruct a live
topology.

## Decision

1. Authority is assigned per fact:

   | Fact | Authority | Rebuildable projections |
   |---|---|---|
   | commissioned channels, exact points, protocol mappings, logical routes | site SQLite | IO protocol runtimes, SHM manifests, service read generations and pinned subscription indexes |
   | current point and channel-health observations | committed point/health SHM pair | API responses, History, Uplink, alarm/rule evaluation, optional mirrors |
   | Pack contents, compatibility range, required capabilities | validated versioned Pack artifact | loaded models, knowledge, rules, mappings and evaluations |
   | executable service/features composition | checksummed runtime manifest artifact | installer layout and runtime validation |

   An external store may mirror live state but never becomes an authority.
2. Channel commands retain ADR-0011's revision, compare-and-set, validation,
   confirmation, and durable user-audit contract. Automatic repair invokes the
   same `ChannelReconciler` and lifecycle gates, but is not a user command and
   does not fabricate an actor or successful command audit record.
3. IO performs a synchronous reconciliation before its HTTP server accepts
   commands, then repeats on a bounded configurable interval. One SQLite read
   transaction fingerprints every channel definition and revision, all four
   point tables, and supported protocol-mapping tables. The projector derives
   point and health manifests from one SQLite snapshot. A second fingerprint
   after activation prevents a concurrent desired-state change from being
   reported as applied.
4. A point-layout change republishes both SHM planes with a fresh common epoch.
   A protocol-mapping-only change fences and rebuilds only the affected channel
   runtime. A logical routing-only change replaces routing/service-local
   generations without rebuilding physical SHM.
5. History and Uplink load exact configured physical points and only the
   logical routes they consume from one `SqliteLiveTopologySnapshot`. They bind
   that snapshot to a committed `ShmReadTopologyGeneration` and atomically
   replace the pair. They never read protocol mappings.
6. Any missing/invalid authority table, non-current SHM projection, degraded
   channel reconciliation, or desired-state race clears the applied
   fingerprint and explicitly fences the affected runtime. Commands therefore
   fail closed until a later cycle proves convergence. The prior coherent
   peripheral read generation is retained while a candidate is partial.
7. Logical routes and PointWatch subscriptions are copied only from one pinned
   service topology generation. Rule reload changes the rule set but cannot
   publish subscriptions until the host supplies that generation's typed
   measurement bindings and point manifest together. Periodic topology refresh
   and PointWatch reconciliation are recovery mechanisms, not alternate
   configuration owners.
8. Automation rules form a separate SQLite aggregate with the persistent
   `configuration_revisions.scope = 'automation_rules'` CAS head. Every typed
   rule mutation, including an externally requested reload, fences and advances
   that head in the same transaction as its durable rule write. After commit,
   scheduler cache and PointWatch subscriptions are rebuilt behind the
   subscription readiness gate. A PointWatch publication mismatch leaves hints
   gated while deterministic tick evaluation continues; a rule-cache reload
   failure stops scheduling fail-closed. Receipts retain the committed revision
   in either degraded state so clients must reconcile rather than retry.
9. Commissioned instances form a separate SQLite aggregate with the persistent
   `configuration_revisions.scope = 'instances'` CAS head. Create, combined
   rename/property update, single-property upsert/delete, and subtree deletion
   enter one typed `InstanceConfigurationApplication`. Every online command is
   authenticated, explicitly confirmed, and durably records attempted plus
   terminal audit state. Validation, expected-revision comparison, revision
   advance, and desired-state writes share one SQLite transaction. A committed
   receipt always returns the resulting revision; a failed post-commit name or
   routing projection reports `reconciliation_required` and is never presented
   as a retryable mutation failure.
   The globally catalogued `automation.instance.manage` capability is a
   high-risk, non-idempotent command with its own permission; it is present in
   the AI safety policy and signed runtime composition manifest.
10. `measurement_routing.instance_name` and `action_routing.instance_name` are
    redundant display projections, not binding identity. Instance rename
    updates both columns atomically with `instances.instance_name`, but does not
    advance `logical_routing`: `(instance_id, point_id)` and its physical target
    did not change. Subtree deletion first collects every descendant and rejects
    the whole command if any member is routed. It then deletes the complete set
    in one transaction; routing-integrity triggers remain the fail-closed second
    line of defense against a missed precheck or concurrent legacy writer.

## Compatibility and removal criteria

- The unused `point_mappings` compatibility table and its lazy Automation
  schema creation are removed. Logical bindings use the canonical
  measurement/action routing tables. MQTT/HTTP JSONPath mappings use the same
  point-owned inline `protocol_mappings` authority as every other protocol;
  schema migration v12 atomically validates and merges the former adapter
  table before dropping it. The four point tables already participate in the
  IO reconciliation fingerprint.
- Automation's shared mutable `RoutingCache` projection is removed. Commands,
  rule reads, HTTP reads, and PointWatch rebuilds consume the immutable
  `AutomationTopologyGeneration`; absence of that generation fails closed.
- Revisionless channel mutations remain the HTTP/application compatibility
  exception recorded by ADR-0011. The first-party Rust CLI and MCP catalog now
  require explicit channel revisions and cannot exercise that shim. New online
  configuration aggregates must expose typed revisions and CAS through the
  application boundary rather than adding direct handler writes.
- Instance HTTP mutations have no revisionless online compatibility path. They
  require the explicit `instances` revision returned by
  `GET /api/instances/revision`, authenticated credentials, and confirmation.
  The former direct `InstanceManager` create/rename/property/delete functions
  and generic routing mutators are removed from production and test builds.
  Lifecycle suites now enter `InstanceConfigurationApplication`; routing
  mutation suites enter the typed measurement/action applications. Read-only
  manager queries and validation remain. The architecture gate rejects
  restoration of direct helpers, including behind `#[cfg(test)]`.
- Rule HTTP requests without `expected_revision` temporarily use a named
  revisionless compatibility shim: the boundary reads the current
  `automation_rules` head and still submits a typed CAS command through the
  same authorization, confirmation, and audit path. This prevents concurrent
  commits but cannot detect edits made since the browser's earlier read, so it
  is not strict optimistic concurrency. The shim logs every use and may be
  removed only after the browser sends the revision exposed by rule GET ETags,
  distribution telemetry shows no shim use for one stability window, and the
  compatibility contract tests are replaced by mandatory-revision tests. Rust
  CLI/MCP clients use explicit revisions during this stage.
- The published Rust `RuleMutation` and `ActionRoutingMutation` enums, their
  revisionless constructors, legacy receipt constructors, and the two-state
  `RuleSchedulerRefreshStatus` remain 0.5 compatibility surfaces. First-party
  commands use `RevisionedRuleMutation` or `RevisionedActionRoutingMutation`;
  the SQLite adapters service an old `mutate` call by reading the current head
  and submitting the same CAS path. Like the HTTP shim, that path prevents two
  simultaneous commits from sharing a head but cannot detect an edit since an
  earlier client read. These Rust shims may be removed only in a major release,
  after all supported downstream crates use the revisioned methods, one
  stability window records no legacy calls, and the public-API compatibility
  baseline intentionally advances. New rule runtime states belong in
  `RuleRuntimeStatus` until that major release so exhaustive matches on the
  legacy scheduler enum remain valid.
- Offline `aether sync` remains the explicitly confirmed, service-stopped
  import path. It must not run concurrently with the online configuration
  owner. Before its site transaction commits, it advances the
  `logical_routing`, `automation_rules`, and `instances` heads together with
  the imported rows. Tokens issued before an offline import therefore remain
  stale after services restart instead of matching an ABA-equivalent revision.
- A future protocol-mapping table must be added to the closed fingerprint list
  and its conformance suite before an adapter may consume it.

## Consequences

### Positive

- Restart is no longer the normal mechanism for applying point or protocol
  mapping changes.
- A physical topology, logical route set, and protocol mapping cannot silently
  masquerade as one another.
- Failure receipts and runtime fencing expose desired/applied drift instead of
  returning false success.

### Negative

- IO periodically reads configuration metadata even when no facts changed.
- The closed mapping-table list must evolve with adapter schemas.
- Direct offline SQLite edits can be detected and reconciled but cannot carry a
  user identity; operator attribution remains the responsibility of the
  confirmed import workflow.

## Verification

```bash
cargo test -p aether-store-local --features sqlite-topology --tests
cargo test -p aether-io --test automatic_reconciliation_contract
cargo test -p aether-io --test channel_mutator_contract
cargo test -p aether-io --test shm_topology_projector_contract
cargo test -p aether-automation --test test_instance_configuration_boundary
cargo test -p aether-automation --test test_point_watch_topology_generation
cargo test -p aether-history --all-features
cargo test -p aether-uplink --all-features
./scripts/check-architecture.sh
```
