use std::sync::Arc;

use aether_domain::{
    GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationSnapshot, TopologyGeneration,
};
use aether_ports::{
    IntegrationProjectionChange, IntegrationProjectionQuery, IntegrationProjectionReceipt,
    IntegrationProjectionSink, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

struct UnavailableProjection;

#[async_trait]
impl IntegrationProjectionSink for UnavailableProjection {
    async fn replace_snapshot(
        &self,
        _snapshot: IntegrationSnapshot,
    ) -> PortResult<IntegrationProjectionReceipt> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "projection is offline",
        ))
    }

    async fn apply_observation(
        &self,
        _expected_generation: TopologyGeneration,
        _observation: IntegrationObservation,
    ) -> PortResult<IntegrationProjectionReceipt> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "projection is offline",
        ))
    }
}

#[async_trait]
impl IntegrationProjectionQuery for UnavailableProjection {
    async fn snapshot(
        &self,
        _gateway_id: &GatewayIdentity,
        _integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "projection is offline",
        ))
    }
}

#[test]
fn integration_projection_ports_are_object_safe() {
    fn accepts_sink(_: Option<Arc<dyn IntegrationProjectionSink>>) {}
    fn accepts_query(_: Option<Arc<dyn IntegrationProjectionQuery>>) {}

    accepts_sink(Some(Arc::new(UnavailableProjection)));
    accepts_query(Some(Arc::new(UnavailableProjection)));
}

#[test]
fn projection_receipts_preserve_scope_generation_and_progress() {
    let gateway_id = GatewayIdentity::new("gateway-home").expect("gateway identity");
    let integration_id = IntegrationId::new("home-assistant-home").expect("integration identity");
    let generation = TopologyGeneration::new(7).expect("generation");

    let snapshot = IntegrationProjectionReceipt::snapshot_replaced(
        gateway_id.clone(),
        integration_id.clone(),
        generation,
        3,
        Some(9),
    );
    assert_eq!(snapshot.gateway_id(), &gateway_id);
    assert_eq!(snapshot.integration_id(), &integration_id);
    assert_eq!(snapshot.generation(), generation);
    assert_eq!(
        snapshot.change(),
        IntegrationProjectionChange::SnapshotReplaced {
            observation_count: 3,
            last_sequence: Some(9),
        }
    );

    let observation = IntegrationProjectionReceipt::observation_applied(
        gateway_id,
        integration_id,
        generation,
        10,
    );
    assert_eq!(
        observation.change(),
        IntegrationProjectionChange::ObservationApplied { sequence: 10 }
    );
}
