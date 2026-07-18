//! Delegated device-provider implementation.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aether_domain::{
    AreaId, AreaRecord, DeviceId, DeviceRecord, EntityId, EntityPointDescriptor, EntityRecord,
    ExternalAlias, GatewayIdentity, IntegrationId, IntegrationObservation, IntegrationSnapshot,
    IntegrationStateQuality, IntegrationTopologySnapshot, SnapshotDigest, TimestampMs,
    TopologyGeneration,
};
use aether_ports::{
    DelegatedDeviceProvider, IntegrationTopologyGenerationStore, PortError, PortErrorKind,
    PortResult,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::mapping::{invalid_data, invalid_mapping, observed_value, point_descriptors};
use crate::{
    HomeAssistantEntity, HomeAssistantSnapshot, HomeAssistantState, HomeAssistantStateChanged,
    HomeAssistantTransport,
};

const MAX_PENDING_OBSERVATIONS: usize = 16_384;

#[derive(Clone)]
struct MappedEntity {
    id: EntityId,
    domain: String,
    points: Vec<EntityPointDescriptor>,
}

/// Home Assistant adapter implementing the vendor-neutral provider port.
pub struct HomeAssistantBridge<T> {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    transport: T,
    generation: AtomicU64,
    generation_store: Option<Arc<dyn IntegrationTopologyGenerationStore>>,
    sequence: AtomicU64,
    entities_by_alias: RwLock<BTreeMap<String, MappedEntity>>,
    pending: Mutex<VecDeque<IntegrationObservation>>,
}

impl<T> HomeAssistantBridge<T> {
    /// Creates a bridge around an authenticated Home Assistant transport.
    #[must_use]
    pub fn new(gateway_id: GatewayIdentity, integration_id: IntegrationId, transport: T) -> Self {
        Self {
            gateway_id,
            integration_id,
            transport,
            generation: AtomicU64::new(1),
            generation_store: None,
            sequence: AtomicU64::new(1),
            entities_by_alias: RwLock::new(BTreeMap::new()),
            pending: Mutex::new(VecDeque::new()),
        }
    }

    /// Uses durable digest-to-generation reservations instead of a process-local counter.
    ///
    /// Production compositions that publish the public Integration contract
    /// should inject a restart-stable implementation before the first snapshot.
    #[must_use]
    pub fn with_generation_store(
        mut self,
        generation_store: Arc<dyn IntegrationTopologyGenerationStore>,
    ) -> Self {
        self.generation_store = Some(generation_store);
        self
    }

    /// Returns the explicit point mapping for one Home Assistant domain.
    pub fn mapped_point_descriptors(
        &self,
        domain: &str,
        attributes: &BTreeMap<String, Value>,
    ) -> PortResult<Vec<EntityPointDescriptor>> {
        point_descriptors(domain, None, attributes)
    }

    fn next_counter(counter: &AtomicU64, name: &str) -> PortResult<u64> {
        counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Permanent,
                    format!("Home Assistant {name} exhausted"),
                )
            })
    }

    async fn map_snapshot(
        &self,
        mut snapshot: HomeAssistantSnapshot,
    ) -> PortResult<(IntegrationSnapshot, BTreeMap<String, MappedEntity>)> {
        snapshot.areas.sort_by(|left, right| left.id.cmp(&right.id));
        snapshot
            .devices
            .sort_by(|left, right| left.id.cmp(&right.id));
        snapshot
            .entities
            .sort_by(|left, right| left.id.cmp(&right.id));
        snapshot
            .states
            .sort_by(|left, right| left.entity_id.cmp(&right.entity_id));

        let states: BTreeMap<_, _> = snapshot
            .states
            .iter()
            .map(|state| (state.entity_id.as_str(), state))
            .collect();

        let areas = snapshot
            .areas
            .iter()
            .map(|area| {
                AreaRecord::new(
                    AreaId::new(&area.id).map_err(invalid_mapping)?,
                    &area.name,
                    vec![
                        ExternalAlias::new("home-assistant", "area-id", &area.id)
                            .map_err(invalid_mapping)?,
                    ],
                )
                .map_err(invalid_mapping)
            })
            .collect::<PortResult<Vec<_>>>()?;

        let devices = snapshot
            .devices
            .iter()
            .map(|device| {
                DeviceRecord::new(
                    DeviceId::new(&device.id).map_err(invalid_mapping)?,
                    &device.name,
                    device
                        .area_id
                        .as_deref()
                        .map(AreaId::new)
                        .transpose()
                        .map_err(invalid_mapping)?,
                    vec![
                        ExternalAlias::new("home-assistant", "device-id", &device.id)
                            .map_err(invalid_mapping)?,
                    ],
                )
                .map_err(invalid_mapping)
            })
            .collect::<PortResult<Vec<_>>>()?;

        let mut entities_by_alias = BTreeMap::new();
        let entities = snapshot
            .entities
            .iter()
            .map(|entity| {
                validate_entity_domain(entity)?;
                let state = states.get(entity.entity_id.as_str()).copied();
                let points = point_descriptors(
                    &entity.domain,
                    state.map(|state| state.state.as_str()),
                    state.map_or(&BTreeMap::new(), |state| &state.attributes),
                )?;
                let id = EntityId::new(&entity.id).map_err(invalid_mapping)?;
                let record = EntityRecord::new(
                    id.clone(),
                    &entity.name,
                    &entity.domain,
                    points.clone(),
                    entity
                        .device_id
                        .as_deref()
                        .map(DeviceId::new)
                        .transpose()
                        .map_err(invalid_mapping)?,
                    entity
                        .area_id
                        .as_deref()
                        .map(AreaId::new)
                        .transpose()
                        .map_err(invalid_mapping)?,
                    vec![
                        ExternalAlias::new("home-assistant", "entity-id", &entity.entity_id)
                            .map_err(invalid_mapping)?,
                    ],
                )
                .map_err(invalid_mapping)?;
                if entities_by_alias
                    .insert(
                        entity.entity_id.clone(),
                        MappedEntity {
                            id,
                            domain: entity.domain.clone(),
                            points,
                        },
                    )
                    .is_some()
                {
                    return Err(invalid_data(
                        "Home Assistant snapshot contains a duplicate entity alias",
                    ));
                }
                Ok(record)
            })
            .collect::<PortResult<Vec<_>>>()?;

        let digest = topology_digest(&snapshot, &entities)?;
        let generation = if let Some(store) = &self.generation_store {
            store
                .reserve_generation(&self.gateway_id, &self.integration_id, &digest)
                .await?
        } else {
            TopologyGeneration::new(Self::next_counter(&self.generation, "generation")?)
                .map_err(invalid_mapping)?
        };
        let topology = IntegrationTopologySnapshot::new(
            self.gateway_id.clone(),
            self.integration_id.clone(),
            generation,
            TimestampMs::new(
                snapshot
                    .states
                    .iter()
                    .map(|state| state.observed_at_ms)
                    .max()
                    .unwrap_or(1),
            ),
            digest,
            areas,
            devices,
            entities,
        )
        .map_err(invalid_mapping)?;

        let mut observations = Vec::new();
        for state in &snapshot.states {
            let Some(entity) = entities_by_alias.get(&state.entity_id) else {
                continue;
            };
            observations.extend(self.map_state(entity, state)?);
            if observations.len() > MAX_PENDING_OBSERVATIONS {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    "Home Assistant initial state exceeds the bounded observation queue",
                ));
            }
        }

        let snapshot = IntegrationSnapshot::new(topology, observations).map_err(invalid_mapping)?;
        Ok((snapshot, entities_by_alias))
    }

    fn map_state(
        &self,
        entity: &MappedEntity,
        state: &HomeAssistantState,
    ) -> PortResult<Vec<IntegrationObservation>> {
        let mut observations = Vec::with_capacity(entity.points.len());
        for point in &entity.points {
            let sequence = Self::next_counter(&self.sequence, "observation sequence")?;
            let observation = match state.state.as_str() {
                "unknown" => IntegrationObservation::unknown(
                    self.gateway_id.clone(),
                    self.integration_id.clone(),
                    entity.id.clone(),
                    point.key().clone(),
                    TimestampMs::new(state.observed_at_ms),
                    sequence,
                    state.context_id.as_deref(),
                ),
                "unavailable" => IntegrationObservation::unavailable(
                    self.gateway_id.clone(),
                    self.integration_id.clone(),
                    entity.id.clone(),
                    point.key().clone(),
                    TimestampMs::new(state.observed_at_ms),
                    sequence,
                    state.context_id.as_deref(),
                ),
                provider_state => {
                    let projected_value = match observed_value(
                        point,
                        &entity.domain,
                        provider_state,
                        &state.attributes,
                    ) {
                        Ok(value) => value,
                        Err(error) if error.kind() == PortErrorKind::InvalidData => None,
                        Err(error) => return Err(error),
                    };
                    match projected_value {
                        Some(value) => IntegrationObservation::available(
                            self.gateway_id.clone(),
                            self.integration_id.clone(),
                            entity.id.clone(),
                            point.key().clone(),
                            value,
                            TimestampMs::new(state.observed_at_ms),
                            sequence,
                            state.context_id.as_deref(),
                        ),
                        None => IntegrationObservation::unknown(
                            self.gateway_id.clone(),
                            self.integration_id.clone(),
                            entity.id.clone(),
                            point.key().clone(),
                            TimestampMs::new(state.observed_at_ms),
                            sequence,
                            state.context_id.as_deref(),
                        ),
                    }
                },
            }
            .map_err(invalid_mapping)?;
            observations.push(observation);
        }
        Ok(observations)
    }
}

