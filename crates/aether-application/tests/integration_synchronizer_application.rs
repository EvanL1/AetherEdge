use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aether_application::{
    IntegrationResyncReason, IntegrationSynchronizationError, IntegrationSynchronizer,
};
use aether_domain::{
    EntityId, EntityPointDescriptor, EntityRecord, GatewayIdentity, IntegrationId,
    IntegrationObservation, IntegrationPointKey, IntegrationPointKind, IntegrationSnapshot,
    IntegrationTopologySnapshot, ObservedValue, ObservedValueType, SnapshotDigest, TimestampMs,
    TopologyGeneration,
};
use aether_ports::{
    DelegatedDeviceProvider, IntegrationProjectionQuery, IntegrationProjectionReceipt,
    IntegrationProjectionSink, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

fn gateway_id() -> GatewayIdentity {
    GatewayIdentity::new("gateway-home").expect("gateway identity")
}

fn integration_id() -> IntegrationId {
    IntegrationId::new("home-assistant-home").expect("integration identity")
}

fn entity_id() -> EntityId {
    EntityId::new("entity-registry-17").expect("entity identity")
}

fn point_key() -> IntegrationPointKey {
    IntegrationPointKey::new("state").expect("point key")
}

fn topology(generation: u64) -> IntegrationTopologySnapshot {
    scoped_topology(gateway_id(), integration_id(), generation)
}

fn scoped_topology(
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    generation: u64,
) -> IntegrationTopologySnapshot {
    let entity = EntityRecord::new(
        entity_id(),
        "Kitchen switch",
        "switch",
        vec![
            EntityPointDescriptor::new(
                point_key(),
                "State",
                IntegrationPointKind::State,
                ObservedValueType::Boolean,
                None,
            )
            .expect("point descriptor"),
        ],
        None,
        None,
        vec![],
    )
    .expect("entity");
    IntegrationTopologySnapshot::new(
        gateway_id,
        integration_id,
        TopologyGeneration::new(generation).expect("generation"),
        TimestampMs::new(10),
        SnapshotDigest::new(format!("sha256:{generation:064x}")).expect("digest"),
        vec![],
        vec![],
        vec![entity],
    )
    .expect("topology")
}

fn observation(
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    entity_id: EntityId,
    point_key: IntegrationPointKey,
    value: ObservedValue,
    sequence: u64,
) -> IntegrationObservation {
    IntegrationObservation::available(
        gateway_id,
        integration_id,
        entity_id,
        point_key,
        value,
        TimestampMs::new(10 + sequence),
        sequence,
        None,
    )
    .expect("observation")
}

fn initial_snapshot() -> IntegrationSnapshot {
    IntegrationSnapshot::new(
        topology(7),
        vec![observation(
            gateway_id(),
            integration_id(),
            entity_id(),
            point_key(),
            ObservedValue::boolean(false),
            1,
        )],
    )
    .expect("snapshot")
}

struct ScriptedProvider {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    snapshot: IntegrationSnapshot,
    events: Mutex<Vec<&'static str>>,
    observations: Mutex<VecDeque<PortResult<IntegrationObservation>>>,
}

impl ScriptedProvider {
    fn new(observations: Vec<PortResult<IntegrationObservation>>) -> Self {
        Self {
            gateway_id: gateway_id(),
            integration_id: integration_id(),
            snapshot: initial_snapshot(),
            events: Mutex::new(Vec::new()),
            observations: Mutex::new(observations.into()),
        }
    }
}

#[async_trait]
impl DelegatedDeviceProvider for ScriptedProvider {
    fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    async fn snapshot(&self) -> PortResult<IntegrationSnapshot> {
        self.events.lock().expect("event lock").push("snapshot");
        Ok(self.snapshot.clone())
    }

    async fn next_observation(&self) -> PortResult<IntegrationObservation> {
        self.events
            .lock()
            .expect("event lock")
            .push("next_observation");
        self.observations
            .lock()
            .expect("observation lock")
            .pop_front()
            .unwrap_or_else(|| {
                Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "script is empty",
                ))
            })
    }
}

