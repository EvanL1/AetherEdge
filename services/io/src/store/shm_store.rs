//! SHM-backed data store for protocol acquisition.
//!
//! SHM is the only live-state authority. Construction fails when the coherent
//! writer layout is unavailable; there is deliberately no database fallback.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::warn;

use crate::protocols::core::data::DataBatch;
use crate::protocols::core::error::{GatewayError, Result as ProtocolResult};
use crate::protocols::core::quality::Quality;
use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointKind, PointQuality,
    TimestampMs,
};
use aether_ports::{PortError, PortErrorKind};
use aether_routing::{MAX_C2C_CASCADE_DEPTH, RoutingCache};
use aether_shm_bridge::{ShmAcquisitionStateWriter, ShmChannelHealthWriterHandle, ShmWriterHandle};

/// Acquisition-side live-state writer.
pub struct ShmDataStore {
    routing_cache: Arc<RoutingCache>,
    write_path: ShmWritePath,
    channel_health_writer: Option<Arc<ShmChannelHealthWriterHandle>>,
    slot_miss_count: AtomicU64,
}

enum ShmWritePath {
    DynamicHandle(Arc<ShmWriterHandle>),
    TypedFixedGeneration(Arc<ShmAcquisitionStateWriter>),
}

impl ShmDataStore {
    /// Creates a store over an already-published coherent SHM layout.
    pub fn new(
        shm_handle: Arc<ShmWriterHandle>,
        routing_cache: Arc<RoutingCache>,
    ) -> ProtocolResult<Self> {
        if !shm_handle.is_available() {
            return Err(GatewayError::config(
                "authoritative SHM layout is unavailable",
            ));
        }

        Ok(Self {
            routing_cache,
            write_path: ShmWritePath::DynamicHandle(shm_handle),
            channel_health_writer: None,
            slot_miss_count: AtomicU64::new(0),
        })
    }

    /// Composes the protocol store directly over the typed acquisition port.
    ///
    /// This supports tests and in-process fixed-generation compositions.
    /// Production uses [`Self::new`] so each batch loads the typed writer and
    /// manifest atomically published by the runtime `ShmWriterHandle`.
    pub fn from_acquisition_writer(
        acquisition_writer: Arc<ShmAcquisitionStateWriter>,
        routing_cache: Arc<RoutingCache>,
    ) -> Self {
        Self {
            routing_cache,
            write_path: ShmWritePath::TypedFixedGeneration(acquisition_writer),
            channel_health_writer: None,
            slot_miss_count: AtomicU64::new(0),
        }
    }

    /// Attaches the dedicated SHM channel-health writer.
    #[must_use]
    pub fn with_channel_health_writer(mut self, writer: Arc<ShmChannelHealthWriterHandle>) -> Self {
        self.channel_health_writer = Some(writer);
        self
    }

    /// Returns the cumulative count of writes whose point had no allocated slot.
    pub fn slot_miss_count(&self) -> u64 {
        self.slot_miss_count.load(Ordering::Relaxed)
    }

    /// Refreshes health-plane writer liveness without changing channel state.
    pub fn refresh_channel_health_heartbeat(&self, timestamp_ms: u64) {
        if let Some(writer) = &self.channel_health_writer
            && let Err(error) = writer.update_heartbeat(timestamp_ms)
        {
            warn!("failed to refresh authoritative SHM health heartbeat: {error}");
        }
    }

    fn batch_to_acquired_samples(
        &self,
        channel_id: u32,
        batch: &DataBatch,
    ) -> ProtocolResult<Vec<AcquiredPointSample>> {
        batch
            .iter()
            .map(|point| {
                let kind = match point.point_type {
                    aether_model::PointType::Telemetry => PointKind::Telemetry,
                    aether_model::PointType::Signal => PointKind::Status,
                    aether_model::PointType::Control
                    | aether_model::PointType::Adjustment => {
                        return Err(GatewayError::invalid_data(format!(
                            "acquisition batch contains command-owned {:?} point {}",
                            point.point_type, point.id
                        )));
                    },
                };
                let value = point.value.as_f64().filter(|value| value.is_finite()).ok_or_else(
                    || {
                        GatewayError::invalid_data(format!(
                            "channel {channel_id} {kind:?} point {} has a non-finite or non-numeric value",
                            point.id
                        ))
                    },
                )?;
                let source_timestamp = point.source_timestamp.as_ref().unwrap_or(&point.timestamp);
                let timestamp_ms = u64::try_from(source_timestamp.timestamp_millis()).map_err(
                    |_| {
                        GatewayError::invalid_data(format!(
                            "channel {channel_id} {kind:?} point {} has a pre-epoch timestamp",
                            point.id
                        ))
                    },
                )?;
                let quality = match point.quality {
                    Quality::Good => PointQuality::Good,
                    Quality::Uncertain
                    | Quality::Substituted
                    | Quality::Overflow
                    | Quality::Underflow
                    | Quality::LastKnown => PointQuality::Uncertain,
                    Quality::NotConnected | Quality::CommFailure | Quality::OutOfService => {
                        PointQuality::Unavailable
                    },
                    Quality::Bad
                    | Quality::Invalid
                    | Quality::DeviceFailure
                    | Quality::SensorFailure
                    | Quality::ConfigError => PointQuality::Bad,
                };
                let address = ChannelPointAddress::new(
                    ChannelId::new(channel_id),
                    kind,
                    PointId::new(point.id),
                )
                .map_err(|error| GatewayError::invalid_data(error.to_string()))?;
                AcquiredPointSample::new(
                    address,
                    value,
                    value,
                    TimestampMs::new(timestamp_ms),
                    quality,
                )
                .map_err(|error| GatewayError::invalid_data(error.to_string()))
            })
            .collect()
    }

