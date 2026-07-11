//! SHM-backed data store for protocol acquisition.
//!
//! SHM is the only live-state authority. Construction fails when the coherent
//! writer layout is unavailable; there is deliberately no database fallback.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{trace, warn};

use crate::protocols::core::data::DataBatch;
use crate::protocols::core::error::{GatewayError, Result as ProtocolResult};
use aether_routing::{ChannelPointUpdate, RoutingCache};
use aether_rtdb_shm::ShmHandle;
use aether_shm_bridge::ShmChannelHealthWriter;

/// Acquisition-side live-state writer.
pub struct ShmDataStore {
    routing_cache: Arc<RoutingCache>,
    shm_handle: Arc<ShmHandle>,
    channel_health_writer: Option<Arc<ShmChannelHealthWriter>>,
    slot_miss_count: AtomicU64,
}

impl ShmDataStore {
    /// Creates a store over an already-published coherent SHM layout.
    pub fn new(
        shm_handle: Arc<ShmHandle>,
        routing_cache: Arc<RoutingCache>,
    ) -> ProtocolResult<Self> {
        if !shm_handle.is_available() {
            return Err(GatewayError::config(
                "authoritative SHM layout is unavailable",
            ));
        }

        Ok(Self {
            routing_cache,
            shm_handle,
            channel_health_writer: None,
            slot_miss_count: AtomicU64::new(0),
        })
    }

    /// Attaches the dedicated SHM channel-health writer.
    #[must_use]
    pub fn with_channel_health_writer(mut self, writer: Arc<ShmChannelHealthWriter>) -> Self {
        self.channel_health_writer = Some(writer);
        self
    }

    /// Returns the cumulative count of writes whose point had no allocated slot.
    pub fn slot_miss_count(&self) -> u64 {
        self.slot_miss_count.load(Ordering::Relaxed)
    }

    /// Refreshes health-plane writer liveness without changing channel state.
    pub fn refresh_channel_health_heartbeat(&self, timestamp_ms: u64) {
        if let Some(writer) = &self.channel_health_writer {
            writer.update_heartbeat(timestamp_ms);
        }
    }

    fn batch_to_updates(&self, channel_id: u32, batch: &DataBatch) -> Vec<ChannelPointUpdate> {
        let mut updates = Vec::with_capacity(batch.len());

        for point in batch.iter() {
            let value = match point.value.as_f64() {
                Some(value) if value.is_finite() => value,
                Some(value) => {
                    warn!(
                        "Ch{} [{:?}] Point {}: non-finite value {}, skipping",
                        channel_id, point.point_type, point.id, value
                    );
                    continue;
                },
                None => {
                    warn!(
                        "Ch{} [{:?}] Point {}: non-numeric value {:?}, skipping",
                        channel_id, point.point_type, point.id, point.value
                    );
                    continue;
                },
            };

            trace!(
                "[{:?}] Point {}: value={:.2}",
                point.point_type, point.id, value
            );
            updates.push(ChannelPointUpdate {
                channel_id,
                point_type: point.point_type,
                point_id: point.id,
                value,
                raw_value: None,
                cascade_depth: 0,
            });
        }

        updates
    }

    /// Writes a transformed protocol batch directly to authoritative SHM.
    pub async fn write_batch(&self, channel_id: u32, batch: DataBatch) -> ProtocolResult<()> {
        if batch.is_empty() {
            return Ok(());
        }

        let updates = self.batch_to_updates(channel_id, &batch);
        let layout = self.shm_handle.layout_arc().ok_or_else(|| {
            GatewayError::config("authoritative SHM layout disappeared during acquisition")
        })?;
        let stats = aether_rtdb_shm::write_channel_batch_direct(
            &layout.writer,
            &layout.index,
            &self.routing_cache,
            updates,
        );

        if stats.slot_misses > 0 {
            self.slot_miss_count
                .fetch_add(stats.slot_misses as u64, Ordering::Relaxed);
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