#[derive(Default)]
struct MemoryProjection {
    snapshot: Mutex<Option<IntegrationSnapshot>>,
    events: Mutex<Vec<&'static str>>,
    fail_apply_with_conflict: Mutex<bool>,
    lie_about_replace_receipt: Mutex<bool>,
}

impl MemoryProjection {
    fn max_sequence(snapshot: &IntegrationSnapshot) -> Option<u64> {
        snapshot
            .observations()
            .iter()
            .map(IntegrationObservation::sequence)
            .max()
    }
}

#[async_trait]
impl IntegrationProjectionSink for MemoryProjection {
    async fn replace_snapshot(
        &self,
        snapshot: IntegrationSnapshot,
    ) -> PortResult<IntegrationProjectionReceipt> {
        self.events.lock().expect("event lock").push("replace");
        let topology = snapshot.topology();
        let generation = if *self.lie_about_replace_receipt.lock().expect("receipt lock") {
            TopologyGeneration::new(topology.generation().get() + 1).expect("next generation")
        } else {
            topology.generation()
        };
        let receipt = IntegrationProjectionReceipt::snapshot_replaced(
            topology.gateway_id().clone(),
            topology.integration_id().clone(),
            generation,
            snapshot.observations().len(),
            Self::max_sequence(&snapshot),
        );
        *self.snapshot.lock().expect("snapshot lock") = Some(snapshot);
        Ok(receipt)
    }

    async fn apply_observation(
        &self,
        expected_generation: TopologyGeneration,
        observation: IntegrationObservation,
    ) -> PortResult<IntegrationProjectionReceipt> {
        self.events.lock().expect("event lock").push("apply");
        if *self.fail_apply_with_conflict.lock().expect("conflict lock") {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "projection generation changed",
            ));
        }

        let mut guard = self.snapshot.lock().expect("snapshot lock");
        let projected = guard.as_mut().ok_or_else(|| {
            PortError::new(PortErrorKind::Conflict, "projection is not initialized")
        })?;
        if projected.topology().generation() != expected_generation {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "projection generation changed",
            ));
        }
        let (topology, mut observations) = projected.clone().into_parts();
        let current_sequence = observations
            .iter()
            .map(IntegrationObservation::sequence)
            .max()
            .unwrap_or(0);
        if observation.sequence() != current_sequence + 1 {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "observation sequence has a gap",
            ));
        }
        if let Some(existing) = observations.iter_mut().find(|existing| {
            existing.entity_id() == observation.entity_id()
                && existing.point_key() == observation.point_key()
        }) {
            *existing = observation.clone();
        } else {
            observations.push(observation.clone());
        }
        *projected = IntegrationSnapshot::new(topology, observations)
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        Ok(IntegrationProjectionReceipt::observation_applied(
            observation.gateway_id().clone(),
            observation.integration_id().clone(),
            expected_generation,
            observation.sequence(),
        ))
    }
}

