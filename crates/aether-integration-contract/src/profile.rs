//! Explicit public mapping profiles kept outside the provider-neutral domain.

use aether_domain::{EntityPointDescriptor, EntityRecord};

use crate::error::ContractResult;
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
