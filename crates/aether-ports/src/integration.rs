//! Delegated device-provider capability.

use aether_domain::{
    GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationSnapshot, SnapshotDigest,
    TopologyGeneration,
};
use async_trait::async_trait;

use crate::PortResult;

/// Atomically reserves a restart-stable topology generation for one integration.
///
/// Implementations must serialize reservations per gateway and integration.
/// The first digest is durably committed with generation one. Repeating that
/// digest returns the same generation without a write. A different digest
/// atomically increments and durably commits the generation before returning.
/// An implementation must never return an uncommitted generation, silently
/// wrap `u64::MAX`, or scope one counter across different integrations.
#[async_trait]
pub trait IntegrationTopologyGenerationStore: Send + Sync + 'static {
    /// Returns the durable generation bound to `snapshot_digest`.
    ///
    /// Temporary storage failure is [`crate::PortErrorKind::Unavailable`].
    /// Exhaustion or irrecoverable corruption is
    /// [`crate::PortErrorKind::Permanent`]. Concurrent compare-and-swap loss is
    /// retried inside the adapter or returned as [`crate::PortErrorKind::Conflict`].
    async fn reserve_generation(
        &self,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
        snapshot_digest: &SnapshotDigest,
    ) -> PortResult<TopologyGeneration>;
}

/// Durable progress represented by an integration projection receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationProjectionChange {
    /// A complete topology and its current observations replaced the prior projection atomically.
    SnapshotReplaced {
        /// Number of current observations committed with the snapshot.
        observation_count: usize,
        /// Greatest connection-local sequence committed, if the snapshot contained observations.
        last_sequence: Option<u64>,
    },
    /// One ordered observation advanced the current projection.
    ObservationApplied {
        /// Connection-local sequence that was committed.
        sequence: u64,
    },
}

/// Correlated receipt returned after an integration projection commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationProjectionReceipt {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    generation: TopologyGeneration,
    change: IntegrationProjectionChange,
}

impl IntegrationProjectionReceipt {
    /// Creates a receipt for one atomic complete-snapshot replacement.
    #[must_use]
    pub const fn snapshot_replaced(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        generation: TopologyGeneration,
        observation_count: usize,
        last_sequence: Option<u64>,
    ) -> Self {
        Self {
            gateway_id,
            integration_id,
            generation,
            change: IntegrationProjectionChange::SnapshotReplaced {
                observation_count,
                last_sequence,
            },
        }
    }

    /// Creates a receipt for one committed incremental observation.
    #[must_use]
    pub const fn observation_applied(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        generation: TopologyGeneration,
        sequence: u64,
    ) -> Self {
        Self {
            gateway_id,
            integration_id,
            generation,
            change: IntegrationProjectionChange::ObservationApplied { sequence },
        }
    }

    /// Returns the authoritative edge gateway scope.
    #[must_use]
    pub const fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    /// Returns the integration scope local to the gateway.
    #[must_use]
    pub const fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    /// Returns the topology generation against which the commit occurred.
    #[must_use]
    pub const fn generation(&self) -> TopologyGeneration {
        self.generation
    }

    /// Returns the typed projection change.
    #[must_use]
    pub const fn change(&self) -> IntegrationProjectionChange {
        self.change
    }

    /// Returns the committed incremental sequence, when this is an observation receipt.
    #[must_use]
    pub const fn sequence(&self) -> Option<u64> {
        match self.change {
            IntegrationProjectionChange::SnapshotReplaced { .. } => None,
            IntegrationProjectionChange::ObservationApplied { sequence } => Some(sequence),
        }
    }
}

/// Supplies a complete topology and ordered observations for externally managed devices.
///
/// The provider remains authoritative for the actual state of devices delegated
/// to it. Implementations must treat disconnects as unavailable, bound internal
/// queues, and perform a complete resynchronization after an event gap.
#[async_trait]
pub trait DelegatedDeviceProvider: Send + Sync + 'static {
    /// Returns the Aether gateway that owns this provider connection.
    fn gateway_id(&self) -> &GatewayIdentity;

    /// Returns the stable local identity of this provider connection.
    fn integration_id(&self) -> &IntegrationId;

    /// Reads one atomic topology and current-observation snapshot.
    ///
    /// The greatest initial observation sequence establishes the stream cursor;
    /// when there are no initial observations, the next sequence is one.
    async fn snapshot(&self) -> PortResult<IntegrationSnapshot>;

    /// Waits for the next ordered, typed point observation.
    ///
    /// Queue loss or a topology-generation change must return
    /// [`crate::PortErrorKind::Conflict`] and force the runtime to fetch a new
    /// complete topology and state snapshot before publishing more data.
    async fn next_observation(&self) -> PortResult<IntegrationObservation>;
}

/// Atomically maintains a read-only local projection of delegated integration state.
///
/// The projection is not a device-command path and does not replace Aether's
/// authoritative live-point store. Implementations must reject stale topology
/// generations, scope changes, undeclared points, and non-contiguous
/// observation sequences with [`crate::PortErrorKind::Conflict`] or
/// [`crate::PortErrorKind::InvalidData`]. A conflict requires a new complete
/// provider snapshot; it must not be repaired by fabricating observations.
#[async_trait]
pub trait IntegrationProjectionSink: Send + Sync + 'static {
    /// Atomically replaces topology and all current observations.
    ///
    /// No query may observe the new topology paired with observations from the
    /// previous generation, or vice versa. An equal generation may refresh
    /// current observations only when the topology digest is also equal; a
    /// changed digest without a newer generation is a conflict.
    async fn replace_snapshot(
        &self,
        snapshot: IntegrationSnapshot,
    ) -> PortResult<IntegrationProjectionReceipt>;

    /// Applies exactly one ordered observation against the expected generation.
    ///
    /// Implementations must compare `expected_generation` and the next
    /// connection-local sequence in the same transaction as the write.
    async fn apply_observation(
        &self,
        expected_generation: TopologyGeneration,
        observation: IntegrationObservation,
    ) -> PortResult<IntegrationProjectionReceipt>;
}

/// Reads the current atomic projection for one delegated integration.
#[async_trait]
pub trait IntegrationProjectionQuery: Send + Sync + 'static {
    /// Returns topology and current typed observations from one committed generation.
    async fn snapshot(
        &self,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>>;
}
