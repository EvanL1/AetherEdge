//! Closed semantic power mapping kept entirely inside the provider adapter.

use aether_integration_control::{
    ControllableEntityKind, IntegrationActionExecutor, IntegrationPowerAction, ProviderAcceptance,
    ProviderExecutionResult,
};
use aether_ports::{PortErrorKind, PortResult};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::WebSocketHomeAssistantTransport;

#[derive(Debug, Clone)]
pub(crate) struct HomeAssistantPowerRequest {
    entity_kind: ControllableEntityKind,
    source_address: String,
    value: bool,
}

impl HomeAssistantPowerRequest {
    pub(crate) fn from_action(action: &IntegrationPowerAction) -> Self {
        Self {
            entity_kind: action.entity_kind(),
            source_address: action.source_address().to_string(),
            value: action.value(),
        }
    }

    pub(crate) fn command(&self) -> Value {
        json!({
            "type": "call_service",
            "domain": self.entity_kind.as_str(),
            "service": if self.value { "turn_on" } else { "turn_off" },
            "target": {"entity_id": self.source_address}
        })
    }
}

pub(crate) fn decode_provider_acceptance(result: &Value) -> PortResult<ProviderAcceptance> {
    let context_id = result
        .get("context")
        .and_then(Value::as_object)
        .and_then(|context| context.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            aether_ports::PortError::new(
                PortErrorKind::InvalidData,
                "Home Assistant accepted a service call without correlation context",
            )
        })?;
    ProviderAcceptance::new(context_id).map_err(|_source| {
        aether_ports::PortError::new(
            PortErrorKind::InvalidData,
            "Home Assistant returned invalid correlation context",
        )
    })
}

#[async_trait]
impl IntegrationActionExecutor for WebSocketHomeAssistantTransport {
    async fn execute(&self, action: &IntegrationPowerAction) -> ProviderExecutionResult {
        match self
            .request_power(HomeAssistantPowerRequest::from_action(action))
            .await
        {
            Ok(acceptance) => ProviderExecutionResult::Accepted(acceptance),
            Err(error)
                if matches!(
                    error.kind(),
                    PortErrorKind::Rejected | PortErrorKind::NotFound
                ) =>
            {
                ProviderExecutionResult::Rejected
            },
            Err(_error) => ProviderExecutionResult::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use aether_integration_control::{
        ActionTarget, ControllableEntityKind, IntegrationPowerAction, ResolvedControlTarget,
    };
    use serde_json::json;

    use super::HomeAssistantPowerRequest;

    fn action(kind: ControllableEntityKind, address: &str, value: bool) -> IntegrationPowerAction {
        let target =
            ActionTarget::new("home-assistant.home", 1, "entity-registry-id").expect("target");
        let resolved =
            ResolvedControlTarget::home_assistant(target, kind, address).expect("resolved");
        IntegrationPowerAction::for_resolved_target(
            "55555555-5555-4555-8555-555555555555",
            resolved,
            value,
        )
        .expect("action")
    }

    #[test]
    fn every_supported_kind_maps_only_to_fixed_power_operations() {
        for (kind, address) in [
            (ControllableEntityKind::Light, "light.kitchen"),
            (ControllableEntityKind::Switch, "switch.pump"),
            (ControllableEntityKind::Fan, "fan.bedroom"),
        ] {
            assert_eq!(
                HomeAssistantPowerRequest::from_action(&action(kind, address, true)).command(),
                json!({
                    "type": "call_service",
                    "domain": kind.as_str(),
                    "service": "turn_on",
                    "target": {"entity_id": address}
                })
            );
            assert_eq!(
                HomeAssistantPowerRequest::from_action(&action(kind, address, false)).command(),
                json!({
                    "type": "call_service",
                    "domain": kind.as_str(),
                    "service": "turn_off",
                    "target": {"entity_id": address}
                })
            );
        }
    }
}
