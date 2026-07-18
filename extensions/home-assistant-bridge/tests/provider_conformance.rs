use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use aether_domain::{
    GatewayIdentity, IntegrationId, IntegrationPointKey, IntegrationSnapshot,
    IntegrationStateQuality, ObservedValue, ObservedValueType, SnapshotDigest, TopologyGeneration,
};
use aether_home_assistant_bridge::{
    HomeAssistantArea, HomeAssistantBridge, HomeAssistantDevice, HomeAssistantEntity,
    HomeAssistantSnapshot, HomeAssistantState, HomeAssistantStateChanged, HomeAssistantTransport,
};
use aether_ports::{
    DelegatedDeviceProvider, IntegrationTopologyGenerationStore, PortError, PortErrorKind,
    PortResult,
};
use aether_testkit::assert_delegated_device_provider_scope;
use async_trait::async_trait;
use serde_json::json;

struct FakeTransport {
    snapshot: HomeAssistantSnapshot,
    events: Mutex<VecDeque<HomeAssistantStateChanged>>,
}

struct FixedGenerationStore;

#[async_trait]
impl IntegrationTopologyGenerationStore for FixedGenerationStore {
    async fn reserve_generation(
        &self,
        _gateway_id: &GatewayIdentity,
        _integration_id: &IntegrationId,
        _snapshot_digest: &SnapshotDigest,
    ) -> PortResult<TopologyGeneration> {
        TopologyGeneration::new(41)
            .map_err(|error| PortError::new(PortErrorKind::Permanent, error.to_string()))
    }
}

#[async_trait]
impl HomeAssistantTransport for FakeTransport {
    async fn fetch_snapshot(&self) -> PortResult<HomeAssistantSnapshot> {
        Ok(self.snapshot.clone())
    }

    async fn next_state_changed(&self) -> PortResult<HomeAssistantStateChanged> {
        self.events
            .lock()
            .map_err(|_| PortError::new(PortErrorKind::Permanent, "test event lock poisoned"))?
            .pop_front()
            .ok_or_else(|| PortError::new(PortErrorKind::Unavailable, "test event queue empty"))
    }
}

fn snapshot() -> HomeAssistantSnapshot {
    HomeAssistantSnapshot {
        areas: vec![HomeAssistantArea {
            id: "kitchen".into(),
            name: "Kitchen".into(),
        }],
        devices: vec![HomeAssistantDevice {
            id: "device-42".into(),
            name: "Kitchen lamp".into(),
            area_id: Some("kitchen".into()),
        }],
        entities: vec![HomeAssistantEntity {
            id: "registry-17".into(),
            entity_id: "light.kitchen".into(),
            name: "Kitchen lamp".into(),
            domain: "light".into(),
            device_id: Some("device-42".into()),
            area_id: None,
        }],
        states: vec![HomeAssistantState {
            entity_id: "light.kitchen".into(),
            state: "on".into(),
            attributes: BTreeMap::from([("brightness".into(), json!(128))]),
            observed_at_ms: 1_720_000_000_000,
            context_id: Some("ha-context-initial".into()),
        }],
    }
}

fn observation<'a>(
    snapshot: &'a IntegrationSnapshot,
    entity_id: &str,
    point_key: &str,
) -> &'a aether_domain::IntegrationObservation {
    snapshot
        .observations()
        .iter()
        .find(|observation| {
            observation.entity_id().as_str() == entity_id
                && observation.point_key().as_str() == point_key
        })
        .expect("mapped observation")
}

#[tokio::test]
async fn bridge_maps_registry_and_typed_initial_state_and_passes_conformance() {
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::new()),
        },
    );

    let snapshot = assert_delegated_device_provider_scope(
        &bridge,
        bridge.gateway_id(),
        bridge.integration_id(),
    )
    .await
    .expect("bridge conforms");
    let topology = snapshot.topology();

    assert_eq!(topology.areas().len(), 1);
    assert_eq!(topology.devices().len(), 1);
    assert_eq!(topology.entities().len(), 1);
    assert_eq!(topology.entities()[0].id().as_str(), "registry-17");
    assert_eq!(topology.entities()[0].points().len(), 2);
    assert_eq!(snapshot.observations()[0].point_key().as_str(), "is_on");
    assert_eq!(
        snapshot.observations()[0].value(),
        Some(&ObservedValue::boolean(true))
    );
    assert_eq!(
        snapshot.observations()[1].point_key().as_str(),
        "brightness"
    );
    assert_eq!(
        snapshot.observations()[1].value(),
        Some(&ObservedValue::uint64(128))
    );
}

