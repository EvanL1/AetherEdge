//! Read-only synchronization of delegated device-provider projections.

use std::fmt;
use std::sync::Arc;

use aether_domain::{IntegrationObservation, IntegrationSnapshot, IntegrationStateQuality};
use aether_ports::{
    DelegatedDeviceProvider, IntegrationProjectionChange, IntegrationProjectionQuery,
    IntegrationProjectionReceipt, IntegrationProjectionSink, PortError, PortErrorKind,
};
use thiserror::Error;

/// Stable reason that incremental synchronization must stop for a complete snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationResyncReason {
    /// The provider reports that its bounded event stream lost ordering or data.
    ProviderStreamGap,
    /// No complete local projection exists for the provider scope.
    ProjectionMissing,
    /// The provider observation no longer matches the commissioned gateway or integration.
    ScopeChanged,
    /// The provider emitted an entity or point absent from the projected topology.
    UndeclaredPoint,
    /// The observed scalar type no longer matches the projected point declaration.
    ValueTypeChanged,
    /// The next connection-local observation sequence is not contiguous.
    SequenceGap,
    /// The projection generation or sequence changed concurrently.
    ProjectionConflict,
}

impl fmt::Display for IntegrationResyncReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::ProviderStreamGap => "provider-stream-gap",
            Self::ProjectionMissing => "projection-missing",
            Self::ScopeChanged => "scope-changed",
            Self::UndeclaredPoint => "undeclared-point",
            Self::ValueTypeChanged => "value-type-changed",
            Self::SequenceGap => "sequence-gap",
            Self::ProjectionConflict => "projection-conflict",
        };
        formatter.write_str(reason)
    }
}

/// Failure returned by one bounded integration synchronization operation.
#[derive(Debug, Error)]
pub enum IntegrationSynchronizationError {
    /// The delegated provider failed without proving an event-stream gap.
    #[error("delegated integration provider failed: {0}")]
    Provider(PortError),
    /// The read-only projection failed without a concurrent-state conflict.
    #[error("integration projection failed: {0}")]
    Projection(PortError),
    /// Incremental synchronization must stop until a complete snapshot succeeds.
    #[error("integration requires a complete resynchronization: {reason}")]
    ResyncRequired {
        /// Machine-readable reason for the required snapshot.
        reason: IntegrationResyncReason,
        /// Optional underlying port conflict, retained without string parsing.
        failure: Option<PortError>,
    },
    /// A provider returned data outside the identity advertised by its port.
    #[error("delegated integration provider changed its advertised scope")]
    ProviderScopeMismatch,
    /// A projection adapter returned a receipt that does not correlate to its input.
    #[error("integration projection returned an invalid commit receipt")]
    InvalidProjectionReceipt,
}

impl IntegrationSynchronizationError {
    /// Returns whether the caller must obtain a complete provider snapshot before continuing.
    #[must_use]
    pub const fn requires_resync(&self) -> bool {
        matches!(self, Self::ResyncRequired { .. })
    }

    /// Returns the stable resynchronization reason, when applicable.
    #[must_use]
    pub const fn resync_reason(&self) -> Option<IntegrationResyncReason> {
        match self {
            Self::ResyncRequired { reason, .. } => Some(*reason),
            _ => None,
        }
    }
}

/// Copies delegated integration state into an application-owned read-only projection.
///
/// Each method performs one bounded operation and never retries. After any
/// [`IntegrationSynchronizationError::ResyncRequired`], callers must stop
/// incremental consumption and invoke [`Self::synchronize_snapshot`] before
/// requesting another observation.
pub struct IntegrationSynchronizer {
    provider: Arc<dyn DelegatedDeviceProvider>,
    projection_sink: Arc<dyn IntegrationProjectionSink>,
    projection_query: Arc<dyn IntegrationProjectionQuery>,
}

impl IntegrationSynchronizer {
    /// Creates a synchronizer from vendor-neutral provider and projection ports.
    #[must_use]
    pub fn new(
        provider: Arc<dyn DelegatedDeviceProvider>,
        projection_sink: Arc<dyn IntegrationProjectionSink>,
        projection_query: Arc<dyn IntegrationProjectionQuery>,
    ) -> Self {
        Self {
            provider,
            projection_sink,
            projection_query,
        }
    }

    /// Replaces topology and current observations in one projection commit.
    pub async fn synchronize_snapshot(
        &self,
    ) -> Result<IntegrationProjectionReceipt, IntegrationSynchronizationError> {
        let snapshot = self.provider.snapshot().await.map_err(map_provider_error)?;
        validate_provider_snapshot_scope(self.provider.as_ref(), &snapshot)?;

        let expected_gateway = snapshot.topology().gateway_id().clone();
        let expected_integration = snapshot.topology().integration_id().clone();
        let expected_generation = snapshot.topology().generation();
        let expected_count = snapshot.observations().len();
        let expected_last_sequence = last_sequence(&snapshot);
        let receipt = self
            .projection_sink
            .replace_snapshot(snapshot)
            .await
            .map_err(map_projection_error)?;

        if receipt.gateway_id() != &expected_gateway
            || receipt.integration_id() != &expected_integration
            || receipt.generation() != expected_generation
            || receipt.change()
                != (IntegrationProjectionChange::SnapshotReplaced {
                    observation_count: expected_count,
                    last_sequence: expected_last_sequence,
                })
        {
            return Err(IntegrationSynchronizationError::InvalidProjectionReceipt);
        }
        Ok(receipt)
    }