#[async_trait]
impl IntegrationProjectionQuery for MemoryProjection {
    async fn snapshot(
        &self,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>> {
        self.events.lock().expect("event lock").push("query");
        Ok(self
            .snapshot
            .lock()
            .expect("snapshot lock")
            .clone()
            .filter(|snapshot| {
                snapshot.topology().gateway_id() == gateway_id
                    && snapshot.topology().integration_id() == integration_id
            }))
    }
}

fn make_synchronizer(
    provider: Arc<ScriptedProvider>,
    projection: Arc<MemoryProjection>,
) -> IntegrationSynchronizer {
    IntegrationSynchronizer::new(provider, projection.clone(), projection)
}

#[tokio::test]
async fn snapshot_is_projected_atomically_before_incremental_observations() {
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let projection = Arc::new(MemoryProjection::default());
    let synchronizer = make_synchronizer(provider.clone(), projection.clone());

    let receipt = synchronizer
        .synchronize_snapshot()
        .await
        .expect("snapshot synchronization");

    assert_eq!(receipt.generation().get(), 7);
    assert_eq!(
        provider
            .events
            .lock()
            .expect("provider event lock")
            .as_slice(),
        ["snapshot"]
    );
    assert_eq!(
        projection
            .events
            .lock()
            .expect("projection event lock")
            .as_slice(),
        ["replace"]
    );
    assert_eq!(
        projection.snapshot.lock().expect("snapshot lock").as_ref(),
        Some(&initial_snapshot())
    );
}

#[tokio::test]
async fn one_declared_ordered_observation_is_applied_per_call() {
    let next = observation(
        gateway_id(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(true),
        2,
    );
    let provider = Arc::new(ScriptedProvider::new(vec![Ok(next.clone())]));
    let projection = Arc::new(MemoryProjection::default());
    let synchronizer = make_synchronizer(provider.clone(), projection.clone());
    synchronizer
        .synchronize_snapshot()
        .await
        .expect("snapshot synchronization");

    let receipt = synchronizer
        .synchronize_next()
        .await
        .expect("observation synchronization");

    assert_eq!(receipt.sequence(), Some(2));
    assert_eq!(
        provider
            .events
            .lock()
            .expect("provider event lock")
            .as_slice(),
        ["snapshot", "next_observation"]
    );
    assert_eq!(
        projection
            .events
            .lock()
            .expect("projection event lock")
            .as_slice(),
        ["replace", "query", "apply"]
    );
    assert_eq!(
        projection
            .snapshot
            .lock()
            .expect("snapshot lock")
            .as_ref()
            .expect("projection")
            .observations(),
        &[next]
    );
}

#[tokio::test]
async fn undeclared_or_wrongly_typed_observation_requires_resync_without_sink_write() {
    for invalid in [
        observation(
            gateway_id(),
            integration_id(),
            EntityId::new("unknown-entity").expect("entity id"),
            point_key(),
            ObservedValue::boolean(true),
            2,
        ),
        observation(
            gateway_id(),
            integration_id(),
            entity_id(),
            point_key(),
            ObservedValue::string("on").expect("string value"),
            2,
        ),
    ] {
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(invalid)]));
        let projection = Arc::new(MemoryProjection::default());
        let synchronizer = make_synchronizer(provider, projection.clone());
        synchronizer
            .synchronize_snapshot()
            .await
            .expect("snapshot synchronization");

        let error = synchronizer
            .synchronize_next()
            .await
            .expect_err("invalid observation must fail closed");

