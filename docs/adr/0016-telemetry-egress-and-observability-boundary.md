# ADR-0016: Telemetry egress and the observability boundary

## Status

Accepted on 2026-07-14. The channel-connectivity egress, the bounded-cardinality
telemetry contract, and W3C trace-context propagation are implemented, with
telemetry off by default pending a broker policy that grants the topic.

Acquisition counters (reconnect attempts, decode errors) and history backpressure
depth are explicitly deferred: they are not readable outside their owning process
today and require a new loopback dependency, which this ADR does not authorise. A
per-class outbox quota is likewise deferred.

## Context

`docs/concepts/data-model.md` splits runtime state into four orthogonal datasets,
each with exactly one writer. Three of them can leave the gateway:

| Dataset | Egress |
| --- | --- |
| Instance current values | uplink `property` topic, history store |
| Alarm events | uplink `alarm` topic |
| Routing configuration | uplink `inst-sync` topic |
| **Channel connectivity** | **none** |

The connectivity dataset has no egress. This is a direct consequence of the purity
rule: an instance deliberately carries no `online` field, because online-ness is a
property of a communication channel, not of a device. The rule is correct, but the
fact it exiles to the channel-health SHM segment was never given a way off the box.

The operational consequence is that the most basic question an IoT operator asks —
*is my equipment reachable right now, and which sites are degraded?* — is
unanswerable for a fleet. It can only be answered one gateway at a time, by an
engineer who can already reach that gateway.

Meanwhile the runtime accumulated three disconnected, hand-rolled metric surfaces:
io's `/health` (component health plus a CPU percentage), history's `/hisApi/metrics`
(a bespoke `{success, data}` envelope), and uplink's `SystemMetrics`. They share no
naming, no types, and no aggregation semantics, and nothing consumes them.

Adopting OpenTelemetry was considered. The analysis separated three questions that
are usually collapsed into one:

- **Instrumentation API** — how code emits signals. `tracing` already fills this.
- **Data model** — what a metric *means*: temporality, reset detection, mergeable
  histograms, attribute conventions. This is genuinely hard to get right and is
  the part the runtime lacks.
- **Export protocol** — OTLP, Prometheus exposition, or MQTT.

The data model is worth adopting. The export protocol is not: an OTLP exporter on
the gateway would duplicate infrastructure the runtime already has and does it
worse. `aether-uplink` already owns an authenticated, TLS-protected MQTT channel
with a crash-durable `FileOutbox` behind it, which survives the multi-hour
disconnections that edge deployments treat as normal. OTLP exporters buffer in
memory and drop on overflow — they would lose telemetry precisely when the network
is degraded, which is when the telemetry matters most. A second egress path would
also mean a second firewall exception, a second credential, and a second failure
mode, and would pull a gRPC stack into an ARM gateway binary.

## Decision

1. **Two data classes, never mixed.** *Operational data* (point values, alarm
   events) is the plant's own record: unbounded cardinality, retained for years,
   queried for reports. It flows to history and to the `property` / `alarm` topics.
   *Ops telemetry* is metadata about the acquisition path itself: bounded
   cardinality, retained for weeks, read by engineers. It flows to a new
   `telemetry` topic. **Point values must never be emitted as telemetry metrics.**
   Doing so would explode metric cardinality, and an observability backend is not a
   historian: it cannot satisfy the retention, range-query, or backfill
   requirements that plant data has.

2. **The gateway borrows OpenTelemetry's data model and none of its transport.**
   No crate under `crates/`, `services/`, or `extensions/` may depend on
   `opentelemetry*`, and no service speaks OTLP. Telemetry is encoded as JSON whose
   shape mirrors the OTel metrics model — resource attributes, named metrics, typed
   points, explicit units — and is published through the existing MQTT channel and
   durable outbox. Terminating that stream into OTLP is a cloud-side concern and is
   out of scope for this repository. A consumer that wants Prometheus, InfluxDB, or
   a SCADA historian instead is equally well served; the gateway does not presume.

3. **Telemetry cardinality is bounded by construction.** Per-channel metrics are
   permitted: channel count is bounded by configuration and is small. Per-point
   metrics are forbidden: point count is unbounded and reaches tens of thousands.
   A per-point condition that matters is an alarm rule, not a metric; if it must be
   observed in aggregate, it is emitted as a per-channel count.

4. **Counters are cumulative, never delta.** MQTT QoS1 is at-least-once: duplicate
   delivery is guaranteed, not exceptional. A delta counter double-counts on replay
   and forces the consumer to deduplicate, which makes the consumer stateful. A
   cumulative counter is idempotent under replay and reordering, which keeps the
   consumer a stateless mapper. Cumulative counters carry the emitting process's
   start time so a consumer can detect a restart instead of rendering a negative
   step.