    fn expand_c2c(
        &self,
        source_samples: Vec<AcquiredPointSample>,
    ) -> ProtocolResult<Vec<AcquiredPointSample>> {
        let mut positions = HashMap::with_capacity(source_samples.len());
        for (index, sample) in source_samples.iter().enumerate() {
            if positions.insert(sample.address(), index).is_some() {
                return Err(GatewayError::invalid_data(format!(
                    "duplicate acquired point address {:?}",
                    sample.address()
                )));
            }
        }

        let mut expanded = source_samples.clone();
        for sample in source_samples {
            let mut path = HashSet::from([sample.address()]);
            self.expand_c2c_from(sample, 0, &mut path, &mut positions, &mut expanded)?;
        }
        Ok(expanded)
    }

    fn expand_c2c_from(
        &self,
        source: AcquiredPointSample,
        depth: u8,
        path: &mut HashSet<ChannelPointAddress>,
        positions: &mut HashMap<ChannelPointAddress, usize>,
        expanded: &mut Vec<AcquiredPointSample>,
    ) -> ProtocolResult<()> {
        if depth >= MAX_C2C_CASCADE_DEPTH {
            return Ok(());
        }
        let point_type = match source.address().kind() {
            PointKind::Telemetry => aether_model::PointType::Telemetry,
            PointKind::Status => aether_model::PointType::Signal,
            PointKind::Command | PointKind::Action => {
                return Err(GatewayError::invalid_data(
                    "command-owned point reached acquisition C2C expansion",
                ));
            },
        };
        let Some(target) = self.routing_cache.lookup_c2c_by_parts(
            source.address().channel_id().get(),
            point_type,
            source.address().point_id().get(),
        ) else {
            return Ok(());
        };
        let target_kind = match target.point_type {
            aether_model::PointType::Telemetry => PointKind::Telemetry,
            aether_model::PointType::Signal => PointKind::Status,
            aether_model::PointType::Control | aether_model::PointType::Adjustment => {
                return Err(GatewayError::invalid_data(format!(
                    "C2C target {}:{:?}:{} is not acquisition-owned",
                    target.channel_id, target.point_type, target.point_id
                )));
            },
        };
        let target_address = ChannelPointAddress::new(
            ChannelId::new(target.channel_id),
            target_kind,
            PointId::new(target.point_id),
        )
        .map_err(|error| GatewayError::invalid_data(error.to_string()))?;
        if path.contains(&target_address) || positions.contains_key(&target_address) {
            return Ok(());
        }

        let value = target.transform(source.value());
        let target_sample = AcquiredPointSample::new(
            target_address,
            value,
            value,
            source.timestamp(),
            source.quality(),
        )
        .map_err(|error| GatewayError::invalid_data(error.to_string()))?;
        positions.insert(target_address, expanded.len());
        expanded.push(target_sample);
        path.insert(target_address);
        let result = self.expand_c2c_from(target_sample, depth + 1, path, positions, expanded);
        path.remove(&target_address);
        result
    }

    /// Expands routing and commits one typed batch to authoritative SHM.
    pub async fn write_batch(&self, channel_id: u32, batch: DataBatch) -> ProtocolResult<()> {
        if batch.is_empty() {
            return Ok(());
        }

        let source_samples = self.batch_to_acquired_samples(channel_id, &batch)?;
        let samples = self.expand_c2c(source_samples)?;
        let result = match &self.write_path {
            ShmWritePath::TypedFixedGeneration(writer) => writer.commit_batch(&samples),
            ShmWritePath::DynamicHandle(shm_handle) => {
                let layout = shm_handle.generation().ok_or_else(|| {
                    GatewayError::config("authoritative SHM layout disappeared during acquisition")
                })?;
                layout.acquisition_writer().commit_batch(&samples)
            },
        };
        if let Err(error) = result {
            if error.kind() == PortErrorKind::NotFound {
                self.slot_miss_count.fetch_add(1, Ordering::Relaxed);
            }
            return Err(map_acquisition_write_error(error));
        }
        Ok(())
    }

    /// Publishes channel connectivity on the dedicated SHM health plane.
    pub async fn publish_channel_online(&self, channel_id: u32, online: bool) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if let Some(writer) = &self.channel_health_writer
            && let Err(error) = writer.set_online(channel_id, online, timestamp_ms)
        {
            warn!(
                "Ch{} failed to publish authoritative SHM health: {}",
                channel_id, error
            );
        }
    }
}

fn map_acquisition_write_error(error: PortError) -> GatewayError {
    let kind = error.kind();
    let message = error.to_string();
    match kind {
        PortErrorKind::NotFound => GatewayError::PointNotFound(message),
        PortErrorKind::Unavailable | PortErrorKind::Conflict => GatewayError::connection(message),
        PortErrorKind::Timeout => GatewayError::WriteTimeout,
        PortErrorKind::Rejected | PortErrorKind::InvalidData => GatewayError::invalid_data(message),
        PortErrorKind::Permanent => GatewayError::config(message),
    }
}
