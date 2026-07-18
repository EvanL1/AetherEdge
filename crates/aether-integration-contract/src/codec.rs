//! Strict decoding, canonical encoding, and domain-to-contract projection.

use aether_domain::{
    IntegrationObservation, IntegrationStateQuality, IntegrationTopologySnapshot, ObservedValue,
    ObservedValueType,
};
use serde::Serialize;

use crate::error::{
    ContractResult, IntegrationContractError, IntegrationContractErrorCode as Code,
};
use crate::profile::IntegrationV1Alpha1Profile;
use crate::validation::{encode_base64url, foundation_float64, identifier};
use crate::wire::{
    EntityPointDescriptorV1Alpha1, IntegrationAreaV1Alpha1, IntegrationDeviceV1Alpha1,
    IntegrationEntityV1Alpha1, IntegrationObservationBatchV1Alpha1,
    IntegrationObservationQualityV1Alpha1, IntegrationObservationV1Alpha1,
    IntegrationTopologySnapshotV1Alpha1, ObservedValueTypeV1Alpha1, ObservedValueV1Alpha1,
};
use crate::{MAX_INTEGRATION_MESSAGE_BYTES, OBSERVATION_BATCH_SCHEMA, TOPOLOGY_SNAPSHOT_SCHEMA};

/// Strict Integration v1alpha1 codec and projection entry point.
pub struct IntegrationContractCodec;

impl IntegrationContractCodec {
    /// Strictly decodes and context-validates one complete topology snapshot.
    pub fn decode_topology(bytes: &[u8]) -> ContractResult<IntegrationTopologySnapshotV1Alpha1> {
        bound_message(bytes)?;
        let topology: IntegrationTopologySnapshotV1Alpha1 = serde_json::from_slice(bytes)
            .map_err(|source| IntegrationContractError::from_json(&source))?;
        topology.validate()?;
        Ok(topology)
    }

    /// Strictly decodes one observation batch against the exact supplied topology.
    pub fn decode_observation_batch(
        bytes: &[u8],
        topology: &IntegrationTopologySnapshotV1Alpha1,
    ) -> ContractResult<IntegrationObservationBatchV1Alpha1> {
        bound_message(bytes)?;
        let batch: IntegrationObservationBatchV1Alpha1 = serde_json::from_slice(bytes)
            .map_err(|source| IntegrationContractError::from_json(&source))?;
        batch.validate_against(topology)?;
        Ok(batch)
    }

    /// Strictly decodes the closed observation wire shape without topology context.
    ///
    /// Transport envelopes use this before the Cloud projection resolves the
    /// exact accepted topology generation. Product code that has the topology
    /// should prefer [`Self::decode_observation_batch`].
    pub fn decode_observation_batch_wire(
        bytes: &[u8],
    ) -> ContractResult<IntegrationObservationBatchV1Alpha1> {
        bound_message(bytes)?;
        let batch: IntegrationObservationBatchV1Alpha1 = serde_json::from_slice(bytes)
            .map_err(|source| IntegrationContractError::from_json(&source))?;
        batch.validate_wire()?;
        Ok(batch)
    }

    /// Strictly decodes one standalone observed value.
    pub fn decode_observed_value(bytes: &[u8]) -> ContractResult<ObservedValueV1Alpha1> {
        bound_message(bytes)?;
        let value: ObservedValueV1Alpha1 = serde_json::from_slice(bytes)
            .map_err(|source| IntegrationContractError::from_json(&source))?;
        value.validate()?;
        Ok(value)
    }

    /// Canonically encodes a previously validated topology.
    pub fn encode_topology(
        topology: &IntegrationTopologySnapshotV1Alpha1,
    ) -> ContractResult<Vec<u8>> {
        topology.validate()?;
        canonical_json(topology)
    }

    /// Canonically encodes a previously validated observation batch.
    pub fn encode_observation_batch(
        batch: &IntegrationObservationBatchV1Alpha1,
        topology: &IntegrationTopologySnapshotV1Alpha1,
    ) -> ContractResult<Vec<u8>> {
        batch.validate_against(topology)?;
        canonical_json(batch)
    }

