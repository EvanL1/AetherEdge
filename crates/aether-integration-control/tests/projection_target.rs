use std::sync::Arc;

use aether_domain::{
    EntityId, EntityPointDescriptor, EntityRecord, ExternalAlias, GatewayIdentity, IntegrationId,
    IntegrationPointKey, IntegrationPointKind, IntegrationSnapshot, IntegrationTopologySnapshot,
    ObservedValueType, SnapshotDigest, TimestampMs, TopologyGeneration,
};
use aether_integration_control::{
    ActionTarget, ControlFailureCode, ProjectionTargetResolver, TargetResolver,
};
use aether_ports::{IntegrationProjectionQuery, PortResult};
use async_trait::async_trait;

struct StaticProjection(IntegrationSnapshot);

#[async_trait]
impl IntegrationProjectionQuery for StaticProjection {
    async fn snapshot(
        &self,
        _gateway_id: &GatewayIdentity,
        _integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>> {
        Ok(Some(self.0.clone()))
    }
}

fn projection() -> IntegrationSnapshot {
    let point = EntityPointDescriptor::new(
        IntegrationPointKey::new("is_on").expect("point key"),
        "Power",
        IntegrationPointKind::State,
        ObservedValueType::Boolean,
        None,
    )
    .expect("point");
    let entity = EntityRecord::new(
        EntityId::new("entity-registry-light-bedroom").expect("entity ID"),
        "Bedroom light",
        "light",
        vec![point],
        None,
        None,
        vec![
            ExternalAlias::new("home-assistant", "entity-id", "light.bedroom")
                .expect("source alias"),
        ],
    )
    .expect("entity");
    let topology = IntegrationTopologySnapshot::new(
        GatewayIdentity::new("33333333-3333-4333-8333-333333333333").expect("gateway"),
        IntegrationId::new("home-assistant.home").expect("integration"),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(1_784_217_600_000),
        SnapshotDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
        vec![],
        vec![],
        vec![entity],
    )
    .expect("topology");
    IntegrationSnapshot::new(topology, vec![]).expect("snapshot")
}

#[tokio::test]
async fn projection_resolver_uses_exact_generation_entity_boolean_point_and_current_source() {
    let resolver = ProjectionTargetResolver::new(Arc::new(StaticProjection(projection())));
    let target = ActionTarget::new("home-assistant.home", 1, "entity-registry-light-bedroom")
        .expect("target");
    let resolved = resolver
        .resolve("33333333-3333-4333-8333-333333333333", &target)
        .await
        .expect("resolved");
    assert_eq!(resolved.target(), &target);
    assert_eq!(resolved.source_address(), "light.bedroom");

    let stale = ActionTarget::new("home-assistant.home", 2, "entity-registry-light-bedroom")
        .expect("stale target");
    let error = resolver
        .resolve("33333333-3333-4333-8333-333333333333", &stale)
        .await
        .expect_err("generation fence");
    assert_eq!(
        error.failure_code(),
        ControlFailureCode::TopologyGenerationMismatch
    );
}
