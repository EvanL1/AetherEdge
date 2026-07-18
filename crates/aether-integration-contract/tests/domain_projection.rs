use aether_domain::{
    AreaId, AreaRecord, DeviceId, DeviceRecord, EntityId, EntityPointDescriptor, EntityRecord,
    ExternalAlias, GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationPointKey,
    IntegrationPointKind, IntegrationTopologySnapshot, ObservedValue, ObservedValueType,
    SnapshotDigest, TimestampMs, TopologyGeneration,
};
use aether_integration_contract::{
    AETHER_CONTRACTS_RELEASE, HomeAssistantV1Alpha1Profile, IntegrationContractCodec,
};
use serde_json::{Value, json};

fn descriptor(
    key: &str,
    kind: IntegrationPointKind,
    value_type: ObservedValueType,
) -> EntityPointDescriptor {
    EntityPointDescriptor::new(
        IntegrationPointKey::new(key).expect("valid point key"),
        key.replace('_', " "),
        kind,
        value_type,
        None,
    )
    .expect("valid descriptor")
}

fn entity(
    id: &str,
    source_address: &str,
    entity_kind: &str,
    points: Vec<EntityPointDescriptor>,
) -> EntityRecord {
    EntityRecord::new(
        EntityId::new(id).expect("valid entity id"),
        id.replace('-', " "),
        entity_kind,
        points,
        Some(DeviceId::new("device-1").expect("valid device id")),
        None,
        vec![
            ExternalAlias::new("home-assistant", "entity-id", source_address).expect("valid alias"),
        ],
    )
    .expect("valid entity")
}