    /// Projects an internal topology through an explicit public mapping profile.
    pub fn topology_from_domain(
        topology: &IntegrationTopologySnapshot,
        profile: &impl IntegrationV1Alpha1Profile,
    ) -> ContractResult<IntegrationTopologySnapshotV1Alpha1> {
        let areas = topology
            .areas()
            .iter()
            .map(|area| IntegrationAreaV1Alpha1 {
                area_id: area.id().as_str().to_string(),
                name: area.display_name().to_string(),
            })
            .collect();
        let devices = topology
            .devices()
            .iter()
            .map(|device| IntegrationDeviceV1Alpha1 {
                device_id: device.id().as_str().to_string(),
                name: device.display_name().to_string(),
                area_id: device.area_id().map(|area| area.as_str().to_string()),
                manufacturer: None,
                model: None,
                software_version: None,
                hardware_version: None,
            })
            .collect();
        let entities = topology
            .entities()
            .iter()
            .map(|entity| {
                let source_address = profile.source_address(entity)?.to_string();
                let points = entity
                    .points()
                    .iter()
                    .map(|point| EntityPointDescriptorV1Alpha1 {
                        point_key: point.key().as_str().to_string(),
                        title: point.display_name().to_string(),
                        kind: profile.point_kind(entity, point),
                        value_type: public_value_type(point.value_type()),
                        unit: point.unit().map(str::to_string),
                    })
                    .collect();
                Ok(IntegrationEntityV1Alpha1 {
                    entity_id: entity.id().as_str().to_string(),
                    source_address,
                    name: entity.display_name().to_string(),
                    entity_kind: entity.kind().to_string(),
                    device_id: entity.device_id().map(|device| device.as_str().to_string()),
                    area_id: entity.area_id().map(|area| area.as_str().to_string()),
                    points,
                })
            })
            .collect::<ContractResult<Vec<_>>>()?;

        let wire = IntegrationTopologySnapshotV1Alpha1 {
            schema: TOPOLOGY_SNAPSHOT_SCHEMA.to_string(),
            integration_id: topology.integration_id().as_str().to_string(),
            integration_kind: profile.integration_kind().to_string(),
            snapshot_generation: topology.generation().get().to_string(),
            observed_at_ms: topology.observed_at().get().to_string(),
            areas,
            devices,
            entities,
        };
        wire.validate()?;
        Ok(wire)
    }

    /// Projects typed internal observations into one closed public batch.
    pub fn observation_batch_from_domain(
        topology: &IntegrationTopologySnapshot,
        batch_id: &str,
        observations: &[IntegrationObservation],
    ) -> ContractResult<IntegrationObservationBatchV1Alpha1> {
        identifier(batch_id)?;
        let mut observed_at_ms = 0_u64;
        let observations = observations
            .iter()
            .map(|observation| {
                if observation.gateway_id() != topology.gateway_id()
                    || observation.integration_id() != topology.integration_id()
                {
                    return Err(IntegrationContractError::new(
                        Code::ReferenceNotFound,
                        "observation scope differs from the topology",
                    ));
                }
                let descriptor = topology
                    .entities()
                    .iter()
                    .find(|entity| entity.id() == observation.entity_id())
                    .and_then(|entity| {
                        entity
                            .points()
                            .iter()
                            .find(|point| point.key() == observation.point_key())
                    })
                    .ok_or_else(|| {
                        IntegrationContractError::new(
                            Code::ReferenceNotFound,
                            "observation refers to an undeclared point",
                        )
                    })?;
                let value = observation
                    .value()
                    .map(observed_value_from_domain)
                    .transpose()?;
                if value.as_ref().is_some_and(|value| {
                    value.value_type() != public_value_type(descriptor.value_type())
                }) {
                    return Err(IntegrationContractError::new(
                        Code::ValueTypeMismatch,
                        "observation value differs from the declared point type",
                    ));
                }
                observed_at_ms = observed_at_ms.max(observation.observed_at().get());
                Ok(IntegrationObservationV1Alpha1 {
                    entity_id: observation.entity_id().as_str().to_string(),
                    point_key: observation.point_key().as_str().to_string(),
                    observed_at_ms: observation.observed_at().get().to_string(),
                    quality: match observation.quality() {
                        IntegrationStateQuality::Good => {
                            IntegrationObservationQualityV1Alpha1::Good
                        },
                        IntegrationStateQuality::Unknown | IntegrationStateQuality::Unavailable => {
                            IntegrationObservationQualityV1Alpha1::Unavailable
                        },
                    },
                    value,
                    diagnostic: None,
                })
            })
            .collect::<ContractResult<Vec<_>>>()?;

        let batch = IntegrationObservationBatchV1Alpha1 {
            schema: OBSERVATION_BATCH_SCHEMA.to_string(),
            integration_id: topology.integration_id().as_str().to_string(),
            snapshot_generation: topology.generation().get().to_string(),
            batch_id: batch_id.to_string(),
            observed_at_ms: observed_at_ms.to_string(),
            observations,
        };
        let topology = topology_for_observation_validation(topology)?;
        batch.validate_against(&topology)?;
        Ok(batch)
    }
}

