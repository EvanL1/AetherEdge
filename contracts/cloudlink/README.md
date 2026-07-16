# Experimental CloudLink contracts

`v1/` is AetherIot's integration surface for the public AetherContracts
release. AetherContracts `v0.1.0-alpha.3`, pinned by the root consumer lock,
is shared authority. The local files are adopted byte copies, migration
history, or product proposals; they are not a second authority. Adoption is
partial and is not a production release.

The contract is transport neutral at the business layer. MQTT v3.1.1/QoS 1 is
the first binding, with application-level durable acknowledgements above MQTT.
See `docs/adr/0017-experimental-cloudlink-mqtt-edge-foundation.md`,
`docs/adr/0018-pinned-aethercontracts-consumption.md`, and
`docs/reference/cloudlink-mqtt-v1.md` for authority, migration, and compatibility
findings. `v1/MIGRATION.md`, `v1/wire-profile.json`, its fixture manifest, and
the interoperability files record product integration history and remaining
release work; they cannot override the public core.

Validate the self-contained schemas with an explicit local base URI:

```bash
cd contracts/cloudlink/v1
uvx check-jsonschema \
  --base-uri "file://$PWD/" \
  --schemafile telemetry-batch.schema.json \
  fixtures/telemetry-batch.valid.json
```
