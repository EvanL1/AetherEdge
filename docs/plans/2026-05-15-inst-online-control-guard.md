# Inst Online Control Guard

## Goal

Keep the existing four-remote model and instance data layout unchanged:

- `comsrv:{channel}:{T/S/C/A}`
- `inst:{id}:M`
- `inst:{id}:A`

Add a lightweight runtime online state and fail-closed control guard so an offline communication channel cannot continue controlling devices.

## Simple Design

Use sidecar runtime keys instead of adding new four-remote or instance point types:

```text
comsrv:online      channel_id -> 0/1
inst:online        inst_id -> 0/1
inst:online:ts     inst_id -> epoch_ms
```

`inst:online` is derived from routing and channel liveness, not from product point definitions.

## Flow

```text
com channel offline
  -> comsrv marks comsrv:online[channel_id] = 0
  -> related inst status is derived as offline
  -> modsrv/rules/UI stop accepting actions for offline inst
  -> comsrv rejects or drops pending C/A commands for the offline channel
```

When an action is requested:

```text
modsrv receives inst action
  -> resolve M2C target channel
  -> pre-check inst/channel online state
  -> write existing inst:{id}:A state as today
  -> dispatch command to comsrv
  -> comsrv performs final online + command freshness check before device write
```

## Guard Rules

- `inst:online` is a runtime status surface, not a new M/A point.
- `modsrv` may reject actions early when the target instance/channel is offline.
- `comsrv` is the final safety boundary: if target channel is offline, stale, or reconnecting, it must not call protocol `write_control` / `write_adjustment`.
- Commands created while offline must not be replayed after reconnect. Use command TTL or channel epoch if queueing remains buffered.
- UI and rules should treat `inst:online = 0` as not controllable.

## Open Decisions

- Multi-channel instance policy: `any_online`, `all_required_online`, or per-action target channel.
- Command expiry policy: fixed TTL or channel epoch invalidation.
- Whether `inst:online` is materialized in Redis by a background derivation task or computed on read from route + `comsrv:online`.
