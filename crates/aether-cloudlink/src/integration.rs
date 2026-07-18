//! Explicit Cloud-first Integration extension and bounded producer preparation.

use aether_domain::TimestampMs;
use aether_integration_contract::{
    IntegrationContractCodec, IntegrationObservationBatchV1Alpha1,
    IntegrationTopologySnapshotV1Alpha1, OBSERVATION_BATCH_SCHEMA,
};
use aether_ports::{
    CloudLinkEnqueue, CloudLinkMessageKind, CloudLinkRecord, CloudLinkSpoolStatus,
    CloudLinkTransportRoute,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    CLOUDLINK_INTEGRATION_EXTENSION, CLOUDLINK_PROTOCOL, CLOUDLINK_PROTOCOL_VERSION,
    CloudLinkCodec, CloudLinkCodecError, MAX_CLOUDLINK_MESSAGE_BYTES,
};

const MAX_BATCH_ID_BYTES: usize = 128;
const MAX_STREAM_ID_BYTES: usize = 128;
const MAX_PARTITIONS: usize = 65_536;
const MAX_U64: &str = "18446744073709551615";
const MAX_TRACEPARENT: &str = "00-11111111111111111111111111111111-2222222222222222-01";

/// Explicitly activated producer boundary for one delegated Integration.
///
/// Construction proves that the current Runtime Manifest declares the exact
/// extension, the Cloud consumer was enabled first, and topology and
/// observations own different durable streams for their current epochs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkIntegrationExtension {
    integration_id: String,
    topology_stream_id: String,
    topology_stream_epoch: u64,
    observation_stream_id: String,
    observation_stream_epoch: u64,
}

impl CloudLinkIntegrationExtension {
    /// Activates one Integration only after explicit Cloud-first rollout gates.
    pub fn enable_cloud_first(
        runtime_protocols: &[&str],
        cloud_consumer_extensions: &[&str],
        integration_id: &str,
        topology_stream: &CloudLinkSpoolStatus,
        observation_stream: &CloudLinkSpoolStatus,
    ) -> Result<Self, CloudLinkCodecError> {
        let runtime_declares = runtime_protocols.contains(&CLOUDLINK_INTEGRATION_EXTENSION);
        let cloud_enabled = cloud_consumer_extensions.contains(&CLOUDLINK_INTEGRATION_EXTENSION);
        if !runtime_declares || !cloud_enabled {
            return Err(CloudLinkCodecError::IntegrationExtensionNotEnabled);
        }
        validate_integration_id(integration_id)?;
        if topology_stream.stream_id() == observation_stream.stream_id()
            || topology_stream.stream_epoch() == 0
            || observation_stream.stream_epoch() == 0
        {
            return Err(CloudLinkCodecError::IntegrationStreamBindingConflict);
        }
        Ok(Self {
            integration_id: integration_id.to_string(),
            topology_stream_id: topology_stream.stream_id().to_string(),
            topology_stream_epoch: topology_stream.stream_epoch(),
            observation_stream_id: observation_stream.stream_id().to_string(),
            observation_stream_epoch: observation_stream.stream_epoch(),
        })
    }

    /// Returns the configured Integration identity.
    #[must_use]
    pub fn integration_id(&self) -> &str {
        &self.integration_id
    }

    /// Seals exactly one complete, atomic topology replacement.
    ///
    /// An oversized replacement fails before it can enter a durable spool.
    pub fn prepare_topology(
        &self,
        topology: &IntegrationTopologySnapshotV1Alpha1,
        created_at: TimestampMs,
        expires_at: Option<TimestampMs>,
    ) -> Result<CloudLinkEnqueue, CloudLinkCodecError> {
        self.validate_integration(topology.integration_id())?;
        let payload_bytes = IntegrationContractCodec::encode_topology(topology)?;
        let payload: Value = serde_json::from_slice(&payload_bytes)?;
        let batch_id = format!("topology-{}", topology.snapshot_generation());
        prepare_integration_value(
            CloudLinkMessageKind::IntegrationTopologySnapshot,
            batch_id,
            payload,
            created_at,
            expires_at,
        )
    }

