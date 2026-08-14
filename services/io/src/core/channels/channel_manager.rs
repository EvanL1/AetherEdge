//! Channel lifecycle management module
//!
//! Handles channel creation, removal, and lifecycle operations.
//! Channel entry types are in `channel_entry`, task logic in `channel_task`,
//! and creation/factory methods in `channel_creation`.

use arc_swap::ArcSwapOption;
use dashmap::DashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use tracing::{info, warn};

use crate::core::channels::channel_entry::{ChannelEntry, ChannelStats, MAX_CHANNELS};
use crate::core::channels::command_ledger::{
    CommandLedger, CommandLedgerRecord, CommandLedgerStats,
};
use crate::core::channels::command_outcome::{
    CommandOutcome, CommandOutcomeStats, CommandOutcomeTracker,
};
use crate::core::channels::shm_listener::ShmCommandListener;
use crate::error::{IoError, Result};
use crate::protocols::core::file_logging::{FileLogStats, LogWorker};
use crate::store::ShmDataStore;
use aether_domain::CommandId;
use aether_shm_bridge::{ShmChannelHealthWriterHandle, ShmWriterHandle};

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
    /// Per-slot lifecycle reservation shared by create/remove. This prevents
    /// duplicate protocol construction before the ArcSwap publication point.
    lifecycle_in_progress: Vec<AtomicBool>,
    /// Creation fence held for the complete compile-and-publication path.
    /// Shutdown takes the write side before enumerating channels, which makes
    /// it impossible for an in-flight reconciliation to publish a late runtime.
    shutdown_gate: RwLock<()>,
    /// Fast rejection for lifecycle callers after shutdown has begun.
    shutting_down: AtomicBool,
    /// Bounded post-acceptance command lifecycle registry.
    pub(super) command_outcomes: Arc<CommandOutcomeTracker>,
    /// Durable identity, deduplication, and device-outcome ledger.
    pub(super) command_ledger: Option<Arc<CommandLedger>>,
    /// One lazily-created disk actor retained for this manager's full lifetime.
    file_log_worker: std::sync::Mutex<Option<Arc<LogWorker>>>,
    /// Shared authoritative SHM store used by all channels.
    pub(super) store: Arc<ShmDataStore>,
    /// Routing cache for C2M/M2C routing (public for reload operations)
    pub routing_cache: Arc<aether_routing::RoutingCache>,
    /// Runtime-swappable shared memory handle (writer + index, rebuilt on routing reload)
    pub(super) shm_handle: Arc<ShmWriterHandle>,
    /// Runtime-swappable channel-health writer paired with the point plane.
    pub(super) channel_health_writer: Option<Arc<ShmChannelHealthWriterHandle>>,
    // ========== SHM Command Listener (Event-driven M2C via UDS) ==========
    /// SHM command listener for event-driven M2C command dispatch (UDS path, self-healing)
    pub(super) shm_listener: Option<Arc<ShmCommandListener>>,
}

pub(super) struct ChannelLifecycleReservation<'a> {
    flag: &'a AtomicBool,
}

