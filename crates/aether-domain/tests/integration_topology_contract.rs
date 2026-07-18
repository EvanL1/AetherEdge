use aether_domain::{
    AreaId, AreaRecord, DeviceId, DeviceRecord, DomainError, EntityId, EntityPointDescriptor,
    EntityRecord, ExternalAlias, GatewayIdentity, IntegrationId, IntegrationObservation,
    IntegrationPointKey, IntegrationPointKind, IntegrationSnapshot, IntegrationStateQuality,
    IntegrationTopologySnapshot, ObservedValue, ObservedValueType, SnapshotDigest, TimestampMs,
    TopologyGeneration,
};

fn gateway_id() -> GatewayIdentity {
    GatewayIdentity::new("gateway-home").expect("gateway identity is valid")
}

fn integration_id() -> IntegrationId {
    IntegrationId::new("home-assistant-home").expect("integration id is valid")
}

fn area() -> AreaRecord {
    AreaRecord::new(
        AreaId::new("kitchen").expect("area id is valid"),
        "Kitchen",
        vec![
            ExternalAlias::new("home-assistant", "area-id", "kitchen")
                .expect("area alias is valid"),
        ],
    )
    .expect("area is valid")
}

fn device() -> DeviceRecord {
    DeviceRecord::new(
        DeviceId::new("device-registry-42").expect("device id is valid"),
        "Kitchen lamp",
        Some(area().id().clone()),
        vec![
            ExternalAlias::new("home-assistant", "device-id", "device-registry-42")
                .expect("device alias is valid"),
        ],
    )
    .expect("device is valid")
}

fn entity() -> EntityRecord {
    let state = EntityPointDescriptor::new(
        IntegrationPointKey::new("state").expect("point key is valid"),
        "Power state",
        IntegrationPointKind::State,
        ObservedValueType::Boolean,
        None,
    )
    .expect("state point is valid");
    let brightness = EntityPointDescriptor::new(
        IntegrationPointKey::new("brightness").expect("point key is valid"),
        "Brightness",
        IntegrationPointKind::Attribute,
        ObservedValueType::UInt64,
        Some("%"),
    )
    .expect("brightness point is valid");
    EntityRecord::new(
        EntityId::new("entity-registry-17").expect("entity id is valid"),
        "Kitchen lamp",
        "light",
        vec![state, brightness],
        Some(device().id().clone()),
        Some(area().id().clone()),
        vec![
            ExternalAlias::new("home-assistant", "entity-id", "light.kitchen")
                .expect("entity alias is valid"),
        ],
    )
    .expect("entity is valid")
}