fn topology() -> IntegrationTopologySnapshot {
    let area = AreaRecord::new(
        AreaId::new("living-room").expect("valid area id"),
        "Living room",
        vec![],
    )
    .expect("valid area");
    let device = DeviceRecord::new(
        DeviceId::new("device-1").expect("valid device id"),
        "Living room devices",
        Some(area.id().clone()),
        vec![],
    )
    .expect("valid device");
    let climate = entity(
        "climate-registry",
        "climate.living_room",
        "climate",
        vec![
            descriptor(
                "state",
                IntegrationPointKind::State,
                ObservedValueType::Enum,
            ),
            descriptor(
                "current_temperature",
                IntegrationPointKind::Attribute,
                ObservedValueType::Float64,
            ),
            descriptor(
                "temperature",
                IntegrationPointKind::Attribute,
                ObservedValueType::Float64,
            ),
        ],
    );
    let event = entity(
        "doorbell-event",
        "event.doorbell",
        "event",
        vec![descriptor(
            "event_type",
            IntegrationPointKind::Attribute,
            ObservedValueType::Enum,
        )],
    );

    IntegrationTopologySnapshot::new(
        GatewayIdentity::new("gateway-home").expect("valid gateway id"),
        IntegrationId::new("home-assistant.home").expect("valid integration id"),
        TopologyGeneration::new(9).expect("valid generation"),
        TimestampMs::new(1_784_217_600_000),
        SnapshotDigest::new(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("valid digest"),
        vec![area],
        vec![device],
        vec![climate, event],
    )
    .expect("valid topology")
}

#[test]
fn topology_projection_is_closed_and_uses_semantic_point_kinds() {
    assert_eq!(AETHER_CONTRACTS_RELEASE, "0.1.0-alpha.4");

    let topology = topology();
    let wire =
        IntegrationContractCodec::topology_from_domain(&topology, &HomeAssistantV1Alpha1Profile)
            .expect("topology projects");
    let encoded = IntegrationContractCodec::encode_topology(&wire).expect("topology encodes");
    let json: Value = serde_json::from_slice(&encoded).expect("encoded topology is JSON");

    assert_eq!(
        json["schema"],
        "aether.integration.topology-snapshot.v1alpha1"
    );
    assert_eq!(json["integration_kind"], "home-assistant");
    assert_eq!(json["snapshot_generation"], "9");
    assert_eq!(json["entities"][0]["source_address"], "climate.living_room");
    assert_eq!(json["entities"][0]["points"][0]["value_type"], "string");
    assert_eq!(json["entities"][0]["points"][0]["kind"], "status");
    assert_eq!(
        json["entities"][0]["points"][1]["kind"], "telemetry",
        "current temperature is measurement telemetry"
    );
    assert_eq!(
        json["entities"][0]["points"][2]["kind"], "status",
        "temperature setpoint is status, despite both points being attributes"
    );
    assert_eq!(json["entities"][1]["points"][0]["kind"], "event");
    assert!(json.get("gateway_id").is_none());
    assert!(json.get("digest").is_none());
}

#[test]
fn observation_projection_uses_exact_lossless_wire_encodings() {
    let topology = topology();
    let climate = &topology.entities()[0];
    let observations = vec![
        IntegrationObservation::available(
            topology.gateway_id().clone(),
            topology.integration_id().clone(),
            climate.id().clone(),
            IntegrationPointKey::new("state").expect("point key"),
            ObservedValue::enumeration("heat").expect("enum"),
            TimestampMs::new(1_784_217_600_001),
            1,
            Some("must-not-cross-public-boundary"),
        )
        .expect("observation"),
        IntegrationObservation::available(
            topology.gateway_id().clone(),
            topology.integration_id().clone(),
            climate.id().clone(),
            IntegrationPointKey::new("current_temperature").expect("point key"),
            ObservedValue::float64(23.5).expect("float"),
            TimestampMs::new(1_784_217_600_002),
            2,
            None,
        )
        .expect("observation"),
    ];

    let batch = IntegrationContractCodec::observation_batch_from_domain(
        &topology,
        "batch-0001",
        &observations,
    )
    .expect("batch projects");
    let wire_topology =
        IntegrationContractCodec::topology_from_domain(&topology, &HomeAssistantV1Alpha1Profile)
            .expect("topology projects");
    let encoded = IntegrationContractCodec::encode_observation_batch(&batch, &wire_topology)
        .expect("batch encodes");
    let json: Value = serde_json::from_slice(&encoded).expect("encoded batch is JSON");

    assert_eq!(json["snapshot_generation"], "9");
    assert_eq!(
        json["observations"][0]["value"],
        json!({
            "type": "string",
            "value": "heat"
        })
    );
    assert_eq!(
        json["observations"][1]["value"],
        json!({
            "type": "float64",
            "value": 23.5
        })
    );
    assert!(json["observations"][0].get("source_context").is_none());
    assert!(json["observations"][0].get("sequence").is_none());
}

#[test]
fn boundary_rejects_non_contract_identifiers_and_foundation_unsafe_floats() {
    let mut topology = topology();
    let observations = vec![
        IntegrationObservation::available(
            topology.gateway_id().clone(),
            topology.integration_id().clone(),
            topology.entities()[0].id().clone(),
            IntegrationPointKey::new("current_temperature").expect("point key"),
            ObservedValue::float64(1.5e20).expect("finite runtime value"),
            TimestampMs::new(1),
            1,
            None,
        )
        .expect("runtime accepts finite value"),
    ];

    let error = IntegrationContractCodec::observation_batch_from_domain(
        &topology,
        "batch-unsafe-float",
        &observations,
    )
    .expect_err("Foundation unsafe float must fail");
    assert_eq!(error.code().as_str(), "JSON_UNSAFE_NUMBER");

    topology = IntegrationTopologySnapshot::new(
        GatewayIdentity::new("gateway-home").expect("valid gateway"),
        IntegrationId::new("runtime permits spaces").expect("runtime identifier"),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(1),
        SnapshotDigest::new(
            "sha256:1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest"),
        vec![],
        vec![],
        vec![],
    )
    .expect("runtime topology");
    let error =
        IntegrationContractCodec::topology_from_domain(&topology, &HomeAssistantV1Alpha1Profile)
            .expect_err("wire identifier must be strict");
    assert_eq!(error.code().as_str(), "IDENTIFIER_INVALID");
}

#[test]
fn every_lossless_scalar_uses_the_frozen_public_representation() {
    let entity = entity(
        "diagnostic-registry",
        "sensor.integration_diagnostic",
        "sensor",
        vec![
            descriptor(
                "signed_counter",
                IntegrationPointKind::State,
                ObservedValueType::Int64,
            ),
            descriptor(
                "unsigned_counter",
                IntegrationPointKind::State,
                ObservedValueType::UInt64,
            ),
            descriptor(
                "precise_value",
                IntegrationPointKind::State,
                ObservedValueType::Decimal,
            ),
            descriptor(
                "text_value",
                IntegrationPointKind::State,
                ObservedValueType::String,
            ),
            descriptor(
                "raw_payload",
                IntegrationPointKind::Event,
                ObservedValueType::Bytes,
            ),
        ],
    );
    let topology = IntegrationTopologySnapshot::new(
        GatewayIdentity::new("gateway-home").expect("gateway"),
        IntegrationId::new("home-assistant.home").expect("integration"),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(10),
        SnapshotDigest::new(
            "sha256:2123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest"),
        vec![],
        vec![
            DeviceRecord::new(
                DeviceId::new("device-1").expect("device"),
                "Diagnostic device",
                None,
                vec![],
            )
            .expect("device"),
        ],
        vec![entity],
    )
    .expect("topology");
    let values = [
        ("signed_counter", ObservedValue::int64(i64::MIN)),
        ("unsigned_counter", ObservedValue::uint64(u64::MAX)),
        (
            "precise_value",
            ObservedValue::decimal("999999999999999999.125").expect("decimal"),
        ),
        (
            "text_value",
            ObservedValue::string("Aether").expect("string"),
        ),
        ("raw_payload", ObservedValue::bytes(vec![0, 1, 2, 3, 4, 5])),
    ];
    let observations = values
        .into_iter()
        .enumerate()
        .map(|(index, (key, value))| {
            IntegrationObservation::available(
                topology.gateway_id().clone(),
                topology.integration_id().clone(),
                topology.entities()[0].id().clone(),
                IntegrationPointKey::new(key).expect("point key"),
                value,
                TimestampMs::new(20 + index as u64),
                1 + index as u64,
                None,
            )
            .expect("observation")
        })
        .collect::<Vec<_>>();

    let batch = IntegrationContractCodec::observation_batch_from_domain(
        &topology,
        "batch-scalars",
        &observations,
    )
    .expect("batch");
    let wire_topology =
        IntegrationContractCodec::topology_from_domain(&topology, &HomeAssistantV1Alpha1Profile)
            .expect("wire topology");
    let encoded = IntegrationContractCodec::encode_observation_batch(&batch, &wire_topology)
        .expect("encoded batch");
    let json: Value = serde_json::from_slice(&encoded).expect("JSON");

    assert_eq!(
        json["observations"][0]["value"],
        json!({"type":"int64","value":"-9223372036854775808"})
    );
    assert_eq!(
        json["observations"][1]["value"],
        json!({"type":"uint64","value":"18446744073709551615"})
    );
    assert_eq!(
        json["observations"][2]["value"],
        json!({"type":"decimal","value":"999999999999999999.125"})
    );
    assert_eq!(
        json["observations"][3]["value"],
        json!({"type":"string","value":"Aether"})
    );
    assert_eq!(
        json["observations"][4]["value"],
        json!({"type":"bytes","encoding":"base64url","value":"AAECAwQF"})
    );
}

#[test]
fn wider_runtime_values_fail_closed_at_the_public_boundary() {
    let topology = topology();
    let entity = &topology.entities()[0];
    for (batch_id, key, value, expected_code) in [
        (
            "batch-decimal",
            "state",
            ObservedValue::decimal("1.2300").expect("runtime decimal"),
            "VALUE_ENCODING_INVALID",
        ),
        (
            "batch-string",
            "state",
            ObservedValue::string("x".repeat(4_097)).expect("runtime string"),
            "FIELD_BOUND",
        ),
        (
            "batch-bytes",
            "state",
            ObservedValue::bytes(vec![0; 12_289]),
            "FIELD_BOUND",
        ),
    ] {
        let observation = IntegrationObservation::available(
            topology.gateway_id().clone(),
            topology.integration_id().clone(),
            entity.id().clone(),
            IntegrationPointKey::new(key).expect("point key"),
            value,
            TimestampMs::new(1),
            1,
            None,
        )
        .expect("runtime observation");
        let error = IntegrationContractCodec::observation_batch_from_domain(
            &topology,
            batch_id,
            &[observation],
        )
        .expect_err("public boundary must reject");
        assert_eq!(error.code().as_str(), expected_code, "{batch_id}");
    }
}