fn topology_for_observation_validation(
    topology: &IntegrationTopologySnapshot,
) -> ContractResult<IntegrationTopologySnapshotV1Alpha1> {
    struct ValidationProfile;
    impl IntegrationV1Alpha1Profile for ValidationProfile {
        fn integration_kind(&self) -> &str {
            "internal-validation"
        }

        fn source_address<'a>(
            &self,
            entity: &'a aether_domain::EntityRecord,
        ) -> ContractResult<&'a str> {
            Ok(entity.id().as_str())
        }

        fn point_kind(
            &self,
            _entity: &aether_domain::EntityRecord,
            point: &aether_domain::EntityPointDescriptor,
        ) -> crate::IntegrationPointKindV1Alpha1 {
            match point.kind() {
                aether_domain::IntegrationPointKind::Event => {
                    crate::IntegrationPointKindV1Alpha1::Event
                },
                aether_domain::IntegrationPointKind::State
                | aether_domain::IntegrationPointKind::Attribute => {
                    crate::IntegrationPointKindV1Alpha1::Status
                },
            }
        }
    }
    IntegrationContractCodec::topology_from_domain(topology, &ValidationProfile)
}

const fn public_value_type(value_type: ObservedValueType) -> ObservedValueTypeV1Alpha1 {
    match value_type {
        ObservedValueType::Boolean => ObservedValueTypeV1Alpha1::Boolean,
        ObservedValueType::Int64 => ObservedValueTypeV1Alpha1::Int64,
        ObservedValueType::UInt64 => ObservedValueTypeV1Alpha1::UInt64,
        ObservedValueType::Float64 => ObservedValueTypeV1Alpha1::Float64,
        ObservedValueType::Decimal => ObservedValueTypeV1Alpha1::Decimal,
        ObservedValueType::String | ObservedValueType::Enum => ObservedValueTypeV1Alpha1::String,
        ObservedValueType::Bytes => ObservedValueTypeV1Alpha1::Bytes,
    }
}

fn observed_value_from_domain(value: &ObservedValue) -> ContractResult<ObservedValueV1Alpha1> {
    let value = match value {
        ObservedValue::Boolean(value) => ObservedValueV1Alpha1::Boolean { value: *value },
        ObservedValue::Int64(value) => ObservedValueV1Alpha1::Int64 {
            value: value.to_string(),
        },
        ObservedValue::UInt64(value) => ObservedValueV1Alpha1::UInt64 {
            value: value.to_string(),
        },
        ObservedValue::Float64(value) => {
            foundation_float64(*value)?;
            ObservedValueV1Alpha1::Float64 { value: *value }
        },
        ObservedValue::Decimal(value) => ObservedValueV1Alpha1::Decimal {
            value: value.clone(),
        },
        ObservedValue::String(value) | ObservedValue::Enum(value) => {
            ObservedValueV1Alpha1::String {
                value: value.clone(),
            }
        },
        ObservedValue::Bytes(value) => ObservedValueV1Alpha1::Bytes {
            encoding: "base64url".to_string(),
            value: encode_base64url(value),
        },
    };
    value.validate()?;
    Ok(value)
}

fn canonical_json(value: &impl Serialize) -> ContractResult<Vec<u8>> {
    let bytes = serde_json_canonicalizer::to_vec(value).map_err(|_source| {
        IntegrationContractError::new(Code::JsonSyntaxError, "canonical JSON encoding failed")
    })?;
    bound_message(&bytes)?;
    Ok(bytes)
}

fn bound_message(bytes: &[u8]) -> ContractResult<()> {
    if bytes.len() > MAX_INTEGRATION_MESSAGE_BYTES {
        return Err(IntegrationContractError::new(
            Code::FieldBound,
            "integration JSON exceeds the Edge binding limit",
        ));
    }
    Ok(())
}