fn validate_entity_domain(entity: &HomeAssistantEntity) -> PortResult<()> {
    let prefix = entity
        .entity_id
        .split_once('.')
        .map(|(domain, _)| domain)
        .ok_or_else(|| invalid_data("Home Assistant entity alias has no domain"))?;
    if prefix != entity.domain {
        return Err(invalid_data(
            "Home Assistant entity alias and declared domain disagree",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct PointDigestProjection<'a> {
    entity_id: &'a str,
    points: Vec<(&'a str, &'static str, Option<&'a str>)>,
}

fn topology_digest(
    snapshot: &HomeAssistantSnapshot,
    entities: &[EntityRecord],
) -> PortResult<SnapshotDigest> {
    let point_projection: Vec<_> = entities
        .iter()
        .map(|entity| PointDigestProjection {
            entity_id: entity.id().as_str(),
            points: entity
                .points()
                .iter()
                .map(|point| {
                    (
                        point.key().as_str(),
                        value_type_name(point.value_type()),
                        point.unit(),
                    )
                })
                .collect(),
        })
        .collect();
    let bytes = serde_json::to_vec(&(
        &snapshot.areas,
        &snapshot.devices,
        &snapshot.entities,
        point_projection,
    ))
    .map_err(|_| invalid_data("Home Assistant topology digest serialization failed"))?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| invalid_data("Home Assistant topology digest formatting failed"))?;
    }
    SnapshotDigest::new(encoded).map_err(invalid_mapping)
}

const fn value_type_name(value_type: aether_domain::ObservedValueType) -> &'static str {
    use aether_domain::ObservedValueType;
    match value_type {
        ObservedValueType::Boolean => "boolean",
        ObservedValueType::Int64 => "int64",
        ObservedValueType::UInt64 => "uint64",
        ObservedValueType::Float64 => "float64",
        ObservedValueType::Decimal => "decimal",
        ObservedValueType::String => "string",
        ObservedValueType::Bytes => "bytes",
        ObservedValueType::Enum => "enum",
    }
}

#[async_trait]
impl<T> DelegatedDeviceProvider for HomeAssistantBridge<T>
where
    T: HomeAssistantTransport,
{
    fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    async fn snapshot(&self) -> PortResult<IntegrationSnapshot> {
        let snapshot = self.transport.fetch_snapshot().await?;
        let (snapshot, entities) = self.map_snapshot(snapshot).await?;
        *self.entities_by_alias.write().await = entities;
        self.pending.lock().await.clear();
        Ok(snapshot)
    }

    async fn next_observation(&self) -> PortResult<IntegrationObservation> {
        loop {
            if let Some(observation) = self.pending.lock().await.pop_front() {
                return Ok(observation);
            }

            let HomeAssistantStateChanged { new_state } =
                self.transport.next_state_changed().await?;
            let entity = self
                .entities_by_alias
                .read()
                .await
                .get(&new_state.entity_id)
                .cloned()
                .ok_or_else(|| {
                    PortError::new(
                        PortErrorKind::Conflict,
                        "Home Assistant state refers to an entity outside the last snapshot and requires a complete resynchronization",
                    )
                })?;
            let current_points = point_descriptors(
                &entity.domain,
                Some(new_state.state.as_str()),
                &new_state.attributes,
            )?;
            if current_points != entity.points {
                return Err(PortError::new(
                    PortErrorKind::Conflict,
                    "Home Assistant point mapping changed and requires a complete resynchronization",
                ));
            }
            let observations = self.map_state(&entity, &new_state)?;
            let mut pending = self.pending.lock().await;
            if observations.len() > MAX_PENDING_OBSERVATIONS - pending.len() {
                pending.clear();
                return Err(PortError::new(
                    PortErrorKind::Conflict,
                    "Home Assistant observation queue overflow requires a complete resync",
                ));
            }
            pending.extend(observations);
        }
    }
}

#[allow(dead_code)]
fn _quality_is_closed(quality: IntegrationStateQuality) -> bool {
    matches!(
        quality,
        IntegrationStateQuality::Good
            | IntegrationStateQuality::Unknown
            | IntegrationStateQuality::Unavailable
    )
}
