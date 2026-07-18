//! Projection commit boundary that reliably feeds the Integration CloudLink streams.

use std::fmt::Display;
use std::sync::Arc;

use aether_domain::{
    GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationSnapshot, TopologyGeneration,
};
use aether_integration_contract::{
    HomeAssistantV1Alpha1Profile, IntegrationContractCodec, IntegrationObservationBatchV1Alpha1,
    IntegrationTopologySnapshotV1Alpha1,
};
use aether_ports::{
    CloudLinkSpool, IntegrationProjectionChange, IntegrationProjectionQuery,
    IntegrationProjectionReceipt, IntegrationProjectionSink, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::CloudLinkIntegrationExtension;

/// Commits one delegated-device projection and feeds two independent durable streams.
///
/// The local projection remains authoritative for the synchronized read model.
/// A spool failure is returned to the application synchronizer, which must fetch
/// and resend a complete provider snapshot before processing another incremental
/// observation. Startup uses that same complete-snapshot path, closing the
/// process-crash window after a projection commit and before a journal append.
pub struct CloudLinkIntegrationProjectionOutbox {
    projection_sink: Arc<dyn IntegrationProjectionSink>,
    projection_query: Arc<dyn IntegrationProjectionQuery>,
    extension: CloudLinkIntegrationExtension,
    topology_spool: Arc<dyn CloudLinkSpool>,
    observation_spool: Arc<dyn CloudLinkSpool>,
    operation: Mutex<()>,
}

impl CloudLinkIntegrationProjectionOutbox {
    /// Composes an already Cloud-first-enabled extension with its committed projection.
    #[must_use]
    pub fn new(
        projection_sink: Arc<dyn IntegrationProjectionSink>,
        projection_query: Arc<dyn IntegrationProjectionQuery>,
        extension: CloudLinkIntegrationExtension,
        topology_spool: Arc<dyn CloudLinkSpool>,
        observation_spool: Arc<dyn CloudLinkSpool>,
    ) -> Self {
        Self {
            projection_sink,
            projection_query,
            extension,
            topology_spool,
            observation_spool,
            operation: Mutex::new(()),
        }
    }

    async fn committed_snapshot(
        &self,
        receipt: &IntegrationProjectionReceipt,
    ) -> PortResult<IntegrationSnapshot> {
        let snapshot = self
            .projection_query
            .snapshot(receipt.gateway_id(), receipt.integration_id())
            .await?
            .ok_or_else(|| conflict("committed integration projection cannot be read back"))?;
        if snapshot.topology().gateway_id() != receipt.gateway_id()
            || snapshot.topology().integration_id() != receipt.integration_id()
            || snapshot.topology().generation() != receipt.generation()
        {
            return Err(conflict(
                "committed integration projection does not match its receipt",
            ));
        }
        Ok(snapshot)
    }

    async fn enqueue_complete_snapshot(&self, snapshot: &IntegrationSnapshot) -> PortResult<()> {
        let public_topology = public_topology(snapshot)?;
        let topology_input = self
            .extension
            .prepare_topology(&public_topology, snapshot.topology().observed_at(), None)
            .map_err(contract_error)?;
        self.ensure_topology_enqueued(topology_input).await?;
        self.enqueue_observations(
            snapshot,
            &public_topology,
            "snapshot",
            snapshot.observations(),
        )
        .await
    }

    async fn ensure_topology_enqueued(
        &self,
        input: aether_ports::CloudLinkEnqueue,
    ) -> PortResult<()> {
        let status = self.topology_spool.status().await.map_err(spool_error)?;
        if status
            .last_ack()
            .is_some_and(|ack| ack.batch_id() == input.batch_id())
        {
            return Ok(());
        }
        if status.pending_records() > 0 {
            let pending = self
                .topology_spool
                .replay_from(
                    status.earliest_retained_position(),
                    status.pending_records(),
                )
                .await
                .map_err(spool_error)?;
            if let Some(existing) = pending
                .records()
                .iter()
                .find(|record| record.batch_id() == input.batch_id())
            {
                let topology = IntegrationContractCodec::decode_topology(existing.payload())
                    .map_err(contract_error)?;
                if topology.integration_id() != self.extension.integration_id() {
                    return Err(conflict(
                        "retained Integration topology belongs to another integration",
                    ));
                }
                return Ok(());
            }
        }
        self.topology_spool
            .enqueue(input)
            .await
            .map_err(spool_error)?;
        Ok(())
    }

    async fn enqueue_observations(
        &self,
        snapshot: &IntegrationSnapshot,
        public_topology: &IntegrationTopologySnapshotV1Alpha1,
        identity_prefix: &str,
        observations: &[IntegrationObservation],
    ) -> PortResult<()> {
        if observations.is_empty() {
            return Ok(());
        }
        let batch = deterministic_observation_batch(
            snapshot,
            identity_prefix,
            observations,
            public_topology,
        )?;
        let created_at = observations
            .iter()
            .map(IntegrationObservation::observed_at)
            .max_by_key(|timestamp| timestamp.get())
            .unwrap_or_else(|| snapshot.topology().observed_at());
        let inputs = self
            .extension
            .prepare_observation_batches(public_topology, &batch, created_at, None)
            .map_err(contract_error)?;
        for input in inputs {
            self.observation_spool
                .enqueue(input)
                .await
                .map_err(spool_error)?;
        }
        Ok(())
    }
}

#[async_trait]
impl IntegrationProjectionSink for CloudLinkIntegrationProjectionOutbox {
    async fn replace_snapshot(
        &self,
        snapshot: IntegrationSnapshot,
    ) -> PortResult<IntegrationProjectionReceipt> {
        let _operation = self.operation.lock().await;
        let receipt = self.projection_sink.replace_snapshot(snapshot).await?;
        let committed = self.committed_snapshot(&receipt).await?;
        let expected_last_sequence = committed
            .observations()
            .iter()
            .map(IntegrationObservation::sequence)
            .max();
        if receipt.change()
            != (IntegrationProjectionChange::SnapshotReplaced {
                observation_count: committed.observations().len(),
                last_sequence: expected_last_sequence,
            })
        {
            return Err(conflict(
                "integration snapshot receipt does not describe the committed projection",
            ));
        }
        self.enqueue_complete_snapshot(&committed).await?;
        Ok(receipt)
    }

    async fn apply_observation(
        &self,
        expected_generation: TopologyGeneration,
        observation: IntegrationObservation,
    ) -> PortResult<IntegrationProjectionReceipt> {
        let _operation = self.operation.lock().await;
        let committed_observation = observation.clone();
        let receipt = self
            .projection_sink
            .apply_observation(expected_generation, observation)
            .await?;
        if receipt.change()
            != (IntegrationProjectionChange::ObservationApplied {
                sequence: committed_observation.sequence(),
            })
        {
            return Err(conflict(
                "integration observation receipt does not describe the committed projection",
            ));
        }
        let committed = self.committed_snapshot(&receipt).await?;
        let public_topology = public_topology(&committed)?;
        self.enqueue_observations(
            &committed,
            &public_topology,
            "observation",
            std::slice::from_ref(&committed_observation),
        )
        .await?;
        Ok(receipt)
    }
}

#[async_trait]
impl IntegrationProjectionQuery for CloudLinkIntegrationProjectionOutbox {
    async fn snapshot(
        &self,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>> {
        self.projection_query
            .snapshot(gateway_id, integration_id)
            .await
    }
}

fn public_topology(
    snapshot: &IntegrationSnapshot,
) -> PortResult<IntegrationTopologySnapshotV1Alpha1> {
    IntegrationContractCodec::topology_from_domain(
        snapshot.topology(),
        &HomeAssistantV1Alpha1Profile,
    )
    .map_err(contract_error)
}

fn deterministic_observation_batch(
    snapshot: &IntegrationSnapshot,
    identity_prefix: &str,
    observations: &[IntegrationObservation],
    public_topology: &IntegrationTopologySnapshotV1Alpha1,
) -> PortResult<IntegrationObservationBatchV1Alpha1> {
    let candidate = IntegrationContractCodec::observation_batch_from_domain(
        snapshot.topology(),
        "identity-candidate",
        observations,
    )
    .map_err(contract_error)?;
    let candidate_bytes =
        IntegrationContractCodec::encode_observation_batch(&candidate, public_topology)
            .map_err(contract_error)?;
    let digest = Sha256::digest(candidate_bytes);
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let batch_id = format!(
        "{identity_prefix}-{}-{suffix}",
        snapshot.topology().generation().get()
    );
    IntegrationContractCodec::observation_batch_from_domain(
        snapshot.topology(),
        &batch_id,
        observations,
    )
    .map_err(contract_error)
}

fn conflict(message: &str) -> PortError {
    PortError::new(PortErrorKind::Conflict, message)
}

fn contract_error(error: impl Display) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        format!("Integration CloudLink contract projection failed: {error}"),
    )
}

fn spool_error(error: impl Display) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("Integration CloudLink durable spool is unavailable: {error}"),
    )
}
