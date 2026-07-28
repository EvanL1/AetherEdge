# ADR-0032: Retire zero-consumer runtime compatibility planes

## Status

Accepted and implemented on 2026-07-28.

## Context

After physical point topology moved offline, several runtime surfaces had no
production consumer but still preserved state, routes, and configuration:

- IO registered every channel command sender in both `ShmCommandListener` and
  an HTTP-era `CommandTxCache`, although no handler read the cache.
- The HTTP protocol adapter carried an unmounted webhook mode and a second
  background polling loop in addition to the channel task's polling lifecycle.
- Automation retained generic routing mutations beside canonical,
  revision-fenced per-point commands; several global delete aliases always
  returned an error, and an instance reload route bypassed the application
  command lifecycle.
- CloudLink MQTT exported an unused legacy/dual migration selector even though
  CloudLink has one native transport owner.
- The CLI listed legacy product CSV files from the current directory although
  runtime products come from validated Packs and configured site products.

Keeping these paths made the supported runtime look broader than its actual
composition and created places where a second owner could be restored.

## Decision

1. `ShmCommandListener` is the only channel-sender registry. IO removes the
   duplicate command cache and its router/composition parameters.
2. The in-tree HTTP device adapter is polling-only. The channel task owns its
   polling schedule. Incoming webhook hosting is not an IO runtime capability.
3. Automation routing mutations use only the canonical measurement/action
   point endpoints and their governed application commands. The aggregate
   instance routing route remains read-only.
4. Always-failing mixed-plane delete aliases and the direct instance-cache
   reload route are removed. Offline import is activated by supervised startup;
   online changes reconcile through their owning application command.
5. CloudLink MQTT exposes only its native v1 configuration. The unused
   legacy/dual selector is removed.
6. Product discovery uses the automation query backed by active Packs and the
   configured site directory; the current-working-directory CSV listing is
   removed.
7. Architecture tests prevent the retired modules and route symbols from
   returning.

## Consequences

The six-process composition, SHM command path, protocol polling, logical
C2M/M2C routing, SQLite desired state, and governed per-point routing commands
remain unchanged. The removed paths had no first-party production caller and
no successful runtime behavior. Downstream clients must use canonical query and
command endpoints rather than compatibility aliases.