#[tokio::test]
async fn malformed_declared_values_degrade_per_point_without_invalidating_the_snapshot() {
    let mut source = snapshot();
    source.entities = vec![
        HomeAssistantEntity {
            id: "registry-light".into(),
            entity_id: "light.kitchen".into(),
            name: "Kitchen lamp".into(),
            domain: "light".into(),
            device_id: Some("device-42".into()),
            area_id: None,
        },
        HomeAssistantEntity {
            id: "registry-media".into(),
            entity_id: "media_player.kitchen".into(),
            name: "Kitchen speaker".into(),
            domain: "media_player".into(),
            device_id: Some("device-42".into()),
            area_id: None,
        },
        HomeAssistantEntity {
            id: "registry-number".into(),
            entity_id: "number.kitchen_target".into(),
            name: "Kitchen target".into(),
            domain: "number".into(),
            device_id: Some("device-42".into()),
            area_id: None,
        },
        HomeAssistantEntity {
            id: "registry-sensor".into(),
            entity_id: "sensor.kitchen_temperature".into(),
            name: "Kitchen temperature".into(),
            domain: "sensor".into(),
            device_id: Some("device-42".into()),
            area_id: None,
        },
    ];
    source.states = vec![
        HomeAssistantState {
            entity_id: "light.kitchen".into(),
            state: "maybe".into(),
            attributes: BTreeMap::from([("brightness".into(), json!("maximum"))]),
            observed_at_ms: 1_720_000_000_000,
            context_id: Some("ha-context-bad-boolean".into()),
        },
        HomeAssistantState {
            entity_id: "media_player.kitchen".into(),
            state: "playing".into(),
            attributes: BTreeMap::from([
                ("is_volume_muted".into(), json!(false)),
                ("volume_level".into(), json!("loud")),
            ]),
            observed_at_ms: 1_720_000_000_001,
            context_id: Some("ha-context-bad-attribute".into()),
        },
        HomeAssistantState {
            entity_id: "number.kitchen_target".into(),
            state: "not-a-number".into(),
            attributes: BTreeMap::new(),
            observed_at_ms: 1_720_000_000_002,
            context_id: Some("ha-context-unparseable-number".into()),
        },
        HomeAssistantState {
            entity_id: "sensor.kitchen_temperature".into(),
            state: "NaN".into(),
            attributes: BTreeMap::from([("unit_of_measurement".into(), json!("°C"))]),
            observed_at_ms: 1_720_000_000_003,
            context_id: Some("ha-context-non-finite-number".into()),
        },
    ];
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: source,
            events: Mutex::new(VecDeque::new()),
        },
    );

    let snapshot = bridge
        .snapshot()
        .await
        .expect("one malformed point must not invalidate the snapshot");

    for (entity_id, point_key) in [
        ("registry-light", "is_on"),
        ("registry-light", "brightness"),
        ("registry-media", "volume_level"),
        ("registry-number", "state"),
        ("registry-sensor", "state"),
    ] {
        let degraded = observation(&snapshot, entity_id, point_key);
        assert_eq!(degraded.quality(), IntegrationStateQuality::Unknown);
        assert_eq!(degraded.value(), None);
    }

    let media_state = observation(&snapshot, "registry-media", "state");
    assert_eq!(media_state.quality(), IntegrationStateQuality::Good);
    assert_eq!(
        media_state.value(),
        Some(&ObservedValue::enumeration("playing").expect("enum value"))
    );
    let muted = observation(&snapshot, "registry-media", "is_volume_muted");
    assert_eq!(muted.quality(), IntegrationStateQuality::Good);
    assert_eq!(muted.value(), Some(&ObservedValue::boolean(false)));
}

#[tokio::test]
async fn bridge_can_use_a_restart_stable_generation_store_without_changing_transport() {
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::new()),
        },
    )
    .with_generation_store(Arc::new(FixedGenerationStore));

    let snapshot = bridge.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.topology().generation().get(), 41);
}