    /// Projects one ordered observation and then returns to the caller.
    ///
    /// This method intentionally contains no retry loop. Provider or projection
    /// conflicts become an explicit resynchronization requirement.
    pub async fn synchronize_next(
        &self,
    ) -> Result<IntegrationProjectionReceipt, IntegrationSynchronizationError> {
        let observation = self
            .provider
            .next_observation()
            .await
            .map_err(map_provider_error)?;
        if observation.gateway_id() != self.provider.gateway_id()
            || observation.integration_id() != self.provider.integration_id()
        {
            return Err(resync(IntegrationResyncReason::ScopeChanged, None));
        }

        let projected = self
            .projection_query
            .snapshot(self.provider.gateway_id(), self.provider.integration_id())
            .await
            .map_err(map_projection_error)?
            .ok_or_else(|| resync(IntegrationResyncReason::ProjectionMissing, None))?;
        validate_projection_scope(self.provider.as_ref(), &projected)?;
        validate_observation(&projected, &observation)?;

        let expected_sequence = last_sequence(&projected)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| resync(IntegrationResyncReason::SequenceGap, None))?;
        if observation.sequence() != expected_sequence {
            return Err(resync(IntegrationResyncReason::SequenceGap, None));
        }

        let expected_gateway = observation.gateway_id().clone();
        let expected_integration = observation.integration_id().clone();
        let expected_generation = projected.topology().generation();
        let expected_sequence = observation.sequence();
        let receipt = self
            .projection_sink
            .apply_observation(expected_generation, observation)
            .await
            .map_err(map_projection_error)?;

        if receipt.gateway_id() != &expected_gateway
            || receipt.integration_id() != &expected_integration
            || receipt.generation() != expected_generation
            || receipt.change()
                != (IntegrationProjectionChange::ObservationApplied {
                    sequence: expected_sequence,
                })
        {
            return Err(IntegrationSynchronizationError::InvalidProjectionReceipt);
        }
        Ok(receipt)
    }
}

fn validate_provider_snapshot_scope(
    provider: &dyn DelegatedDeviceProvider,
    snapshot: &IntegrationSnapshot,
) -> Result<(), IntegrationSynchronizationError> {
    if snapshot.topology().gateway_id() != provider.gateway_id()
        || snapshot.topology().integration_id() != provider.integration_id()
    {
        return Err(IntegrationSynchronizationError::ProviderScopeMismatch);
    }
    Ok(())
}

fn validate_projection_scope(
    provider: &dyn DelegatedDeviceProvider,
    snapshot: &IntegrationSnapshot,
) -> Result<(), IntegrationSynchronizationError> {
    if snapshot.topology().gateway_id() != provider.gateway_id()
        || snapshot.topology().integration_id() != provider.integration_id()
    {
        return Err(resync(IntegrationResyncReason::ProjectionConflict, None));
    }
    Ok(())
}

fn validate_observation(
    projected: &IntegrationSnapshot,
    observation: &IntegrationObservation,
) -> Result<(), IntegrationSynchronizationError> {
    let point = projected
        .topology()
        .entities()
        .iter()
        .find(|entity| entity.id() == observation.entity_id())
        .and_then(|entity| {
            entity
                .points()
                .iter()
                .find(|point| point.key() == observation.point_key())
        })
        .ok_or_else(|| resync(IntegrationResyncReason::UndeclaredPoint, None))?;

    let observed_type = match (observation.quality(), observation.value()) {
        (IntegrationStateQuality::Good, Some(value)) => Some(value.value_type()),
        (IntegrationStateQuality::Unknown | IntegrationStateQuality::Unavailable, None) => None,
        _ => return Err(resync(IntegrationResyncReason::ValueTypeChanged, None)),
    };
    if observed_type.is_some_and(|value_type| value_type != point.value_type()) {
        return Err(resync(IntegrationResyncReason::ValueTypeChanged, None));
    }
    Ok(())
}

fn last_sequence(snapshot: &IntegrationSnapshot) -> Option<u64> {
    snapshot
        .observations()
        .iter()
        .map(IntegrationObservation::sequence)
        .max()
}

fn map_provider_error(error: PortError) -> IntegrationSynchronizationError {
    if error.kind() == PortErrorKind::Conflict {
        resync(IntegrationResyncReason::ProviderStreamGap, Some(error))
    } else {
        IntegrationSynchronizationError::Provider(error)
    }
}

fn map_projection_error(error: PortError) -> IntegrationSynchronizationError {
    if error.kind() == PortErrorKind::Conflict {
        resync(IntegrationResyncReason::ProjectionConflict, Some(error))
    } else {
        IntegrationSynchronizationError::Projection(error)
    }
}

fn resync(
    reason: IntegrationResyncReason,
    failure: Option<PortError>,
) -> IntegrationSynchronizationError {
    IntegrationSynchronizationError::ResyncRequired { reason, failure }
}