    /// Seals one batch or partitions it only at complete observation boundaries.
    ///
    /// Every returned item is an independently valid public Integration batch
    /// with a distinct identity and a worst-case complete envelope no larger
    /// than 256 KiB.
    pub fn prepare_observation_batches(
        &self,
        topology: &IntegrationTopologySnapshotV1Alpha1,
        batch: &IntegrationObservationBatchV1Alpha1,
        created_at: TimestampMs,
        expires_at: Option<TimestampMs>,
    ) -> Result<Vec<CloudLinkEnqueue>, CloudLinkCodecError> {
        self.validate_integration(topology.integration_id())?;
        self.validate_integration(batch.integration_id())?;
        let original_bytes = IntegrationContractCodec::encode_observation_batch(batch, topology)?;
        let original_payload: Value = serde_json::from_slice(&original_bytes)?;
        match prepare_integration_value(
            CloudLinkMessageKind::IntegrationObservationBatch,
            batch.batch_id().to_string(),
            original_payload,
            created_at,
            expires_at,
        ) {
            Ok(prepared) => return Ok(vec![prepared]),
            Err(CloudLinkCodecError::MessageTooLarge { .. }) => {},
            Err(error) => return Err(error),
        }

        let observations = batch
            .observations()
            .iter()
            .map(|observation| {
                let value = serde_json::to_value(observation)
                    .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
                let bytes = serde_json_canonicalizer::to_vec(&value)
                    .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
                Ok((value, bytes.len()))
            })
            .collect::<Result<Vec<_>, CloudLinkCodecError>>()?;
        let mut prepared = Vec::new();
        let mut cursor = 0;
        let identity_hash = partition_identity_hash(batch);

        while cursor < observations.len() {
            if prepared.len() >= MAX_PARTITIONS {
                return Err(CloudLinkCodecError::MessageTooLarge {
                    found: observations.len(),
                    maximum: MAX_PARTITIONS,
                });
            }
            let batch_id =
                partition_batch_id(batch.batch_id(), prepared.len() + 1, identity_hash.as_str());
            let empty_payload = observation_payload(batch, &batch_id, Vec::new());
            let base_size = maximum_complete_envelope_size(
                CloudLinkMessageKind::IntegrationObservationBatch,
                &batch_id,
                &empty_payload,
                expires_at.is_some(),
            )?;
            let mut current = Vec::new();
            let mut current_size = base_size;
            while let Some((observation, observation_size)) = observations.get(cursor) {
                let separator = usize::from(!current.is_empty());
                let next_size = current_size
                    .checked_add(separator)
                    .and_then(|size| size.checked_add(*observation_size))
                    .ok_or(CloudLinkCodecError::MessageTooLarge {
                        found: usize::MAX,
                        maximum: MAX_CLOUDLINK_MESSAGE_BYTES,
                    })?;
                if next_size > MAX_CLOUDLINK_MESSAGE_BYTES {
                    if current.is_empty() {
                        return Err(CloudLinkCodecError::MessageTooLarge {
                            found: next_size,
                            maximum: MAX_CLOUDLINK_MESSAGE_BYTES,
                        });
                    }
                    break;
                }
                current.push(observation.clone());
                current_size = next_size;
                cursor += 1;
            }

            let payload = observation_payload(batch, &batch_id, current);
            let payload_bytes = serde_json_canonicalizer::to_vec(&payload)
                .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
            let validated =
                IntegrationContractCodec::decode_observation_batch(&payload_bytes, topology)?;
            let validated_payload = serde_json::to_value(validated)
                .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?;
            prepared.push(prepare_integration_value(
                CloudLinkMessageKind::IntegrationObservationBatch,
                batch_id,
                validated_payload,
                created_at,
                expires_at,
            )?);
        }
        Ok(prepared)
    }

    /// Verifies that an enqueued record stayed on its immutable configured stream.
    pub fn route_for_record(
        &self,
        record: &CloudLinkRecord,
    ) -> Result<CloudLinkTransportRoute, CloudLinkCodecError> {
        let (expected_stream, expected_epoch, route) = match record.message_kind() {
            CloudLinkMessageKind::IntegrationTopologySnapshot => (
                self.topology_stream_id.as_str(),
                self.topology_stream_epoch,
                CloudLinkTransportRoute::IntegrationTopologyUp,
            ),
            CloudLinkMessageKind::IntegrationObservationBatch => (
                self.observation_stream_id.as_str(),
                self.observation_stream_epoch,
                CloudLinkTransportRoute::IntegrationObservationsUp,
            ),
            _ => return Err(CloudLinkCodecError::IntegrationStreamBindingConflict),
        };
        if record.identity().stream_id() != expected_stream
            || record.identity().stream_epoch() != expected_epoch
        {
            return Err(CloudLinkCodecError::IntegrationStreamBindingConflict);
        }
        let payload: Value = serde_json::from_slice(record.payload())?;
        let integration_id = payload
            .get("integration_id")
            .and_then(Value::as_str)
            .ok_or(CloudLinkCodecError::IntegrationStreamBindingConflict)?;
        self.validate_integration(integration_id)?;
        Ok(route)
    }

