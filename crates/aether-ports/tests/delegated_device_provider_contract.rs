use std::sync::Arc;

use aether_domain::{GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationSnapshot};
use aether_ports::{DelegatedDeviceProvider, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;

struct DisconnectedProvider {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
}

#[async_trait]
impl DelegatedDeviceProvider for DisconnectedProvider {
    fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    async fn snapshot(&self) -> PortResult<IntegrationSnapshot> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "integration is disconnected",
        ))
    }

    async fn next_observation(&self) -> PortResult<IntegrationObservation> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "integration is disconnected",
        ))
    }
}

#[tokio::test]
async fn delegated_device_provider_is_object_safe_and_preserves_failure_semantics() {
    let provider: Arc<dyn DelegatedDeviceProvider> = Arc::new(DisconnectedProvider {
        gateway_id: GatewayIdentity::new("gateway-home").expect("gateway identity"),
        integration_id: IntegrationId::new("home-assistant-home").expect("integration id"),
    });

    assert_eq!(provider.gateway_id().as_str(), "gateway-home");
    assert_eq!(
        provider
            .snapshot()
            .await
            .expect_err("disconnected provider must fail")
            .kind(),
        PortErrorKind::Unavailable
    );
    assert_eq!(
        provider
            .next_observation()
            .await
            .expect_err("disconnected provider must fail")
            .kind(),
        PortErrorKind::Unavailable
    );
}