fn topology() -> IntegrationTopologySnapshot {
    IntegrationTopologySnapshot::new(
        gateway_id(),
        integration_id(),
        TopologyGeneration::new(7).expect("generation is valid"),
        TimestampMs::new(1_720_000_000_000),
        SnapshotDigest::new(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest is valid"),
        vec![area()],
        vec![device()],
        vec![entity()],
    )
    .expect("topology is internally consistent")
}

#[test]
fn topology_scopes_stable_registry_identity_to_gateway_and_integration() {
    let topology = topology();
    let entity = &topology.entities()[0];

    assert_eq!(topology.gateway_id(), &gateway_id());
    assert_eq!(topology.integration_id(), &integration_id());
    assert_eq!(topology.generation().get(), 7);
    assert!(topology.is_complete());
    assert_eq!(entity.id().as_str(), "entity-registry-17");
    assert_eq!(entity.points().len(), 2);
    assert_eq!(entity.points()[1].key().as_str(), "brightness");
    assert_eq!(
        entity
            .aliases()
            .iter()
            .find(|alias| alias.kind() == "entity-id")
            .map(ExternalAlias::value),
        Some("light.kitchen")
    );
    assert_eq!(
        entity
            .effective_area_id(topology.devices())
            .map(AreaId::as_str),
        Some("kitchen")
    );
}

#[test]
fn topology_rejects_duplicate_ids_and_dangling_references() {
    let point = EntityPointDescriptor::new(
        IntegrationPointKey::new("state").expect("point key"),
        "State",
        IntegrationPointKind::State,
        ObservedValueType::Boolean,
        None,
    )
    .expect("point");
    assert_eq!(
        EntityRecord::new(
            EntityId::new("duplicate-points").expect("entity id"),
            "Duplicate points",
            "switch",
            vec![point.clone(), point],
            None,
            None,
            vec![],
        ),
        Err(DomainError::DuplicateIntegrationPoint)
    );

    let duplicate = IntegrationTopologySnapshot::new(
        gateway_id(),
        integration_id(),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(1),
        SnapshotDigest::new(
            "sha256:1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest"),
        vec![area(), area()],
        vec![device()],
        vec![entity()],
    );
    assert_eq!(duplicate, Err(DomainError::DuplicateIntegrationResource));

    let orphan = EntityRecord::new(
        EntityId::new("orphan").expect("entity id"),
        "Orphan",
        "switch",
        vec![
            EntityPointDescriptor::new(
                IntegrationPointKey::new("state").expect("point key"),
                "State",
                IntegrationPointKind::State,
                ObservedValueType::Boolean,
                None,
            )
            .expect("point"),
        ],
        Some(DeviceId::new("missing-device").expect("device id")),
        None,
        vec![],
    )
    .expect("entity");
    let dangling = IntegrationTopologySnapshot::new(
        gateway_id(),
        integration_id(),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(1),
        SnapshotDigest::new(
            "sha256:2123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest"),
        vec![],
        vec![],
        vec![orphan],
    );
    assert_eq!(dangling, Err(DomainError::DanglingIntegrationReference));
}

#[test]
fn entity_area_overrides_the_device_area_without_changing_device_identity() {
    let upstairs = AreaRecord::new(
        AreaId::new("upstairs").expect("area id"),
        "Upstairs",
        vec![],
    )
    .expect("area");
    let overridden = EntityRecord::new(
        EntityId::new("temperature-entity").expect("entity id"),
        "Upstairs temperature",
        "sensor",
        vec![
            EntityPointDescriptor::new(
                IntegrationPointKey::new("state").expect("point key"),
                "Temperature",
                IntegrationPointKind::State,
                ObservedValueType::Float64,
                Some("Cel"),
            )
            .expect("point"),
        ],
        Some(device().id().clone()),
        Some(upstairs.id().clone()),
        vec![],
    )
    .expect("entity");
    let topology = IntegrationTopologySnapshot::new(
        gateway_id(),
        integration_id(),
        TopologyGeneration::new(8).expect("generation"),
        TimestampMs::new(2),
        SnapshotDigest::new(
            "sha256:3123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest"),
        vec![area(), upstairs],
        vec![device()],
        vec![overridden],
    )
    .expect("topology");

    assert_eq!(
        topology.entities()[0]
            .effective_area_id(topology.devices())
            .map(AreaId::as_str),
        Some("upstairs")
    );
}

#[test]
fn observed_values_preserve_home_states_without_forcing_them_into_f64() {
    assert_eq!(
        ObservedValue::boolean(true).value_type(),
        ObservedValueType::Boolean
    );
    assert_eq!(
        ObservedValue::enumeration("heat")
            .expect("enum value")
            .value_type(),
        ObservedValueType::Enum
    );
    assert_eq!(
        ObservedValue::string("Now playing")
            .expect("string value")
            .value_type(),
        ObservedValueType::String
    );
    assert_eq!(
        ObservedValue::float64(f64::NAN),
        Err(DomainError::NonFiniteObservedValue)
    );
}

#[test]
fn unknown_and_unavailable_are_quality_evidence_not_fabricated_values() {
    let available = IntegrationObservation::available(
        gateway_id(),
        integration_id(),
        entity().id().clone(),
        IntegrationPointKey::new("state").expect("point key"),
        ObservedValue::boolean(true),
        TimestampMs::new(100),
        1,
        Some("home-assistant-context-1"),
    )
    .expect("state is valid");
    assert_eq!(available.quality(), IntegrationStateQuality::Good);
    assert_eq!(available.value(), Some(&ObservedValue::boolean(true)));

    let unknown = IntegrationObservation::unknown(
        gateway_id(),
        integration_id(),
        entity().id().clone(),
        IntegrationPointKey::new("state").expect("point key"),
        TimestampMs::new(101),
        2,
        None,
    )
    .expect("unknown state is valid");
    assert_eq!(unknown.quality(), IntegrationStateQuality::Unknown);
    assert!(unknown.value().is_none());

    let unavailable = IntegrationObservation::unavailable(
        gateway_id(),
        integration_id(),
        entity().id().clone(),
        IntegrationPointKey::new("state").expect("point key"),
        TimestampMs::new(102),
        3,
        None,
    )
    .expect("unavailable state is valid");
    assert_eq!(unavailable.quality(), IntegrationStateQuality::Unavailable);
    assert!(unavailable.value().is_none());
}

#[test]
fn malformed_identity_generation_digest_and_context_are_rejected() {
    assert_eq!(
        IntegrationId::new(" "),
        Err(DomainError::InvalidIntegrationIdentifier)
    );
    assert_eq!(
        TopologyGeneration::new(0),
        Err(DomainError::ZeroTopologyGeneration)
    );
    assert_eq!(
        SnapshotDigest::new("sha256:not-a-digest"),
        Err(DomainError::InvalidSnapshotDigest)
    );
    assert_eq!(
        IntegrationObservation::available(
            gateway_id(),
            integration_id(),
            entity().id().clone(),
            IntegrationPointKey::new("state").expect("point key"),
            ObservedValue::boolean(true),
            TimestampMs::new(1),
            0,
            Some("context"),
        ),
        Err(DomainError::ZeroIntegrationStateSequence)
    );
    assert_eq!(
        IntegrationObservation::available(
            gateway_id(),
            integration_id(),
            entity().id().clone(),
            IntegrationPointKey::new("state").expect("point key"),
            ObservedValue::boolean(true),
            TimestampMs::new(1),
            1,
            Some(" "),
        ),
        Err(DomainError::InvalidIntegrationContext)
    );
}

#[test]
fn provider_snapshot_validates_initial_observations_against_declared_points() {
    let valid = IntegrationObservation::available(
        gateway_id(),
        integration_id(),
        entity().id().clone(),
        IntegrationPointKey::new("state").expect("point key"),
        ObservedValue::boolean(true),
        TimestampMs::new(100),
        1,
        None,
    )
    .expect("observation");
    let snapshot =
        IntegrationSnapshot::new(topology(), vec![valid.clone()]).expect("provider snapshot");
    assert_eq!(snapshot.observations(), &[valid]);

    let mismatched = IntegrationObservation::available(
        gateway_id(),
        integration_id(),
        entity().id().clone(),
        IntegrationPointKey::new("state").expect("point key"),
        ObservedValue::string("on").expect("string"),
        TimestampMs::new(100),
        2,
        None,
    )
    .expect("observation");
    assert_eq!(
        IntegrationSnapshot::new(topology(), vec![mismatched]),
        Err(DomainError::InvalidIntegrationObservation)
    );
}
