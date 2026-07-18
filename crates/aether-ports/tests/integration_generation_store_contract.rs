use aether_domain::{GatewayIdentity, IntegrationId, SnapshotDigest, TopologyGeneration};
use aether_ports::{IntegrationTopologyGenerationStore, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;

struct ContractOnlyStore;

#[async_trait]
impl IntegrationTopologyGenerationStore for ContractOnlyStore {
    async fn reserve_generation(
        &self,
        _gateway_id: &GatewayIdentity,
        _integration_id: &IntegrationId,
        _snapshot_digest: &SnapshotDigest,
    ) -> PortResult<TopologyGeneration> {
        TopologyGeneration::new(1)
            .map_err(|error| PortError::new(PortErrorKind::Permanent, error.to_string()))
    }
}

#[tokio::test]
async fn generation_store_is_an_object_safe_atomic_reservation_port() {
    let store: Box<dyn IntegrationTopologyGenerationStore> = Box::new(ContractOnlyStore);
    let generation = store
        .reserve_generation(
            &GatewayIdentity::new("gateway-home").expect("gateway"),
            &IntegrationId::new("home-assistant.home").expect("integration"),
            &SnapshotDigest::new(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("digest"),
        )
        .await
        .expect("reservation");
    assert_eq!(generation.get(), 1);
}
