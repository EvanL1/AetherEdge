//! Production target resolution from the current atomic Integration projection.

use std::sync::Arc;

use aether_domain::{GatewayIdentity, IntegrationId, IntegrationPointKind, ObservedValueType};
use aether_ports::IntegrationProjectionQuery;
use async_trait::async_trait;

use crate::{
    ActionTarget, ControlDependencyError, ControlFailureCode, ControllableEntityKind,
    ResolvedControlTarget, TargetResolver,
};

/// Resolves control targets against the same committed topology used for read projection.
pub struct ProjectionTargetResolver {
    projection: Arc<dyn IntegrationProjectionQuery>,
}

impl ProjectionTargetResolver {
    /// Creates a resolver over one atomic Integration projection query.
    #[must_use]
    pub fn new(projection: Arc<dyn IntegrationProjectionQuery>) -> Self {
        Self { projection }
    }
}

#[async_trait]
impl TargetResolver for ProjectionTargetResolver {
    async fn resolve(
        &self,
        gateway_id: &str,
        target: &ActionTarget,
    ) -> Result<ResolvedControlTarget, ControlDependencyError> {
        let gateway = GatewayIdentity::new(gateway_id).map_err(|_source| not_found())?;
        let integration =
            IntegrationId::new(target.integration_id()).map_err(|_source| not_found())?;
        let snapshot = self
            .projection
            .snapshot(&gateway, &integration)
            .await
            .map_err(|_source| ControlDependencyError::new(ControlFailureCode::TargetUnavailable))?
            .ok_or_else(not_found)?;
        let topology = snapshot.topology();
        if topology.generation().get() != target.snapshot_generation() {
            return Err(ControlDependencyError::new(
                ControlFailureCode::TopologyGenerationMismatch,
            ));
        }
        let entity = topology
            .entities()
            .iter()
            .find(|entity| entity.id().as_str() == target.entity_id())
            .ok_or_else(not_found)?;
        let point = entity
            .points()
            .iter()
            .find(|point| point.key().as_str() == target.point_key())
            .ok_or_else(|| ControlDependencyError::new(ControlFailureCode::PointDenied))?;
        if point.kind() != IntegrationPointKind::State
            || point.value_type() != ObservedValueType::Boolean
        {
            return Err(ControlDependencyError::new(ControlFailureCode::PointDenied));
        }
        let entity_kind = match entity.kind() {
            "light" => ControllableEntityKind::Light,
            "switch" => ControllableEntityKind::Switch,
            "fan" => ControllableEntityKind::Fan,
            _ => {
                return Err(ControlDependencyError::new(
                    ControlFailureCode::EntityKindDenied,
                ));
            },
        };
        let source_address = entity
            .aliases()
            .iter()
            .find(|alias| alias.namespace() == "home-assistant" && alias.kind() == "entity-id")
            .map(aether_domain::ExternalAlias::value)
            .ok_or_else(not_found)?;
        ResolvedControlTarget::home_assistant(target.clone(), entity_kind, source_address)
    }
}

const fn not_found() -> ControlDependencyError {
    ControlDependencyError::new(ControlFailureCode::TargetNotFound)
}
