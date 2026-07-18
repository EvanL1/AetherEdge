//! Explicit public mapping profiles kept outside the provider-neutral domain.

use aether_domain::{EntityPointDescriptor, EntityRecord, IntegrationPointKind};

use crate::error::{
    ContractResult, IntegrationContractError, IntegrationContractErrorCode as Code,
};
use crate::wire::IntegrationPointKindV1Alpha1;

/// Maps an internal provider-neutral topology into one published Integration profile.
pub trait IntegrationV1Alpha1Profile {
    /// Returns the constrained public integration kind.
    fn integration_kind(&self) -> &str;

    /// Resolves the current provider address without changing stable entity identity.
    fn source_address<'a>(&self, entity: &'a EntityRecord) -> ContractResult<&'a str>;

    /// Classifies semantic point meaning for the public contract.
    fn point_kind(
        &self,
        entity: &EntityRecord,
        point: &EntityPointDescriptor,
    ) -> IntegrationPointKindV1Alpha1;
}

/// Published Home Assistant mapping profile for Integration v1alpha1.
#[derive(Debug, Clone, Copy, Default)]
pub struct HomeAssistantV1Alpha1Profile;

impl IntegrationV1Alpha1Profile for HomeAssistantV1Alpha1Profile {
    fn integration_kind(&self) -> &str {
        "home-assistant"
    }

    fn source_address<'a>(&self, entity: &'a EntityRecord) -> ContractResult<&'a str> {
        let mut aliases = entity
            .aliases()
            .iter()
            .filter(|alias| alias.namespace() == "home-assistant" && alias.kind() == "entity-id");
        let source_address = aliases.next().ok_or_else(|| {
            IntegrationContractError::new(
                Code::ReferenceNotFound,
                "Home Assistant entity has no current entity-id alias",
            )
        })?;
        if aliases.next().is_some() {
            return Err(IntegrationContractError::new(
                Code::IdentityConflict,
                "Home Assistant entity has multiple current entity-id aliases",
            ));
        }
        Ok(source_address.value())
    }

    fn point_kind(
        &self,
        entity: &EntityRecord,
        point: &EntityPointDescriptor,
    ) -> IntegrationPointKindV1Alpha1 {
        let key = point.key().as_str();
        if point.kind() == IntegrationPointKind::Event
            || entity.kind() == "event"
            || key == "event_type"
        {
            return IntegrationPointKindV1Alpha1::Event;
        }
        if (entity.kind() == "sensor" && key == "state")
            || matches!(
                key,
                "current_temperature" | "current_humidity" | "battery_level"
            )
        {
            return IntegrationPointKindV1Alpha1::Telemetry;
        }
        IntegrationPointKindV1Alpha1::Status
    }
}
