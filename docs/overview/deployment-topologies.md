# Deployment topologies

AetherIoT supports deployments that keep physical authority at the edge while
adding cloud coordination only where it creates value.

## Edge-only site

```text
field devices -> AetherEdge -> local applications
                    |
                    `-> embedded history and durable local outbox
```

Use this topology for evaluation, isolated sites, and applications that do not
need cloud coordination. The default runtime requires no Broker, PostgreSQL,
cloud account, browser, or AI client.

## Edge with AetherCloud

```text
field devices -> AetherEdge == CloudLink ==> AetherCloud -> operator clients
                    ^                         |
                    |                         `-> provider adapters
                    `--- final local policy        and governed jobs
```

AetherContracts defines the shared CloudLink behavior. AetherCloud records a
time-stamped projection and sends desired state or governed jobs. AetherEdge
validates, accepts, rejects, expires, or applies that intent under local policy.

The current CloudLink alpha path is experimental. The legacy edge uplink remains
the default until authentication, signed acknowledgement, crash durability, and
joint conformance gates pass.

## Multi-cloud control-plane cell

```text
tenant home cell
├── AetherCloud application modules
├── PostgreSQL transactional state
├── encrypted artifact storage
└── capability-driven provider adapters
    ├── provider A
    └── provider B
```

One deployment stack owns one independently locked infrastructure state. A cell
does not create a tenant-wide or cross-provider global state file. Each provider
continues to own its native resource state.

## AetherEMS solution

AetherEMS layers energy models, workflows, and applications over AetherEdge and
may connect to AetherCloud through public contracts. It cannot change the edge,
cloud, or contract authority boundaries.

## Failure expectations

| Failure | Required behavior |
| --- | --- |
| Internet or cloud unavailable | AetherEdge continues commissioned acquisition, local history, rules, alarms, interlocks, and control |
| Broker unavailable | Local durable outbox retains bounded work; MQTT delivery is not an application receipt |
| Provider API unavailable | AetherCloud exposes a typed failure and never normalizes it to an empty successful observation |
| AI client unavailable | Deterministic behavior is unchanged |
| Cloud job rejected by local policy | Rejection remains an auditable fact; cloud intent never bypasses the edge |