    fn validate_integration(&self, integration_id: &str) -> Result<(), CloudLinkCodecError> {
        if integration_id == self.integration_id {
            Ok(())
        } else {
            Err(CloudLinkCodecError::IntegrationStreamBindingConflict)
        }
    }
}

fn prepare_integration_value(
    kind: CloudLinkMessageKind,
    batch_id: String,
    payload: Value,
    created_at: TimestampMs,
    expires_at: Option<TimestampMs>,
) -> Result<CloudLinkEnqueue, CloudLinkCodecError> {
    maximum_complete_envelope_size(kind, &batch_id, &payload, expires_at.is_some())?;
    CloudLinkCodec::prepare_value(kind, batch_id, payload, created_at, expires_at)
}

fn maximum_complete_envelope_size(
    kind: CloudLinkMessageKind,
    batch_id: &str,
    payload: &Value,
    include_expiry: bool,
) -> Result<usize, CloudLinkCodecError> {
    let mut envelope = json!({
        "schema": "aether.cloudlink.envelope.v1",
        "protocol": CLOUDLINK_PROTOCOL,
        "protocol_version": CLOUDLINK_PROTOCOL_VERSION,
        "message_kind": kind.as_str(),
        "gateway_id": "33333333-3333-4333-8333-333333333333",
        "session_id": "44444444-4444-4444-8444-444444444444",
        "session_epoch": MAX_U64,
        "credential_generation": MAX_U64,
        "sent_at_ms": MAX_U64,
        "delivery": {
            "stream_id": "s".repeat(MAX_STREAM_ID_BYTES),
            "stream_epoch": MAX_U64,
            "position": MAX_U64,
            "batch_id": batch_id,
            "digest": format!("sha256:{}", "0".repeat(64))
        },
        "message_authentication": {
            "key_id": "k".repeat(128),
            "algorithm": "Ed25519",
            "signature": "A".repeat(86)
        },
        "traceparent": MAX_TRACEPARENT,
        "payload": payload
    });
    if include_expiry {
        envelope["expires_at_ms"] = Value::String(MAX_U64.to_string());
    }
    let found = serde_json_canonicalizer::to_vec(&envelope)
        .map_err(|source| CloudLinkCodecError::CanonicalJson { source })?
        .len();
    if found > MAX_CLOUDLINK_MESSAGE_BYTES {
        Err(CloudLinkCodecError::MessageTooLarge {
            found,
            maximum: MAX_CLOUDLINK_MESSAGE_BYTES,
        })
    } else {
        Ok(found)
    }
}

fn observation_payload(
    batch: &IntegrationObservationBatchV1Alpha1,
    batch_id: &str,
    observations: Vec<Value>,
) -> Value {
    json!({
        "schema": OBSERVATION_BATCH_SCHEMA,
        "integration_id": batch.integration_id(),
        "snapshot_generation": batch.snapshot_generation(),
        "batch_id": batch_id,
        "observed_at_ms": batch.observed_at_ms(),
        "observations": observations
    })
}

fn partition_identity_hash(batch: &IntegrationObservationBatchV1Alpha1) -> String {
    let digest = Sha256::digest(
        format!(
            "{}\0{}\0{}",
            batch.integration_id(),
            batch.snapshot_generation(),
            batch.batch_id()
        )
        .as_bytes(),
    );
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn partition_batch_id(original: &str, index: usize, identity_hash: &str) -> String {
    let suffix = format!("-part-{index}-{identity_hash}");
    let prefix_bytes = MAX_BATCH_ID_BYTES.saturating_sub(suffix.len());
    format!(
        "{}{}",
        &original[..original.len().min(prefix_bytes)],
        suffix
    )
}

fn validate_integration_id(value: &str) -> Result<(), CloudLinkCodecError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(CloudLinkCodecError::IntegrationStreamBindingConflict)
    }
}
