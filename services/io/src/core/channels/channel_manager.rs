//! Channel lifecycle management module
//!
//! Handles channel creation, removal, and lifecycle operations.
//! Channel entry types are in `channel_entry`, task logic in `channel_task`,
//! and creation/factory methods in `channel_creation`.

use arc_swap::ArcSwapOption;
use dashmap::DashSet;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::core::channels::channel_entry::{ChannelEntry, ChannelStats, MAX_CHANNELS};
use crate::core::channels::shm_listener::ShmCommandListener;
use crate::error::{IoError, Result};
use crate::store::ShmDataStore;
use aether_shm_bridge::{ShmChannelHealthWriterHandle, ShmWriterHandle};

// Re-export types for backwards compatibility
pub use crate::core::channels::channel_entry::{ChannelMetadata, unix_timestamp_ms};

// ============================================================================
// Channel Manager
// ============================================================================

/// Channel manager - responsible for channel lifecycle management
///
/// # arc-swap + Vec Architecture
/// Uses pre-allocated `Vec<ArcSwapOption<ChannelEntry>>` for O(1) lock-free access.
/// - Read latency: ~5ns (was ~50μs with RwLock+DashMap)
/// - Write latency: ~50ns (atomic swap)
/// - Memory: ~160KB for 10000 slots (16 bytes per ArcSwapOption)
pub struct ChannelManager {
    /// Pre-allocated channel slots for O(1) direct index access
    /// Index = channel_id, value = `Option<Arc<ChannelEntry>>`
    pub(super) channels: Vec<ArcSwapOption<ChannelEntry>>,
    /// Active channel ID index for O(1) iteration (avoids O(10000) full scan)
    /// Synchronized with channels: insert on create_channel, remove on remove_channel
    pub(super) active_channel_ids: DashSet<u32>,
    /// Shared authoritative SHM store used by all channels.
    pub(super) store: Arc<ShmDataStore>,
    /// Routing cache for C2M/M2C routing (public for reload operations)
    pub routing_cache: Arc<aether_routing::RoutingCache>,
    /// SQLite connection pool for configuration loading
    pub(super) sqlite_pool: Option<sqlx::SqlitePool>,
    /// Runtime-swappable shared memory handle (writer + index, rebuilt on routing reload)
    pub(super) shm_handle: Arc<ShmWriterHandle>,
    /// Command TX cache for O(1) hot path access
    /// Shared with AppState for direct API access bypassing RwLock
    pub(super) command_tx_cache: Option<Arc<crate::api::command_cache::CommandTxCache>>,

    // ========== SHM Command Listener (Event-driven M2C via UDS) ==========
    /// SHM command listener for event-driven M2C command dispatch (UDS path, self-healing)
    pub(super) shm_listener: Option<Arc<ShmCommandListener>>,
}

impl std::fmt::Debug for ChannelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelManager")
            .field("channels", &self.channel_count())
            .finish()
    }
}

impl ChannelManager {
    /// Pre-allocate channel slots for O(1) access
    #[inline]
    fn create_channel_slots() -> Vec<ArcSwapOption<ChannelEntry>> {
        (0..MAX_CHANNELS).map(|_| ArcSwapOption::empty()).collect()
    }

    /// Create new channel manager
    pub fn new(
        shm_handle: Arc<ShmWriterHandle>,
        routing_cache: Arc<aether_routing::RoutingCache>,
    ) -> Result<Self> {
        let store = Arc::new(ShmDataStore::new(
            Arc::clone(&shm_handle),
            Arc::clone(&routing_cache),
        )?);
        Ok(Self {
            channels: Self::create_channel_slots(),
            active_channel_ids: DashSet::new(),
            store,
            routing_cache,
            sqlite_pool: None,
            shm_handle,
            command_tx_cache: None,
            shm_listener: None,
        })
    }