5. **W3C trace context is propagated through the MQTT envelope.** Cloud-issued
   `read` / `write` / `call-*` requests cross into the gateway and fan out across
   loopback services. The gateway accepts an inbound `traceparent`, attaches it to
   its `tracing` spans, echoes it on the reply, and forwards it on the loopback hop
   to automation. The gateway does not create trace identifiers it cannot honour and
   does not export spans; it only preserves causality that a caller established, so
   the cloud can attribute latency to a hop instead of to the gateway as a whole.
   This is an envelope-format commitment: adding it once devices are fielded would
   be a breaking change, so it is made now, when it is free.

   The value is untrusted input that reaches an outbound HTTP header on an
   authenticated control-plane request, so it is parsed against the strict W3C
   grammar rather than passed through, and the parsed type is the only way to
   construct one. A malformed value is dropped, never rejected: refusing the body
   would make observability an availability dependency of device actuation.

6. **Telemetry is off by default and never breaks an existing egress path.**
   `telemetry/{productSN}/{deviceSN}` is a topic no fielded broker policy grants.
   A broker that answers an unauthorised PUBLISH by closing the connection — AWS
   IoT Core does — would turn a silently-enabled telemetry publisher into a
   reconnect loop that takes property and alarm egress down with it, and the
   durable outbox would not even surface it, because delivery is acknowledged when
   the client accepts the publish rather than when the broker does. Enabling
   telemetry is therefore an explicit operator act, taken after the broker policy
   allows the topic. The same principle as clause 5, one layer out: an
   observability feature may not become an availability dependency of the plant.

## Consequences

- The connectivity dataset gains its egress. Per-channel online state and time in
  state leave the gateway on the `telemetry` topic and inherit the outbox's
  store-and-forward guarantee, so a gateway that was offline reports what it
  observed while it was offline.
- The gateway takes on no new dependency and no binary growth. Telemetry reuses
  the SHM health reader that uplink already constructs and the outbox it already
  drains. `scripts/check-architecture.sh` fails the build on an `opentelemetry`
  dependency, so clause 2 is enforced rather than merely asserted.
- A consumer of the `telemetry` topic is stateless. This is bought entirely by
  clauses 4 and 5, and is the reason the OTel data model was worth borrowing.
- **A failed observation is never published as an observation.** A health-plane
  read can fail — a topology republication conflicts, io is mid-restart — and that
  says nothing about the plant. It is reported as `aether.channel.health_read_errors`,
  never folded into `aether.channel.unobserved`; otherwise every republication
  would read as a plant going dark. The three outcomes are distinct in the type
  that models a sample, so they cannot be conflated by a later edit.
- **`aether.channel.state.duration_ms` measures time in state, not sample age.**
  The health plane is edge-triggered: io writes only on a transition, so the stored
  timestamp is the last state change. A short duration on a channel that keeps
  reappearing is what flapping looks like. Two caveats for a consumer: an io restart
  rewrites the plane with a fresh timestamp, so the duration resets to zero with no
  real transition; and `docs/concepts/data-model.md` calls this dataset "state and
  heartbeat", where the heartbeat is a separate segment-header field, not this
  timestamp.
- **Telemetry shares the bounded durable outbox with operational data.** The outbox
  rejects at capacity rather than evicting, so there is no unbounded disk growth,
  but during a long offline window telemetry competes for slots with property and
  alarm messages — the very data class clause 1 exists to protect. This is tolerable
  only because telemetry is opt-in (clause 6) and its interval is operator-set. An
  operator enabling it on a gateway with a long expected offline window should size
  `AETHER_UPLINK_OUTBOX_CAPACITY` accordingly. A per-class quota is the correct fix
  and is deferred.
- Acquisition counters — reconnect attempts, decode errors — remain invisible.
  `ReconnectStats` and `ChannelStats` live in io's process memory, are absent from
  the health SHM segment, and are not on io's HTTP API. Exposing them requires
  either widening the health slot encoding or adding a loopback io endpoint plus a
  uplink→io dependency that does not exist today. Both are real changes to the
  dependency graph and are deferred to their own ADR.
- `SystemMetrics` currently rides the `property` topic as a `PropertyEntry` with
  `source = "gateway"`, which is precisely the mixing clause 1 forbids. It is left
  in place: moving it would break existing cloud consumers. It is a known debt to be
  retired when the `telemetry` topic has a consumer, and no new ops signal may be
  added to the `property` topic in the meantime.
- The three ad-hoc metric surfaces are not removed by this ADR. They keep serving
  local, single-gateway diagnosis, which stays necessary precisely because a broken
  uplink is the case where cloud telemetry is dark by definition.
