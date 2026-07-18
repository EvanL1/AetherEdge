//! Strict AetherContracts 0.1.0-alpha.4 `aether.integration` v1alpha1 binding.
//!
//! The internal AetherEdge integration model remains optimized for local
//! synchronization. This crate is the only public wire boundary: it emits
//! closed snake_case DTOs, preserves integer precision, applies Foundation
//! number semantics, and validates topology-dependent observations.

mod codec;
mod error;
mod profile;
mod validation;
mod wire;

pub use codec::IntegrationContractCodec;
pub use error::{IntegrationContractError, IntegrationContractErrorCode};
pub use profile::{HomeAssistantV1Alpha1Profile, IntegrationV1Alpha1Profile};
pub use wire::{
    EntityPointDescriptorV1Alpha1, IntegrationAreaV1Alpha1, IntegrationDeviceV1Alpha1,
    IntegrationEntityV1Alpha1, IntegrationObservationBatchV1Alpha1,
    IntegrationObservationQualityV1Alpha1, IntegrationObservationV1Alpha1,
    IntegrationPointKindV1Alpha1, IntegrationTopologySnapshotV1Alpha1, ObservedValueTypeV1Alpha1,
    ObservedValueV1Alpha1,
};

/// Exact candidate release against which this binding was implemented.
pub const AETHER_CONTRACTS_RELEASE: &str = "0.1.0-alpha.4";

/// Closed topology schema discriminator.
pub const TOPOLOGY_SNAPSHOT_SCHEMA: &str = "aether.integration.topology-snapshot.v1alpha1";

/// Closed observation-batch schema discriminator.
pub const OBSERVATION_BATCH_SCHEMA: &str = "aether.integration.observation-batch.v1alpha1";

/// Edge-specific complete-message safety limit for this binding.
///
/// AetherContracts freezes field limits but permits bindings to fail closed
/// with a documented stricter resource limit.
pub const MAX_INTEGRATION_MESSAGE_BYTES: usize = 16 * 1_024 * 1_024;
