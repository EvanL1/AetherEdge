use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_cloudlink::{
    CLOUDLINK_INTEGRATION_EXTENSION, CloudLinkIntegrationExtension,
    CloudLinkIntegrationProjectionOutbox,
};
use aether_domain::{
    EntityId, EntityPointDescriptor, EntityRecord, ExternalAlias, GatewayIdentity, IntegrationId,
    IntegrationObservation, IntegrationPointKey, IntegrationPointKind, IntegrationSnapshot,
    IntegrationTopologySnapshot, ObservedValue, ObservedValueType, SnapshotDigest, TimestampMs,
    TopologyGeneration,
};
use aether_integration_contract::IntegrationContractCodec;
use aether_ports::{
    CloudLinkDurableAck, CloudLinkEnqueue, CloudLinkRecord, CloudLinkRecordIdentity,
    CloudLinkReplayWindow, CloudLinkSessionBinding, CloudLinkSpool, CloudLinkSpoolError,
    CloudLinkSpoolErrorReason, CloudLinkSpoolStatus, DurableAckOutcome, IntegrationProjectionQuery,
    IntegrationProjectionReceipt, IntegrationProjectionSink, PortResult,
};
use aether_store_local::FileCloudLinkSpool;
use async_trait::async_trait;
use tokio::sync::RwLock;

#[derive(Default)]
struct MemoryProjection {
    snapshot: RwLock<Option<IntegrationSnapshot>>,
}

#[async_trait]
impl IntegrationProjectionSink for MemoryProjection {
    async fn replace_snapshot(
        &self,
        snapshot: IntegrationSnapshot,
    ) -> PortResult<IntegrationProjectionReceipt> {
        let receipt = IntegrationProjectionReceipt::snapshot_replaced(
            snapshot.topology().gateway_id().clone(),
            snapshot.topology().integration_id().clone(),
            snapshot.topology().generation(),
            snapshot.observations().len(),
            snapshot
                .observations()
                .iter()
                .map(IntegrationObservation::sequence)
                .max(),
        );
        *self.snapshot.write().await = Some(snapshot);
        Ok(receipt)
    }

    async fn apply_observation(
        &self,
        expected_generation: TopologyGeneration,
        observation: IntegrationObservation,
    ) -> PortResult<IntegrationProjectionReceipt> {
        let mut guard = self.snapshot.write().await;
        let projected = guard.as_mut().expect("test projection initialized");
        let (topology, mut observations) = projected.clone().into_parts();
        assert_eq!(topology.generation(), expected_generation);
        observations.retain(|current| {
            current.entity_id() != observation.entity_id()
                || current.point_key() != observation.point_key()
        });
        observations.push(observation.clone());
        *projected =
            IntegrationSnapshot::new(topology, observations).expect("updated test projection");
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
        Ok(self.snapshot.read().await.clone().filter(|snapshot| {
            snapshot.topology().gateway_id() == gateway_id
                && snapshot.topology().integration_id() == integration_id
        }))
    }
}

struct FailingEnqueueSpool {
    inner: Arc<dyn CloudLinkSpool>,
    fail_next: AtomicBool,
}