    /// Create channel manager with shared memory support
    pub fn with_shared_memory(
        routing_cache: Arc<aether_routing::RoutingCache>,
        sqlite_pool: sqlx::SqlitePool,
        shm_handle: Arc<ShmWriterHandle>,
        channel_health_writer: Option<Arc<ShmChannelHealthWriterHandle>>,
        command_tx_cache: Option<Arc<crate::api::command_cache::CommandTxCache>>,
    ) -> Result<Self> {
        let mut store = ShmDataStore::new(Arc::clone(&shm_handle), Arc::clone(&routing_cache))?;
        if let Some(writer) = channel_health_writer.as_ref() {
            store = store.with_channel_health_writer(Arc::clone(writer));
        }
        Ok(Self {
            channels: Self::create_channel_slots(),
            active_channel_ids: DashSet::new(),
            store: Arc::new(store),
            routing_cache,
            sqlite_pool: Some(sqlite_pool),
            shm_handle,
            command_tx_cache,
            shm_listener: None,
        })
    }

    /// Configure SHM command listener for event-driven M2C dispatch
    pub fn with_shm_listener(mut self, shutdown_rx: tokio::sync::watch::Receiver<bool>) -> Self {
        let uds_path = std::env::var("AETHER_M2C_SOCKET").ok();
        let listener = ShmCommandListener::new(uds_path.as_deref(), shutdown_rx);
        self.shm_listener = Some(Arc::new(listener));
        self
    }

    /// Start the SHM command listener background task
    pub fn start_shm_listener(&self) -> Option<tokio::task::JoinHandle<std::io::Result<()>>> {
        let listener = self.shm_listener.clone()?;
        Some(tokio::spawn(async move { listener.run().await }))
    }

    /// Get SHM listener for channel registration (internal use)
    pub fn shm_listener(&self) -> Option<&Arc<ShmCommandListener>> {
        self.shm_listener.as_ref()
    }

    /// Get the SHM writer handle for routing reload and SHM rebuild.
    pub fn shm_handle(&self) -> &Arc<ShmWriterHandle> {
        &self.shm_handle
    }

    /// Shared SHM writer used by every acquisition channel and by explicit
    /// telemetry/signal simulation writes.
    pub fn data_store(&self) -> &Arc<ShmDataStore> {
        &self.store
    }

    // ========================================================================
    // Channel Lifecycle
    // ========================================================================

    /// Remove channel with graceful shutdown.
    pub async fn remove_channel(&self, channel_id: u32) -> Result<()> {
        // Unregister from cache before removing channel
        if let Some(ref cache) = self.command_tx_cache {
            cache.unregister(channel_id);
        }

        // Unregister from SHM listener (event-driven M2C via UDS)
        if let Some(ref listener) = self.shm_listener {
            listener.unregister_channel(channel_id);
        }

        // Remove from active channel index
        self.active_channel_ids.remove(&channel_id);

        // O(1) atomic swap
        let slot = self
            .channels
            .get(channel_id as usize)
            .ok_or_else(|| IoError::invalid_channel_id(channel_id))?;

        match slot.swap(None) {
            Some(entry) => {
                self.shutdown_channel_entry(&entry, channel_id).await?;
                info!("Ch{} removed (graceful shutdown)", channel_id);
                Ok(())
            },
            _ => Err(IoError::channel_not_found(channel_id)),
        }
    }

    /// Shutdown a channel entry gracefully with timeout.
    async fn shutdown_channel_entry(&self, entry: &ChannelEntry, channel_id: u32) -> Result<()> {
        // 1. Send shutdown signal to unified task (non-blocking)
        entry.shutdown();

        // 2. Await task exit with timeout, then force-abort via the AbortHandle
        //    captured before moving the JoinHandle into timeout. Dropping a
        //    JoinHandle does NOT abort the task in Tokio — without AbortHandle
        //    a timed-out task would keep running and could still poll/write.
        if let Some(handle) = entry.take_task_handle() {
            let abort_handle = handle.abort_handle();
            if tokio::time::timeout(std::time::Duration::from_millis(500), handle)
                .await
                .is_err()
            {
                warn!("Ch{} task did not exit in 500ms, aborting", channel_id);
                abort_handle.abort();
            }
        }

        Ok(())
    }

    // ========================================================================
    // Channel Query Methods
    // ========================================================================

    /// Get channel entry by ID (O(1) lock-free access ~5ns)
    #[inline]
    pub fn get_channel(&self, channel_id: u32) -> Option<Arc<ChannelEntry>> {
        self.channels.get(channel_id as usize)?.load_full()
    }