#[tokio::test]
async fn bridge_maps_live_events_and_never_projects_unknown_attributes() {
    let event = HomeAssistantStateChanged {
        new_state: HomeAssistantState {
            entity_id: "light.kitchen".into(),
            state: "off".into(),
            attributes: BTreeMap::from([
                ("brightness".into(), json!(0)),
                ("access_token".into(), json!("must-not-cross-boundary")),
                ("vendor_blob".into(), json!({"unbounded": ["data"]})),
            ]),
            observed_at_ms: 1_720_000_000_100,
            context_id: Some("ha-context-event".into()),
        },
    };
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::from([event])),
        },
    );
    bridge
        .snapshot()
        .await
        .expect("snapshot primes the registry");

    let state = bridge.next_observation().await.expect("live state");
    let brightness = bridge.next_observation().await.expect("live brightness");
    assert_eq!(state.point_key().as_str(), "is_on");
    assert_eq!(state.value(), Some(&ObservedValue::boolean(false)));
    assert_eq!(brightness.value(), Some(&ObservedValue::uint64(0)));
    assert_eq!(state.source_context(), Some("ha-context-event"));
}

#[tokio::test]
async fn malformed_live_points_are_unknown_and_a_later_event_recovers() {
    let malformed = HomeAssistantStateChanged {
        new_state: HomeAssistantState {
            entity_id: "light.kitchen".into(),
            state: "maybe".into(),
            attributes: BTreeMap::from([("brightness".into(), json!("maximum"))]),
            observed_at_ms: 1_720_000_000_100,
            context_id: Some("ha-context-malformed-event".into()),
        },
    };
    let recovered = HomeAssistantStateChanged {
        new_state: HomeAssistantState {
            entity_id: "light.kitchen".into(),
            state: "off".into(),
            attributes: BTreeMap::from([("brightness".into(), json!(64))]),
            observed_at_ms: 1_720_000_000_200,
            context_id: Some("ha-context-recovered-event".into()),
        },
    };
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::from([malformed, recovered])),
        },
    );
    bridge
        .snapshot()
        .await
        .expect("snapshot primes the registry");

    for point_key in ["is_on", "brightness"] {
        let degraded = bridge
            .next_observation()
            .await
            .expect("malformed point is a valid unknown observation");
        assert_eq!(degraded.point_key().as_str(), point_key);
        assert_eq!(degraded.quality(), IntegrationStateQuality::Unknown);
        assert_eq!(degraded.value(), None);
        assert_eq!(
            degraded.source_context(),
            Some("ha-context-malformed-event")
        );
    }

    let state = bridge
        .next_observation()
        .await
        .expect("the next valid event must still be processed");
    let brightness = bridge
        .next_observation()
        .await
        .expect("the remaining valid point must still be processed");
    assert_eq!(state.point_key().as_str(), "is_on");
    assert_eq!(state.quality(), IntegrationStateQuality::Good);
    assert_eq!(state.value(), Some(&ObservedValue::boolean(false)));
    assert_eq!(brightness.point_key().as_str(), "brightness");
    assert_eq!(brightness.quality(), IntegrationStateQuality::Good);
    assert_eq!(brightness.value(), Some(&ObservedValue::uint64(64)));
}

#[tokio::test]
async fn event_for_an_entity_outside_the_snapshot_requires_complete_resynchronization() {
    let event = HomeAssistantStateChanged {
        new_state: HomeAssistantState {
            entity_id: "light.added_after_snapshot".into(),
            state: "on".into(),
            attributes: BTreeMap::new(),
            observed_at_ms: 1_720_000_000_100,
            context_id: Some("ha-context-new-entity".into()),
        },
    };
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::from([event])),
        },
    );
    bridge
        .snapshot()
        .await
        .expect("snapshot primes the known entity registry");

    let error = bridge
        .next_observation()
        .await
        .expect_err("an unknown entity invalidates the complete topology snapshot");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        error.message(),
        "Home Assistant state refers to an entity outside the last snapshot and requires a complete resynchronization"
    );
}

#[tokio::test]
async fn unavailable_numeric_state_keeps_a_metadata_defined_numeric_type() {
    let mut source = snapshot();
    source.entities[0] = HomeAssistantEntity {
        id: "registry-17".into(),
        entity_id: "sensor.kitchen_temperature".into(),
        name: "Kitchen temperature".into(),
        domain: "sensor".into(),
        device_id: Some("device-42".into()),
        area_id: None,
    };
    source.states[0] = HomeAssistantState {
        entity_id: "sensor.kitchen_temperature".into(),
        state: "unavailable".into(),
        attributes: BTreeMap::from([("unit_of_measurement".into(), json!("°C"))]),
        observed_at_ms: 1_720_000_000_000,
        context_id: Some("ha-context-unavailable".into()),
    };
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: source,
            events: Mutex::new(VecDeque::new()),
        },
    );

    let snapshot = bridge.snapshot().await.expect("snapshot");

    assert_eq!(
        snapshot.topology().entities()[0].points()[0].value_type(),
        ObservedValueType::Float64
    );
    assert_eq!(
        snapshot.observations()[0].quality(),
        IntegrationStateQuality::Unavailable
    );
    assert_eq!(snapshot.observations()[0].value(), None);
}

