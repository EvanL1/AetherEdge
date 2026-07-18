---
title: Configuration rollback and reconciliation
description: Recover a commissioned Edge configuration without creating a second authority or repeating an uncertain mutation.
---

# Configuration rollback and reconciliation

Use this runbook when an AetherEdge configuration change has committed but the
runtime projection is degraded, or when an operator must restore a previously
verified site configuration.

This is an operator-assisted recovery procedure. AetherEdge does not currently
provide a universal one-click rollback command. Site SQLite remains
authoritative for commissioned channels, points, protocol mappings, and logical
routes; source files are import input, not a second live authority.

## Safety conditions

- Stop issuing configuration commands. Never repeat a non-idempotent command
  merely because its response, terminal audit record, or runtime projection is
  incomplete.
- Preserve the `request_id`, expected and resulting revisions, audit evidence,
  service logs, and the last known-good configuration artifact.
- Fence or disable the affected channel through the governed application
  command only when its current revision is known. A physical safety procedure
  takes precedence over software recovery.
- Run offline `aether sync` only while all services that own online
  configuration are stopped.

## Recovery sequence

1. Read the current desired revision and runtime status. If the mutation
   committed, treat its resulting revision as fact even when reconciliation
   failed.
2. Identify an immutable, reviewed configuration artifact that was previously
   commissioned at this site. Do not reconstruct configuration from SHM,
   protocol sessions, or a cloud cache.
3. Stop the AetherEdge services that can read or mutate site configuration.
4. Back up the current database, configuration artifact, audit evidence, and
   runtime manifest before changing anything.
5. Restore the known-good database backup, or run the explicitly confirmed
   offline import from the known-good source set. Do not edit SQLite tables
   directly.
6. Start the runtime and allow IO reconciliation to build protocol sessions,
   point and health SHM, and service-local generations from one authoritative
   snapshot.
7. Verify the configuration revision heads, common SHM topology epoch, channel
   status, logical routes, and read-only point observations before enabling
   any physical command path.
8. Re-enable channels individually through revision-guarded, confirmed,
   audited application commands.

## Stop and escalate

Keep the affected runtime fenced and request human review when no known-good
artifact exists, authority tables are invalid, reconciliation continues to
report drift, the SHM pair does not share one committed epoch, or audit evidence
cannot determine whether the earlier mutation committed.

See the [configuration reference](../reference/configuration.md),
[agent operating boundaries](../guides/ai-assistants.md), and
[safe operations](../guides/safe-operations.md).