    /// Get channel IDs (O(n) where n = active channels)
    pub fn get_channel_ids(&self) -> Vec<u32> {
        self.active_channel_ids.iter().map(|id| *id).collect()
    }

    /// Get channel count (O(1))
    pub fn channel_count(&self) -> usize {
        self.active_channel_ids.len()
    }

    /// Get running channel count (O(n) where n = active channels)
    pub async fn running_channel_count(&self) -> usize {
        let mut count = 0;
        for channel_id in self.active_channel_ids.iter() {
            if let Some(entry) = self
                .channels
                .get(*channel_id as usize)
                .and_then(|s| s.load_full())
                && entry.is_connected()
            {
                count += 1;
            }
        }
        count
    }

    /// Get channel metadata
    pub fn get_channel_metadata(&self, channel_id: u32) -> Option<(String, String)> {
        self.channels
            .get(channel_id as usize)?
            .load_full()
            .map(|entry| {
                (
                    entry.metadata.name.to_string(),
                    format!("{:?}", entry.metadata.protocol_type),
                )
            })
    }

    /// Get all channel stats (O(n) where n = active channels)
    pub async fn get_all_channel_stats(&self) -> Vec<ChannelStats> {
        let mut stats = Vec::with_capacity(self.active_channel_ids.len());
        for channel_id in self.active_channel_ids.iter() {
            let id = *channel_id;
            if let Some(entry) = self.channels.get(id as usize).and_then(|s| s.load_full()) {
                stats.push(entry.get_stats(id).await);
            }
        }
        stats
    }

    /// Connect all channels
    pub async fn connect_all_channels(&self) -> Result<()> {
        const MAX_CONCURRENT_CONNECTS: usize = 16;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTS));

        let mut connect_tasks = Vec::with_capacity(self.active_channel_ids.len());

        for channel_id_ref in self.active_channel_ids.iter() {
            let channel_id = *channel_id_ref;
            if let Some(entry) = self
                .channels
                .get(channel_id as usize)
                .and_then(|s| s.load_full())
            {
                let entry_clone = Arc::clone(&entry);
                let sem = Arc::clone(&semaphore);

                let task = tokio::spawn(async move {
                    let _permit = sem.acquire().await;
                    match entry_clone.connect().await {
                        Ok(_) => Ok(()),
                        Err(e) => {
                            error!("Ch{} connect err: {}", channel_id, e);
                            Err(e)
                        },
                    }
                });

                connect_tasks.push(task);
            }
        }

        let mut failed_channels = Vec::new();
        for task in connect_tasks {
            if let Ok(Err(e)) = task.await {
                failed_channels.push(e);
            }
        }

        if failed_channels.is_empty() {
            Ok(())
        } else {
            Err(IoError::batch(format!(
                "Failed to connect {} channels",
                failed_channels.len()
            )))
        }
    }

    /// Cleanup all resources
    pub async fn cleanup(&self) -> Result<()> {
        info!("Cleanup started");

        // Remove all channels
        let channel_ids: Vec<u32> = self.get_channel_ids();
        for channel_id in channel_ids {
            let _ = self.remove_channel(channel_id).await;
        }

        info!("Cleanup done");
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    /// Create test routing cache for unit tests
    fn create_test_routing_cache() -> Arc<aether_routing::RoutingCache> {
        Arc::new(aether_routing::RoutingCache::new())
    }

    #[tokio::test]
    async fn test_channel_manager_creation() {
        let shm_handle = crate::test_utils::create_test_shm_handle();
        let routing_cache = create_test_routing_cache();
        let manager = ChannelManager::new(shm_handle, routing_cache).unwrap();

        assert_eq!(manager.channel_count(), 0);
        assert_eq!(manager.get_channel_ids().len(), 0);
    }

    #[tokio::test]
    async fn test_channel_manager_running_count() {
        let shm_handle = crate::test_utils::create_test_shm_handle();
        let routing_cache = create_test_routing_cache();
        let manager = ChannelManager::new(shm_handle, routing_cache).unwrap();

        let count = manager.running_channel_count().await;
        assert_eq!(count, 0);
    }
}