#[tokio::test]
async fn provider_reported_unknown_remains_unknown_for_every_declared_point() {
    let mut source = snapshot();
    source.states[0].state = "unknown".into();
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: source,
            events: Mutex::new(VecDeque::new()),
        },
    );

    let snapshot = bridge.snapshot().await.expect("unknown snapshot");

    assert_eq!(snapshot.observations().len(), 2);
    for observation in snapshot.observations() {
        assert_eq!(observation.quality(), IntegrationStateQuality::Unknown);
        assert_eq!(observation.value(), None);
    }
}

#[tokio::test]
async fn newly_mapped_attribute_requires_complete_resynchronization() {
    let mut source = snapshot();
    source.states[0].attributes.clear();
    let event = HomeAssistantStateChanged {
        new_state: HomeAssistantState {
            entity_id: "light.kitchen".into(),
            state: "on".into(),
            attributes: BTreeMap::from([("brightness".into(), json!(128))]),
            observed_at_ms: 1_720_000_000_100,
            context_id: Some("ha-context-new-point".into()),
        },
    };
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: source,
            events: Mutex::new(VecDeque::from([event])),
        },
    );
    bridge
        .snapshot()
        .await
        .expect("snapshot primes the known point mapping");

    let error = bridge
        .next_observation()
        .await
        .expect_err("a newly mapped attribute changes the topology");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        error.message(),
        "Home Assistant point mapping changed and requires a complete resynchronization"
    );
}

#[test]
fn controllable_boolean_domains_expose_is_on_without_a_state_alias() {
    let snapshot = snapshot();
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot,
            events: Mutex::new(VecDeque::new()),
        },
    );
    for domain in ["light", "switch", "fan"] {
        let attributes = if domain == "light" {
            BTreeMap::from([("brightness".into(), json!(128))])
        } else {
            BTreeMap::new()
        };
        let descriptors = bridge
            .mapped_point_descriptors(domain, &attributes)
            .expect("known mapping");

        assert_eq!(
            descriptors[0].key(),
            &IntegrationPointKey::new("is_on").expect("point key")
        );
        assert_eq!(descriptors[0].value_type(), ObservedValueType::Boolean);
        assert!(
            descriptors
                .iter()
                .all(|point| point.key().as_str() != "state")
        );
    }

    let light = bridge
        .mapped_point_descriptors(
            "light",
            &BTreeMap::from([("brightness".into(), json!(128))]),
        )
        .expect("known light mapping");
    assert_eq!(
        light[1].key(),
        &IntegrationPointKey::new("brightness").expect("point key")
    );
    assert_eq!(light[1].value_type(), ObservedValueType::UInt64);
    assert_eq!(light.len(), 2);
}

#[test]
fn other_domains_keep_the_state_point_key() {
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::new()),
        },
    );

    for domain in ["binary_sensor", "sensor", "climate", "cover", "lock"] {
        let descriptors = bridge
            .mapped_point_descriptors(domain, &BTreeMap::new())
            .expect("known mapping");
        assert_eq!(
            descriptors[0].key(),
            &IntegrationPointKey::new("state").expect("point key")
        );
    }
}

#[test]
fn media_content_is_private_by_default_and_never_enters_the_projection() {
    let bridge = HomeAssistantBridge::new(
        GatewayIdentity::new("gateway-home").expect("gateway identity"),
        IntegrationId::new("home-assistant-home").expect("integration id"),
        FakeTransport {
            snapshot: snapshot(),
            events: Mutex::new(VecDeque::new()),
        },
    );
    let descriptors = bridge
        .mapped_point_descriptors(
            "media_player",
            &BTreeMap::from([
                ("is_volume_muted".into(), json!(false)),
                ("media_title".into(), json!("Private listening history")),
                ("volume_level".into(), json!(0.25)),
            ]),
        )
        .expect("bounded media-player mapping");

    assert!(
        descriptors
            .iter()
            .all(|point| point.key().as_str() != "media_title"),
        "media content must require a future explicit privacy policy before projection"
    );
    assert!(
        descriptors
            .iter()
            .any(|point| point.key().as_str() == "volume_level")
    );
    assert!(
        descriptors
            .iter()
            .any(|point| point.key().as_str() == "is_volume_muted")
    );
}
