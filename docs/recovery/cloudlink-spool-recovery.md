---
title: CloudLink spool recovery
description: Recover a file-backed CloudLink spool without discarding unacknowledged facts or fabricating delivery evidence.
---

# CloudLink spool recovery

Use this runbook when the file-backed CloudLink spool cannot open, detects
corruption, remains locked, or cannot advance after reconnect.

The spool is authoritative for its stream epoch, positions, stable batch
identity and digest, publication evidence, durable application acknowledgement,
and bounded data-loss evidence. MQTT publication acknowledgement alone never
removes a record.

## Expected automatic recovery

After a process crash, the file adapter may truncate only an incomplete journal
tail. It then restores the last complete synchronized record. Exact duplicate
application acknowledgements are idempotent.

A checksum failure, semantic conflict, unsafe file ownership, symbolic link,
multiple hard link, or corruption in a complete record fails closed. The
adapter does not guess or silently skip that state.

## Operator recovery

1. Stop every process that may own the spool. Never break a live exclusive lock
   or start two publishers on one path.
2. Preserve byte-for-byte copies and digests of the spool, lock, cursor,
   challenge ledger, configuration, and relevant logs.
3. Check that the direct parent is private, files are owned by the runtime
   identity, and no symbolic or multiply linked path is involved.
4. For an incomplete-tail event, restart the same adapter once and verify that
   it reports recovery to the last complete record.
5. For checksum or semantic corruption, keep publication disabled. Restore a
   verified backup only when its stream epoch and position history belong to
   the same Gateway and stream.
6. If records are irrecoverable, use only the contract-defined, reviewed
   data-loss evidence flow. Never edit positions, digests, or acknowledgement
   state by hand.
7. After reconnect, verify that replay preserves each original stream
   position, batch identity, business digest, send time, and expiry. Remove a
   record only after an exact durable CloudLink acknowledgement.

Escalate when no verified backup or accepted loss-evidence path exists. Losing
telemetry is preferable to fabricating a contiguous durable acknowledgement.

See the [local store extension](../../extensions/store-local/README.md),
[CloudLink MQTT reference](../reference/cloudlink-mqtt-v1.md), and
[Gateway identity recovery](gateway-identity-recovery.md).