        assert!(error.requires_resync());
        assert!(matches!(
            error,
            IntegrationSynchronizationError::ResyncRequired {
                reason: IntegrationResyncReason::UndeclaredPoint
                    | IntegrationResyncReason::ValueTypeChanged,
                ..
            }
        ));
        assert_eq!(
            projection
                .events
                .lock()
                .expect("projection event lock")
                .iter()
                .filter(|event| **event == "apply")
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn sequence_gap_or_projection_conflict_requires_explicit_resync_without_retry() {
    let gap = observation(
        gateway_id(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(true),
        3,
    );
    let provider = Arc::new(ScriptedProvider::new(vec![Ok(gap)]));
    let projection = Arc::new(MemoryProjection::default());
    let synchronizer = make_synchronizer(provider.clone(), projection.clone());
    synchronizer
        .synchronize_snapshot()
        .await
        .expect("snapshot synchronization");

    let gap_error = synchronizer
        .synchronize_next()
        .await
        .expect_err("sequence gap must require resync");
    assert!(matches!(
        gap_error,
        IntegrationSynchronizationError::ResyncRequired {
            reason: IntegrationResyncReason::SequenceGap,
            ..
        }
    ));
    assert_eq!(
        provider
            .events
            .lock()
            .expect("provider event lock")
            .as_slice(),
        ["snapshot", "next_observation"]
    );

    let next = observation(
        gateway_id(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(true),
        2,
    );
    let provider = Arc::new(ScriptedProvider::new(vec![Ok(next)]));
    let projection = Arc::new(MemoryProjection::default());
    *projection
        .fail_apply_with_conflict
        .lock()
        .expect("conflict lock") = true;
    let synchronizer = make_synchronizer(provider, projection.clone());
    synchronizer
        .synchronize_snapshot()
        .await
        .expect("snapshot synchronization");

    let conflict = synchronizer
        .synchronize_next()
        .await
        .expect_err("projection conflict must require resync");
    assert!(matches!(
        conflict,
        IntegrationSynchronizationError::ResyncRequired {
            reason: IntegrationResyncReason::ProjectionConflict,
            ..
        }
    ));
    assert_eq!(
        projection
            .events
            .lock()
            .expect("projection event lock")
            .iter()
            .filter(|event| **event == "apply")
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_stream_conflict_requires_resync_and_is_not_retried() {
    let provider = Arc::new(ScriptedProvider::new(vec![Err(PortError::new(
        PortErrorKind::Conflict,
        "upstream event queue overflowed",
    ))]));
    let projection = Arc::new(MemoryProjection::default());
    let synchronizer = make_synchronizer(provider.clone(), projection.clone());
    synchronizer
        .synchronize_snapshot()
        .await
        .expect("snapshot synchronization");

    let error = synchronizer
        .synchronize_next()
        .await
        .expect_err("provider stream gap must require resync");

    assert!(matches!(
        error,
        IntegrationSynchronizationError::ResyncRequired {
            reason: IntegrationResyncReason::ProviderStreamGap,
            ..
        }
    ));
    assert_eq!(
        provider
            .events
            .lock()
            .expect("provider event lock")
            .as_slice(),
        ["snapshot", "next_observation"]
    );
    assert_eq!(
        projection
            .events
            .lock()
            .expect("projection event lock")
            .as_slice(),
        ["replace"]
    );
}

#[tokio::test]
async fn missing_projection_consumes_no_sink_write_and_requires_snapshot() {
    let next = observation(
        gateway_id(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(true),
        2,
    );
    let provider = Arc::new(ScriptedProvider::new(vec![Ok(next)]));
    let projection = Arc::new(MemoryProjection::default());
    let synchronizer = make_synchronizer(provider, projection.clone());

    let error = synchronizer
        .synchronize_next()
        .await
        .expect_err("incremental state needs an initialized projection");

    assert_eq!(
        error.resync_reason(),
        Some(IntegrationResyncReason::ProjectionMissing)
    );
    assert_eq!(
        projection
            .events
            .lock()
            .expect("projection event lock")
            .as_slice(),
        ["query"]
    );
}

#[tokio::test]
async fn provider_snapshot_scope_and_projection_receipt_are_verified_before_acceptance() {
    let wrong_gateway = GatewayIdentity::new("gateway-other").expect("gateway identity");
    let wrong_observation = observation(
        wrong_gateway.clone(),
        integration_id(),
        entity_id(),
        point_key(),
        ObservedValue::boolean(false),
        1,
    );
    let provider = Arc::new(ScriptedProvider {
        gateway_id: gateway_id(),
        integration_id: integration_id(),
        snapshot: IntegrationSnapshot::new(
            scoped_topology(wrong_gateway, integration_id(), 7),
            vec![wrong_observation],
        )
        .expect("scoped snapshot"),
        events: Mutex::new(Vec::new()),
        observations: Mutex::new(VecDeque::new()),
    });
    let projection = Arc::new(MemoryProjection::default());
    let synchronizer = make_synchronizer(provider, projection.clone());

    let scope_error = synchronizer
        .synchronize_snapshot()
        .await
        .expect_err("provider scope mismatch must fail closed");
    assert!(matches!(
        scope_error,
        IntegrationSynchronizationError::ProviderScopeMismatch
    ));
    assert!(
        projection
            .events
            .lock()
            .expect("projection event lock")
            .is_empty()
    );

    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let projection = Arc::new(MemoryProjection::default());
    *projection
        .lie_about_replace_receipt
        .lock()
        .expect("receipt lock") = true;
    let synchronizer = make_synchronizer(provider, projection);
    let receipt_error = synchronizer
        .synchronize_snapshot()
        .await
        .expect_err("uncorrelated receipt must fail closed");
    assert!(matches!(
        receipt_error,
        IntegrationSynchronizationError::InvalidProjectionReceipt
    ));
}