impl Drop for ChannelLifecycleReservation<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
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

    fn create_lifecycle_slots() -> Vec<AtomicBool> {
        (0..MAX_CHANNELS).map(|_| AtomicBool::new(false)).collect()
    }

    pub(super) fn reserve_channel_lifecycle(
        &self,
        channel_id: u32,
    ) -> Result<ChannelLifecycleReservation<'_>> {
        let flag = self
            .lifecycle_in_progress
            .get(channel_id as usize)
            .ok_or_else(|| IoError::invalid_channel_id(channel_id))?;
        flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| IoError::resource(format!("channel {channel_id} lifecycle is busy")))?;
        Ok(ChannelLifecycleReservation { flag })
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
            lifecycle_in_progress: Self::create_lifecycle_slots(),
            shutdown_gate: RwLock::new(()),
            shutting_down: AtomicBool::new(false),
            command_outcomes: Arc::new(CommandOutcomeTracker::default()),
            command_ledger: None,
            file_log_worker: std::sync::Mutex::new(None),
            store,
            routing_cache,
            shm_handle,
            channel_health_writer: None,
            shm_listener: None,
        })
    }

    /// Create channel manager with shared memory support
    pub fn with_shared_memory(
        routing_cache: Arc<aether_routing::RoutingCache>,
        shm_handle: Arc<ShmWriterHandle>,
        channel_health_writer: Option<Arc<ShmChannelHealthWriterHandle>>,
    ) -> Result<Self> {
        let mut store = ShmDataStore::new(Arc::clone(&shm_handle), Arc::clone(&routing_cache))?;
        if let Some(writer) = channel_health_writer.as_ref() {
            store = store.with_channel_health_writer(Arc::clone(writer));
        }
        Ok(Self {
            channels: Self::create_channel_slots(),
            active_channel_ids: DashSet::new(),
            lifecycle_in_progress: Self::create_lifecycle_slots(),
            shutdown_gate: RwLock::new(()),
            shutting_down: AtomicBool::new(false),
            command_outcomes: Arc::new(CommandOutcomeTracker::default()),
            command_ledger: None,
            file_log_worker: std::sync::Mutex::new(None),
            store: Arc::new(store),
            routing_cache,
            shm_handle,
            channel_health_writer,
            shm_listener: None,
        })
    }

    /// Configure the single durable command listener from the environment.
    pub fn with_shm_listener(
        self,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        ledger: Arc<CommandLedger>,
    ) -> Self {
        let uds_path = std::env::var("AETHER_M2C_SOCKET").ok();
        self.with_shm_listener_path(shutdown_rx, uds_path.as_deref(), ledger)
    }

    /// Configure the command listener and its mandatory ledger.
    pub fn with_shm_listener_path(
        mut self,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        uds_path: Option<&str>,
        ledger: Arc<CommandLedger>,
    ) -> Self {
        let listener = ShmCommandListener::with_outcomes_and_ledger(
            uds_path,
            shutdown_rx,
            Arc::clone(&self.command_outcomes),
            Arc::clone(&ledger),
        );
        self.command_ledger = Some(ledger);
        self.shm_listener = Some(Arc::new(listener));
        self
    }

    /// Composes a durable command ledger without starting the UDS listener.
    ///
    /// This is used by library and API test compositions. Production uses
    /// [`Self::with_shm_listener_path`], which makes the ledger mandatory.
    pub fn with_command_ledger(mut self, ledger: Arc<CommandLedger>) -> Self {
        self.command_ledger = Some(ledger);
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

    /// Get the coordinated channel-health writer for liveness observation.
    pub fn channel_health_writer(&self) -> Option<&Arc<ShmChannelHealthWriterHandle>> {
        self.channel_health_writer.as_ref()
    }

    /// Fence new channel creation and command dispatch before shutdown.
    ///
    /// Taking the write side waits for any creation that already holds a read
    /// lease to finish publication. Once this returns, the active-channel set
    /// is a stable upper bound for the shutdown pass and no reconciler can
    /// recreate a channel behind it.
    pub fn begin_shutdown(&self) -> Result<()> {
        // Publish the rejection fence before waiting on an in-flight creator,
        // otherwise a stream of new read leases could delay the write lease.
        self.shutting_down.store(true, Ordering::Release);
        if let Some(listener) = &self.shm_listener {
            listener.quiesce();
        }
        let _gate = self
            .shutdown_gate
            .write()
            .map_err(|_| IoError::state("channel shutdown gate was poisoned"))?;
        Ok(())
    }

    /// Whether the channel lifecycle has entered its terminal shutdown phase.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    /// Query the latest bounded lifecycle state for a command ID.
    pub fn command_outcome_by_transport_id(&self, transport_id: &str) -> Option<CommandOutcome> {
        self.command_outcomes.outcome(transport_id)
    }

    /// Return process-lifetime post-acceptance command counters.
    pub fn command_outcome_stats(&self) -> CommandOutcomeStats {
        self.command_outcomes.stats()
    }

    /// Query one command identity from the durable outcome ledger.
    ///
    /// `Ok(None)` means either the ledger is not composed in this runtime or
    /// the identity has no retained record. Storage failures remain explicit.
    pub async fn command_ledger_record(
        &self,
        command_id: CommandId,
    ) -> Result<Option<CommandLedgerRecord>> {
        let Some(ledger) = self.command_ledger.as_ref() else {
            return Ok(None);
        };
        ledger
            .query(command_id)
            .await
            .map_err(|error| IoError::storage(error.to_string()))
    }

    /// Return the current durable ledger population and capacity telemetry.
    pub async fn command_ledger_stats(&self) -> Result<Option<CommandLedgerStats>> {
        let Some(ledger) = self.command_ledger.as_ref() else {
            return Ok(None);
        };
        ledger
            .stats()
            .await
            .map(Some)
            .map_err(|error| IoError::storage(error.to_string()))
    }

    /// Whether this composition includes the mandatory production ledger.
    #[must_use]
    pub const fn has_command_ledger(&self) -> bool {
        self.command_ledger.is_some()
    }

    /// Return process-wide bounded file-log admission and disk-worker state.
    pub fn file_log_stats(&self) -> Option<FileLogStats> {
        self.file_log_worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|worker| worker.stats())
    }

    pub(crate) fn file_log_worker(&self) -> Result<Arc<LogWorker>> {
        let mut current = self
            .file_log_worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(worker) = current.as_ref() {
            if worker.stats().worker_running {
                return Ok(Arc::clone(worker));
            }
            return Err(IoError::resource("channel file-log worker is not running"));
        }
        let worker = LogWorker::spawn_default().map_err(|error| {
            IoError::resource(format!("channel file-log worker unavailable: {error}"))
        })?;
        *current = Some(Arc::clone(&worker));
        Ok(worker)
    }

    /// Drain and flush the manager-owned file-log actor after all channels stop.
    pub fn shutdown_file_logging(&self) -> Result<()> {
        let worker = self
            .file_log_worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(worker) = worker else {
            return Ok(());
        };
        worker.shutdown_blocking().map_err(|error| {
            IoError::resource(format!("channel file-log shutdown failed: {error}"))
        })
    }

    #[cfg(test)]
    pub(crate) fn install_file_log_worker_for_test(&self, worker: Arc<LogWorker>) {
        self.file_log_worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(worker);
    }

    pub(super) fn acquire_creation_lease(&self) -> Result<RwLockReadGuard<'_, ()>> {
        if self.is_shutting_down() {
            return Err(IoError::state("channel manager is shutting down"));
        }
        let guard = self
            .shutdown_gate
            .read()
            .map_err(|_| IoError::state("channel shutdown gate was poisoned"))?;
        if self.is_shutting_down() {
            return Err(IoError::state("channel manager is shutting down"));
        }
        Ok(guard)
    }

    // ========================================================================
    // Channel Lifecycle
    // ========================================================================

    /// Remove channel with graceful shutdown.
    pub async fn remove_channel(&self, channel_id: u32) -> Result<()> {
        let _reservation = self.reserve_channel_lifecycle(channel_id)?;

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
                match self.shutdown_channel_entry(&entry, channel_id).await {
                    Ok(()) => {
                        info!("Ch{} removed (graceful shutdown)", channel_id);
                        Ok(())
                    },
                    Err(error) => {
                        // Retain a recovery handle for the outer shutdown pass;
                        // the channel is no longer active but must not become an
                        // unobservable detached writer.
                        slot.store(Some(entry));
                        Err(error)
                    },
                }
            },
            _ => Err(IoError::channel_not_found(channel_id)),
        }
    }

    /// Shutdown a channel entry gracefully with timeout.
    async fn shutdown_channel_entry(&self, entry: &ChannelEntry, channel_id: u32) -> Result<()> {
        // 1. Reserve a bounded window for a graceful shutdown request. A full
        // command queue must not silently turn graceful shutdown into an abort.
        match tokio::time::timeout(std::time::Duration::from_millis(100), entry.shutdown()).await {
            Ok(true) => {},
            Ok(false) => warn!("Ch{} task command receiver already closed", channel_id),
            Err(_) => warn!("Ch{} shutdown command queue remained full", channel_id),
        }

        // 2. Await task exit with timeout, then abort and explicitly observe
        //    cancellation. Merely dropping a timed-out JoinHandle would detach
        //    a task that could still poll or write during the final snapshot.
        if let Some(mut handle) = entry.take_task_handle() {
            match tokio::time::timeout(std::time::Duration::from_millis(500), &mut handle).await {
                Ok(Ok(())) => {},
                Ok(Err(error)) => {
                    warn!(channel_id, %error, "channel task exited abnormally");
                },
                Err(_) => {
                    warn!("Ch{} task did not exit in 500ms, aborting", channel_id);
                    handle.abort();
                    if let Err(error) = handle.await
                        && !error.is_cancelled()
                    {
                        warn!(channel_id, %error, "channel task abort join failed");
                    }
                },
            }
        }

        // Always reconcile after the task is known stopped. A normal task has
        // already drained queued commands and yields zero rows; forced abort,
        // panic, or a previous failed remove is repaired conservatively here.
        self.reconcile_stopped_channel_commands(channel_id).await?;

        Ok(())
    }

    pub(crate) async fn reconcile_stopped_channel_commands(&self, channel_id: u32) -> Result<()> {
        let Some(ledger) = self.command_ledger.as_ref() else {
            return Ok(());
        };
        let mut last_error = None;
        for attempt in 0..3_u32 {
            match ledger.reconcile_stopped_channel(channel_id).await {
                Ok(reconciled) => {
                    if reconciled.queued_failed > 0 || reconciled.dispatching_possibly_applied > 0 {
                        warn!(
                            channel_id,
                            queued_failed = reconciled.queued_failed,
                            dispatching_possibly_applied = reconciled.dispatching_possibly_applied,
                            "reconciled durable commands after channel-task exit"
                        );
                    }
                    return Ok(());
                },
                Err(error) => {
                    warn!(
                        channel_id,
                        attempt = attempt + 1,
                        %error,
                        "failed to reconcile stopped channel commands"
                    );
                    last_error = Some(error);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            10 * u64::from(attempt + 1),
                        ))
                        .await;
                    }
                },
            }
        }
        ledger.record_outcome_persistence_failure();
        Err(IoError::storage(format!(
            "failed to reconcile stopped channel {channel_id} commands after bounded retries: {}",
            last_error.map_or_else(
                || "unknown ledger error".to_string(),
                |error| error.to_string()
            )
        )))
    }

    // ========================================================================
    // Channel Query Methods
    // ========================================================================

    /// Get channel entry by ID (O(1) lock-free access ~5ns)
    #[inline]
    pub fn get_channel(&self, channel_id: u32) -> Option<Arc<ChannelEntry>> {
        self.channels.get(channel_id as usize)?.load_full()
    }

    pub(crate) fn clear_channel_slot_after_forced_shutdown(&self, channel_id: u32) {
        self.active_channel_ids.remove(&channel_id);
        if let Some(slot) = self.channels.get(channel_id as usize) {
            slot.store(None);
        }
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
    pub fn running_channel_count(&self) -> usize {
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

    /// Get all channel stats (O(n) where n = active channels)
    pub fn get_all_channel_stats(&self) -> Vec<ChannelStats> {
        let mut stats = Vec::with_capacity(self.active_channel_ids.len());
        for channel_id in self.active_channel_ids.iter() {
            let id = *channel_id;
            if let Some(entry) = self.channels.get(id as usize).and_then(|s| s.load_full()) {
                stats.push(entry.get_stats());
            }
        }
        stats
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

        let count = manager.running_channel_count();
        assert_eq!(count, 0);
    }

    #[test]
    fn file_log_worker_is_unique_and_retained_across_handler_reload() {
        let manager = ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            create_test_routing_cache(),
        )
        .expect("test manager");

        let first = manager.file_log_worker().expect("start lazy worker");
        drop(first);
        assert!(
            manager
                .file_log_stats()
                .is_some_and(|stats| stats.worker_running)
        );

        let reloaded = manager.file_log_worker().expect("reuse manager worker");
        let retained = manager
            .file_log_worker
            .lock()
            .expect("worker owner lock")
            .as_ref()
            .cloned()
            .expect("manager retains worker");
        assert!(Arc::ptr_eq(&reloaded, &retained));

        drop(reloaded);
        drop(retained);
        manager
            .shutdown_file_logging()
            .expect("explicit file-log drain");
        assert!(
            manager
                .file_log_stats()
                .is_some_and(|stats| !stats.worker_running)
        );
    }
}
