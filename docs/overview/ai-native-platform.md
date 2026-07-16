# AI-native platform

AetherIoT is building an open runtime platform for agents to turn human intent
into governed, verifiable behavior across physical spaces.

The product promise is simple: people should describe the outcome they want
instead of learning device identifiers, trigger editors, condition trees, or
vendor-specific automation syntax.

```text
"Keep the bedroom comfortable tonight without wasting energy."
                              |
                              v
                  discover available capabilities
                              |
                              v
                    generate a typed proposal
                              |
                              v
                validate policy, risk, and conflicts
                              |
                              v
                 confirm when the contract requires it
                              |
                              v
                 commission deterministic edge behavior
                              |
                              v
                    observe, explain, and revise
```

The complete end-user conversational lifecycle is a product direction, not a
claim about the current beta. The beta supplies important foundations: a
deterministic edge runtime, machine-readable capabilities, governed commands,
agent-readable documentation, MCP surfaces, public interoperability contracts,
and cloud-side domain/application slices.

## Native, not attached

Adding a chat box to an existing automation editor is not enough. An AI-native
Aether system treats the agent as the primary configuration experience while
keeping model output outside physical authority.

| Conventional AI feature | Aether product direction |
| --- | --- |
| Translate one sentence into fields in an automation form | Maintain the complete lifecycle from intent through observation and revision |
| Give a model direct device tools | Compile model output into typed, policy-checked application commands and artifacts |
| Depend on the model for every event | Commission deterministic behavior that continues without the model |
| Hide generated configuration | Preserve versions, explanations, audit evidence, expiry, and rollback |
| Bind the experience to one model provider | Keep agent clients and model providers replaceable |

## One loop, three explicit authorities

```text
human intent
    |
    v
AetherCloud       context, desired state, governed jobs, and future agent lifecycle
    |
    v
AetherContracts   typed capabilities, messages, fixtures, and conformance evidence
    |
    v
AetherEdge        live state and deterministic physical execution
```

- **AetherCloud** is the evolving agent and control plane. It owns governed
  desired state and cloud-side jobs, but it does not own live physical state.
- **AetherContracts** is the language-neutral interoperability authority. Its
  current release covers the Thing Model and CloudLink foundation; future
  intent, proposal, policy, and automation contracts require their own
  specification and TCK before they can be claimed.
- **AetherEdge** is the deterministic execution runtime. AI is an application
  client, never part of acquisition, safety interlocks, or hard real-time loops.

No model receives a bypass around the application layer. Every exposed command
declares risk, permission, confirmation, idempotency, and audit policy, and the
edge may accept, reject, expire, or apply a requested change under local policy.

## Conversation replaces configuration screens

The target experience has five basic forms:

1. **Immediate action:** "Turn off the lights in empty rooms now."
2. **Persistent behavior:** "When a window stays open, stop heating that room."
3. **Temporary behavior:** "My parents are staying this week; keep a low
   hallway light at night."
4. **Explanation:** "Why did the ventilation start?"
5. **Reversal:** "Undo the energy policy created this morning."

No fixed configuration screen is required for these tasks. The system may
still generate an on-demand summary, simulation, risk explanation, or
confirmation surface when a person needs to understand a consequential change.
That surface explains the generated behavior; it is not a second configuration
authority.

## Generation is separate from execution

An agent may be creative and probabilistic while interpreting intent. Physical
execution must not be.

The intended pipeline is:

1. Discover the exact runtime, Pack, device, query, and command capabilities.
2. Resolve ambiguity and collect only the missing constraints.
3. Generate a typed proposal with scope, duration, expected revision, and
   rollback information.
4. Reject unknown capabilities, unsafe values, conflicting policy, stale state,
   and unauthorized commands.
5. Require confirmation according to the command contract.
6. Commission a versioned deterministic artifact or invoke a governed command.
7. Observe reported and applied state, preserve audit evidence, explain the
   outcome, and revise only through another governed change.

An LLM is therefore not called for every sensor event. Once commissioned,
AetherEdge continues the behavior locally even if the agent, model provider,
cloud, or internet is unavailable.

## Delivery status

**Available foundations:** AetherEdge runtime manifests, OpenAPI, agent-readable
Markdown, Agent Skill, read-only and explicitly gated MCP tools, governed
application commands, deterministic local rules, audit foundations, and the
current AetherContracts release evidence.

**Partial or experimental foundations:** AetherCloud's transport-neutral MCP
application interface, CloudLink, telemetry persistence, desired/reported/
applied deployment, governed jobs, and the end-to-end Edge/Cloud development
harnesses.

**Not yet delivered as a complete product:** the household semantic context,
end-user conversational agent, intent-to-policy compiler, historical
simulation, generated confirmation experience, automatic outcome evaluation,
and continuous adaptation loop.

See [platform status](../roadmap/status.md) for the component-level evidence,
[Build Applications with AI](../guides/build-applications-with-ai.md) for the
available development workflow, and [safe operations](../guides/safe-operations.md)
before enabling any write capability.