impl FailingEnqueueSpool {
    fn once(inner: Arc<dyn CloudLinkSpool>) -> Self {
        Self {
            inner,
            fail_next: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl CloudLinkSpool for FailingEnqueueSpool {
    async fn enqueue(
        &self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(CloudLinkSpoolError::new(
                CloudLinkSpoolErrorReason::Storage,
                "injected crash before observation journal append",
            ));
        }
        self.inner.enqueue(input).await
    }

    async fn replay_from(
        &self,
        requested_position: u64,
        limit: usize,
    ) -> Result<CloudLinkReplayWindow, CloudLinkSpoolError> {
        self.inner.replay_from(requested_position, limit).await
    }

    async fn mark_offered(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        self.inner.mark_offered(identity, session).await
    }

    async fn mark_transport_published(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        self.inner.mark_transport_published(identity, session).await
    }

    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, CloudLinkSpoolError> {
        self.inner.acknowledge(ack).await
    }

    async fn status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError> {
        self.inner.status().await
    }

    async fn rotate_stream_epoch(&self) -> Result<u64, CloudLinkSpoolError> {
        self.inner.rotate_stream_epoch().await
    }
}

fn snapshot(observed_at_ms: u64, sequence: u64, value: bool) -> IntegrationSnapshot {
    let gateway_id = GatewayIdentity::new("33333333-3333-4333-8333-333333333333").expect("gateway");
    let integration_id = IntegrationId::new("home-assistant.home").expect("integration");
    let entity_id = EntityId::new("registry-switch-kitchen").expect("entity");
    let point_key = IntegrationPointKey::new("is_on").expect("point");
    let entity = EntityRecord::new(
        entity_id.clone(),
        "Kitchen switch",
        "switch",
        vec![
            EntityPointDescriptor::new(
                point_key.clone(),
                "Is on",
                IntegrationPointKind::State,
                ObservedValueType::Boolean,
                None,
            )
            .expect("descriptor"),
        ],
        None,
        None,
        vec![
            ExternalAlias::new("home-assistant", "entity-id", "switch.kitchen")
                .expect("source alias"),
        ],
    )
    .expect("entity record");
    let topology = IntegrationTopologySnapshot::new(
        gateway_id.clone(),
        integration_id.clone(),
        TopologyGeneration::new(7).expect("generation"),
        TimestampMs::new(observed_at_ms),
        SnapshotDigest::new(format!("sha256:{:064x}", 7)).expect("digest"),
        vec![],
        vec![],
        vec![entity],
    )
    .expect("topology");
    let observation = IntegrationObservation::available(
        gateway_id,
        integration_id,
        entity_id,
        point_key,
        ObservedValue::boolean(value),
        TimestampMs::new(observed_at_ms),
        sequence,
        Some("home-assistant-context"),
    )
    .expect("observation");
    IntegrationSnapshot::new(topology, vec![observation]).expect("snapshot")
}

async fn extension(
    topology: &dyn CloudLinkSpool,
    observations: &dyn CloudLinkSpool,
) -> CloudLinkIntegrationExtension {
    CloudLinkIntegrationExtension::enable_cloud_first(
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        "home-assistant.home",
        &topology.status().await.expect("topology status"),
        &observations.status().await.expect("observation status"),
    )
    .expect("Cloud-first extension")
}

#[tokio::test]
async fn committed_projection_reaches_two_independent_file_spools_through_strict_codecs() {
    let root = tempfile::tempdir().expect("temporary spool directory");
    let topology = Arc::new(
        FileCloudLinkSpool::open(root.path().join("topology.spool"), "ha-topology", 32)
            .expect("topology spool"),
    );
    let observations = Arc::new(
        FileCloudLinkSpool::open(
            root.path().join("observations.spool"),
            "ha-observations",
            32,
        )
        .expect("observation spool"),
    );
    let projection = Arc::new(MemoryProjection::default());
    let outbox = CloudLinkIntegrationProjectionOutbox::new(
        projection.clone(),
        projection,
        extension(topology.as_ref(), observations.as_ref()).await,
        topology.clone(),
        observations.clone(),
    );

    outbox
        .replace_snapshot(snapshot(1_784_217_600_000, 1, true))
        .await
        .expect("projection and outbox commit");

    let topology_records = topology.replay_from(1, 8).await.expect("topology replay");
    let observation_records = observations
        .replay_from(1, 8)
        .await
        .expect("observation replay");
    assert_eq!(topology_records.records().len(), 1);
    assert_eq!(observation_records.records().len(), 1);
    let public_topology =
        IntegrationContractCodec::decode_topology(topology_records.records()[0].payload())
            .expect("strict topology payload");
    assert_eq!(
        public_topology.entities()[0].points()[0].point_key(),
        "is_on"
    );
    let public_observations = IntegrationContractCodec::decode_observation_batch(
        observation_records.records()[0].payload(),
        &public_topology,
    )
    .expect("strict observation payload");
    assert_eq!(public_observations.observations()[0].point_key(), "is_on");
}

#[tokio::test]
async fn startup_full_snapshot_recovers_a_crash_after_projection_commit_before_spool_append() {
    let root = tempfile::tempdir().expect("temporary spool directory");
    let topology_path = root.path().join("topology.spool");
    let observation_path = root.path().join("observations.spool");
    {
        let topology = Arc::new(
            FileCloudLinkSpool::open(&topology_path, "ha-topology", 32).expect("topology spool"),
        );
        let observations = Arc::new(
            FileCloudLinkSpool::open(&observation_path, "ha-observations", 32)
                .expect("observation spool"),
        );
        let failing_observations: Arc<dyn CloudLinkSpool> =
            Arc::new(FailingEnqueueSpool::once(observations));
        let projection = Arc::new(MemoryProjection::default());
        let outbox = CloudLinkIntegrationProjectionOutbox::new(
            projection.clone(),
            projection.clone(),
            extension(topology.as_ref(), failing_observations.as_ref()).await,
            topology,
            failing_observations,
        );

        outbox
            .replace_snapshot(snapshot(1_784_217_600_000, 1, false))
            .await
            .expect_err("injected crash window");
        assert!(
            projection
                .snapshot(
                    &GatewayIdentity::new("33333333-3333-4333-8333-333333333333").expect("gateway"),
                    &IntegrationId::new("home-assistant.home").expect("integration"),
                )
                .await
                .expect("projection query")
                .is_some(),
            "the simulated failure occurs after the local projection commit"
        );
    }

    let topology = Arc::new(
        FileCloudLinkSpool::open(&topology_path, "ha-topology", 32).expect("reopen topology"),
    );
    let observations = Arc::new(
        FileCloudLinkSpool::open(&observation_path, "ha-observations", 32)
            .expect("reopen observations"),
    );
    let projection = Arc::new(MemoryProjection::default());
    let recovered = CloudLinkIntegrationProjectionOutbox::new(
        projection.clone(),
        projection,
        extension(topology.as_ref(), observations.as_ref()).await,
        topology.clone(),
        observations.clone(),
    );

    recovered
        .replace_snapshot(snapshot(1_784_217_600_100, 1, true))
        .await
        .expect("startup complete snapshot resends current state");

    assert_eq!(
        observations
            .status()
            .await
            .expect("observation status")
            .pending_records(),
        1
    );
    let topology_window = topology.replay_from(1, 8).await.expect("topology replay");
    let observation_window = observations
        .replay_from(1, 8)
        .await
        .expect("observation replay");
    let latest_topology = topology_window.records().last().expect("topology record");
    let public_topology = IntegrationContractCodec::decode_topology(latest_topology.payload())
        .expect("strict recovered topology");
    IntegrationContractCodec::decode_observation_batch(
        observation_window.records()[0].payload(),
        &public_topology,
    )
    .expect("strict recovered observations");
}
