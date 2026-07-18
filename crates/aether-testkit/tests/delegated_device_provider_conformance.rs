use std::sync::Mutex;

use aether_domain::{
    EntityId, EntityPointDescriptor, EntityRecord, GatewayIdentity, IntegrationId,
    IntegrationObservation, IntegrationPointKey, IntegrationPointKind, IntegrationSnapshot,
    IntegrationTopologySnapshot, ObservedValue, ObservedValueType, SnapshotDigest, TimestampMs,
    TopologyGeneration,
};
use aether_ports::{DelegatedDeviceProvider, PortError, PortErrorKind, PortResult};
use aether_testkit::assert_delegated_device_provider_scope;
use async_trait::async_trait;

fn gateway_id() -> GatewayIdentity {
    GatewayIdentity::new("gateway-home").expect("gateway identity")
}

fn integration_id() -> IntegrationId {
    IntegrationId::new("home-assistant-home").expect("integration id")
}

fn entity_id() -> EntityId {
    EntityId::new("entity-registry-17").expect("entity id")
}

fn point_key() -> IntegrationPointKey {
    IntegrationPointKey::new("state").expect("point key")
}

fn topology() -> IntegrationTopologySnapshot {
    let entity = EntityRecord::new(
        entity_id(),
        "Kitchen switch",
        "switch",
        vec![
            EntityPointDescriptor::new(
                point_key(),
                "State",
                IntegrationPointKind::State,
                ObservedValueType::Boolean,
                None,
            )
            .expect("point"),
        ],
        None,
        None,
        vec![],
    )
    .expect("entity");
    IntegrationTopologySnapshot::new(
        gateway_id(),
        integration_id(),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(10),
        SnapshotDigest::new(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("digest"),
        vec![],
        vec![],
        vec![entity],
    )
    .expect("topology")
}

struct ScriptedProvider {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    snapshot: IntegrationSnapshot,
    observation: Mutex<Option<IntegrationObservation>>,
}

impl ScriptedProvider {
    fn new(observation: IntegrationObservation) -> Self {
        Self {
            gateway_id: gateway_id(),
            integration_id: integration_id(),
            snapshot: IntegrationSnapshot::new(topology(), vec![observation.clone()])
                .expect("provider snapshot"),
            observation: Mutex::new(Some(observation)),
        }
    }
}

#[async_trait]
impl DelegatedDeviceProvider for ScriptedProvider {
    fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    async fn snapshot(&self) -> PortResult<IntegrationSnapshot> {
        Ok(self.snapshot.clone())
    }

    async fn next_observation(&self) -> PortResult<IntegrationObservation> {
        self.observation
            .lock()
            .map_err(|_| PortError::new(PortErrorKind::Permanent, "test lock poisoned"))?
            .take()
            .ok_or_else(|| PortError::new(PortErrorKind::Unavailable, "test queue empty"))
    }
}

#[tokio::test]
async fn conformance_accepts_scoped_known_point_with_matching_value_type() {
    let observation = IntegrationObservation::available(
        gateway_id(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(true),
        TimestampMs::new(11),
        1,
        None,
    )
    .expect("observation");
    let provider = ScriptedProvider::new(observation.clone());

    let actual =
        assert_delegated_device_provider_scope(&provider, &gateway_id(), &integration_id())
            .await
            .expect("provider conforms");
    assert_eq!(actual.observations(), &[observation]);
}

#[tokio::test]
async fn conformance_rejects_provider_identity_mismatch() {
    let observation = IntegrationObservation::available(
        gateway_id(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(true),
        TimestampMs::new(11),
        1,
        None,
    )
    .expect("observation");
    let mut provider = ScriptedProvider::new(observation);
    provider.gateway_id = GatewayIdentity::new("different-gateway").expect("gateway identity");

    let error = assert_delegated_device_provider_scope(&provider, &gateway_id(), &integration_id())
        .await
        .expect_err("provider identity changed");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}
