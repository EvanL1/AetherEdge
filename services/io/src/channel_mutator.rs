//! SQLite-backed production adapter for governed I/O channel commissioning.
//!
//! SQLite is authoritative for desired configuration. The protocol runtime is
//! a rebuildable projection: after desired state commits, a runtime failure is
//! reported as a degraded receipt rather than a retryable command failure.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use aether_domain::ChannelId;
use aether_ports::{
    ChannelDefinition, ChannelDesiredStateObservation, ChannelLoggingPolicy, ChannelMutation,
    ChannelMutationKind, ChannelMutationReceipt, ChannelMutator, ChannelParameterValue,
    ChannelParameters, ChannelPatch, ChannelReconciler, ChannelReconciliationItem,
    ChannelReconciliationReceipt, ChannelReconciliationScope, ChannelRevision,
    ChannelRuntimeProjection, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use dashmap::DashMap;
use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::sync::{Mutex, RwLock};

use crate::core::channels::ChannelManager;
use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};
use crate::store::SqliteShmTopologyProjector;

const MAX_CHANNEL_ID: u32 = 10_000;
const MAX_SQLITE_REVISION: u64 = i64::MAX as u64;

/// Narrow lifecycle surface used to project durable channel configuration.
///
/// Implementations must make `activate` and `fence` safe when the requested
/// runtime state already exists. Errors use port-level recovery semantics so
/// interface/application layers never depend on the IO service error enum.
#[async_trait]
pub trait ChannelRuntimeLifecycle: Send + Sync + 'static {
    /// Validates that this runtime can instantiate the complete configuration.
    fn validate(&self, config: &ChannelConfig) -> PortResult<()>;

    /// Returns whether a runtime projection currently occupies the channel.
    fn is_present(&self, channel_id: ChannelId) -> bool;

    /// Returns the runtime identities currently owned by the projection.
    fn channel_ids(&self) -> Vec<ChannelId>;

    /// Ensures an active runtime exists for the supplied desired state.
    async fn activate(&self, config: Arc<ChannelConfig>) -> PortResult<()>;

    /// Fences acquisition/control and removes any runtime projection.
    async fn fence(&self, channel_id: ChannelId) -> PortResult<()>;
}

#[async_trait]
impl ChannelRuntimeLifecycle for ChannelManager {
    fn validate(&self, config: &ChannelConfig) -> PortResult<()> {
        validate_runtime_config(config)
    }

    fn is_present(&self, channel_id: ChannelId) -> bool {
        self.get_channel(channel_id.get()).is_some()
    }

    fn channel_ids(&self) -> Vec<ChannelId> {
        self.get_channel_ids()
            .into_iter()
            .map(ChannelId::new)
            .collect()
    }

    async fn activate(&self, config: Arc<ChannelConfig>) -> PortResult<()> {
        let entry = match self.get_channel(config.id()) {
            Some(entry) => entry,
            None => self
                .create_channel(config)
                .await
                .map_err(|_| unavailable("channel runtime activation failed"))?,
        };
        if entry.is_connected() {
            return Ok(());
        }
        if entry.connect().await.is_err() {
            // Do not leave a failed projection that a later same-state enable
            // could mistake for an active runtime.
            let _ = self.remove_channel(entry.channel_config.id()).await;
            return Err(unavailable("channel runtime connection failed"));
        }
        Ok(())
    }

    async fn fence(&self, channel_id: ChannelId) -> PortResult<()> {
        if self.get_channel(channel_id.get()).is_none() {
            return Ok(());
        }
        self.remove_channel(channel_id.get())
            .await
            .map_err(|_| unavailable("channel runtime fencing failed"))
    }
}

/// Default SQLite implementation of [`ChannelMutator`].
///
/// Compatibility mutations without a revision are still serialized by
/// channel. Explicit revisions are also checked in SQL, preventing a staged
/// legacy writer from being silently overwritten.
#[derive(Clone)]
pub struct SqliteChannelMutator {
    pool: SqlitePool,
    runtime: Arc<dyn ChannelRuntimeLifecycle>,
    channel_locks: Arc<DashMap<u32, Arc<Mutex<()>>>>,
    allocation_lock: Arc<Mutex<()>>,
    projection_gate: Arc<RwLock<()>>,
    topology: Option<Arc<SqliteShmTopologyProjector>>,
}

impl SqliteChannelMutator {
    /// Creates the production adapter around the IO channel manager.
    #[must_use]
    pub fn new(pool: SqlitePool, manager: Arc<ChannelManager>) -> Self {
        Self::with_runtime(pool, manager)
    }

    /// Creates the production adapter with shared runtime and SHM topology ownership.
    #[must_use]
    pub fn new_with_topology(
        pool: SqlitePool,
        manager: Arc<ChannelManager>,
        topology: Arc<SqliteShmTopologyProjector>,
    ) -> Self {
        let mut adapter = Self::with_runtime(pool, manager);
        adapter.topology = Some(topology);
        adapter
    }

    /// Creates an adapter around an alternate runtime lifecycle.
    ///
    /// This constructor supports deterministic conformance and fault tests.
    #[must_use]
    pub fn with_runtime<R>(pool: SqlitePool, runtime: Arc<R>) -> Self
    where
        R: ChannelRuntimeLifecycle,
    {
        Self {
            pool,
            runtime,
            channel_locks: Arc::new(DashMap::new()),
            allocation_lock: Arc::new(Mutex::new(())),
            projection_gate: Arc::new(RwLock::new(())),
            topology: None,
        }
    }

    fn channel_lock(&self, channel_id: ChannelId) -> Arc<Mutex<()>> {
        Arc::clone(
            self.channel_locks
                .entry(channel_id.get())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .value(),
        )
    }

    async fn create(&self, definition: ChannelDefinition) -> PortResult<ChannelMutationReceipt> {
        match definition.requested_channel_id() {
            Some(channel_id) => {
                validate_channel_id(channel_id)?;
                let lock = self.channel_lock(channel_id);
                let _guard = lock.lock().await;
                self.create_locked(channel_id, &definition).await
            },
            None => {
                // Allocation has no natural key yet; serialize only that
                // window, then also acquire the selected channel key.
                let _allocation_guard = self.allocation_lock.lock().await;
                let channel_id = self.next_available_channel_id().await?;
                let lock = self.channel_lock(channel_id);
                let _guard = lock.lock().await;
                self.create_locked(channel_id, &definition).await
            },
        }
    }

    async fn create_locked(
        &self,
        channel_id: ChannelId,
        definition: &ChannelDefinition,
    ) -> PortResult<ChannelMutationReceipt> {
        if self.runtime.is_present(channel_id) {
            return Err(conflict("channel identity is already occupied at runtime"));
        }

        let config = definition_to_config(channel_id, definition)?;
        self.runtime.validate(&config)?;
        let config_json = encode_config(&config)?;
        let revision: i64 = sqlx::query_scalar(
            "INSERT INTO channels \
             (channel_id, name, protocol, enabled, config, revision) \
             SELECT ?, ?, ?, ?, ?, \
                    COALESCE((SELECT last_revision + 1 \
                              FROM channel_revision_tombstones \
                              WHERE channel_id = ?), 1) \
             RETURNING revision",
        )
        .bind(i64::from(channel_id.get()))
        .bind(&config.core.name)
        .bind(&config.core.protocol)
        .bind(config.core.enabled)
        .bind(&config_json)
        .bind(i64::from(channel_id.get()))
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_database_error(error, "create channel desired state"))?;
        let revision = stored_revision(revision)?;
        let committed = StoredChannel::from_committed_config(&config, config_json, revision);

        let projection = if config.core.enabled {
            match self.runtime.activate(Arc::new(config)).await {
                Ok(()) => ChannelRuntimeProjection::Active,
                Err(_) => ChannelRuntimeProjection::Degraded,
            }
        } else {
            ChannelRuntimeProjection::Stopped
        };
        let (resulting_revision, desired_enabled, projection) =
            self.observe_reconcile(&committed, projection).await;
        Ok(ChannelMutationReceipt::new(
            channel_id,
            ChannelMutationKind::Create,
            resulting_revision,
            desired_enabled,
            projection,
        ))
    }

    async fn update(
        &self,
        channel_id: ChannelId,
        expected_revision: Option<ChannelRevision>,
        patch: ChannelPatch,
    ) -> PortResult<ChannelMutationReceipt> {
        validate_channel_id(channel_id)?;
        if patch.is_empty() {
            return Err(invalid("channel update patch is empty"));
        }
        let lock = self.channel_lock(channel_id);
        let _guard = lock.lock().await;

        let stored = load_channel(&self.pool, channel_id).await?;
        verify_expected_revision(expected_revision, stored.revision)?;
        let next_revision = next_revision(stored.revision)?;
        let config = apply_patch(stored.to_config()?, &patch)?;
        self.runtime.validate(&config)?;
        let config_json = encode_config(&config)?;

        let result = sqlx::query(
            "UPDATE channels \
             SET name = ?, protocol = ?, config = ?, revision = ?, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE channel_id = ? AND revision = ?",
        )
        .bind(&config.core.name)
        .bind(&config.core.protocol)
        .bind(&config_json)
        .bind(revision_i64(next_revision)?)
        .bind(i64::from(channel_id.get()))
        .bind(revision_i64(stored.revision)?)
        .execute(&self.pool)
        .await
        .map_err(|error| map_database_error(error, "update channel desired state"))?;
        if result.rows_affected() != 1 {
            return Err(conflict("channel desired state changed concurrently"));
        }

        let committed = StoredChannel::from_committed_config(&config, config_json, next_revision);
        let projection = self.reconcile_after_update(channel_id, config).await;
        let (resulting_revision, desired_enabled, projection) =
            self.observe_reconcile(&committed, projection).await;
        Ok(ChannelMutationReceipt::new(
            channel_id,
            ChannelMutationKind::Update,
            resulting_revision,
            desired_enabled,
            projection,
        ))
    }

    async fn set_enabled(
        &self,
        channel_id: ChannelId,
        expected_revision: Option<ChannelRevision>,
        enabled: bool,
    ) -> PortResult<ChannelMutationReceipt> {
        validate_channel_id(channel_id)?;
        let lock = self.channel_lock(channel_id);
        let _guard = lock.lock().await;

        let stored = load_channel(&self.pool, channel_id).await?;
        verify_expected_revision(expected_revision, stored.revision)?;
        // Safety shutdown must remain possible when a protocol was removed or
        // a staged legacy writer left malformed activation configuration.
        let mut activation_config = if enabled {
            let config = stored.to_config()?;
            self.runtime.validate(&config)?;
            Some(config)
        } else {
            None
        };

        if stored.enabled == enabled {
            // Desired content is unchanged, so its revision stays stable. The
            // command still reconciles drift and is identified by request_id
            // in the application audit trail.
            let projection = if enabled {
                let config = activation_config
                    .take()
                    .ok_or_else(|| invalid("enabled channel has no activation configuration"))?;
                // A present projection can contain stale configuration. Fence
                // it before re-activation so the default ChannelManager does
                // not merely reconnect an old entry.
                if self.runtime.is_present(channel_id)
                    && self.runtime.fence(channel_id).await.is_err()
                {
                    ChannelRuntimeProjection::Degraded
                } else {
                    match self.runtime.activate(Arc::new(config)).await {
                        Ok(()) => ChannelRuntimeProjection::Active,
                        Err(_) => ChannelRuntimeProjection::Degraded,
                    }
                }
            } else {
                match self.runtime.fence(channel_id).await {
                    Ok(()) => ChannelRuntimeProjection::Stopped,
                    Err(_) => ChannelRuntimeProjection::Degraded,
                }
            };
            let (resulting_revision, desired_enabled, projection) =
                self.observe_reconcile(&stored, projection).await;
            return Ok(ChannelMutationReceipt::new(
                channel_id,
                if enabled {
                    ChannelMutationKind::Enable
                } else {
                    ChannelMutationKind::Disable
                },
                resulting_revision,
                desired_enabled,
                projection,
            ));
        }

        let next_revision = next_revision(stored.revision)?;
        if !enabled && self.runtime.fence(channel_id).await.is_err() {
            // A fence can partially remove a runtime before returning an
            // error. Re-read authority before attempting any restoration.
            restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
            return Err(unavailable("channel runtime could not be fenced"));
        }

        let update = sqlx::query(
            "UPDATE channels \
             SET enabled = ?, revision = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE channel_id = ? AND revision = ?",
        )
        .bind(enabled)
        .bind(revision_i64(next_revision)?)
        .bind(i64::from(channel_id.get()))
        .bind(revision_i64(stored.revision)?)
        .execute(&self.pool)
        .await;

        match update {
            Ok(result) if result.rows_affected() == 1 => {},
            Ok(_) => {
                if !enabled {
                    restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
                }
                return Err(conflict("channel desired state changed concurrently"));
            },
            Err(error) => {
                if !enabled {
                    restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
                }
                return Err(map_database_error(error, "set channel enabled state"));
            },
        }

        let mut committed = stored.clone();
        committed.enabled = enabled;
        committed.revision = next_revision;
        let projection = if enabled {
            let mut config = activation_config
                .take()
                .ok_or_else(|| invalid("enabled channel has no activation configuration"))?;
            config.core.enabled = true;
            // Desired state is committed. Remove any stale disabled-state
            // zombie before constructing the active projection.
            if self.runtime.is_present(channel_id) && self.runtime.fence(channel_id).await.is_err()
            {
                ChannelRuntimeProjection::Degraded
            } else {
                match self.runtime.activate(Arc::new(config)).await {
                    Ok(()) => ChannelRuntimeProjection::Active,
                    Err(_) => ChannelRuntimeProjection::Degraded,
                }
            }
        } else {
            ChannelRuntimeProjection::Stopped
        };
        let (resulting_revision, desired_enabled, projection) =
            self.observe_reconcile(&committed, projection).await;

        Ok(ChannelMutationReceipt::new(
            channel_id,
            if enabled {
                ChannelMutationKind::Enable
            } else {
                ChannelMutationKind::Disable
            },
            resulting_revision,
            desired_enabled,
            projection,
        ))
    }

    async fn delete(
        &self,
        channel_id: ChannelId,
        expected_revision: Option<ChannelRevision>,
    ) -> PortResult<ChannelMutationReceipt> {
        validate_channel_id(channel_id)?;
        let lock = self.channel_lock(channel_id);
        let _guard = lock.lock().await;

        let stored = load_channel(&self.pool, channel_id).await?;
        verify_expected_revision(expected_revision, stored.revision)?;
        let tombstone_revision = next_revision(stored.revision)?;

        // Fail before fencing in the common case, then repeat inside the
        // transaction to close the race with governed routing commands.
        if action_route_count(&self.pool, channel_id).await? != 0 {
            return Err(conflict(
                "remove governed action routes before deleting their channel",
            ));
        }
        if self.runtime.fence(channel_id).await.is_err() {
            restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
            return Err(unavailable("channel runtime could not be fenced"));
        }

        let mut transaction = match self.pool.begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
                return Err(map_database_error(error, "begin channel deletion"));
            },
        };
        if let Err(error) = delete_desired_state(
            &mut transaction,
            channel_id,
            stored.revision,
            tombstone_revision,
        )
        .await
        {
            let _ = transaction.rollback().await;
            restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
            return Err(error);
        }
        if let Err(error) = transaction.commit().await {
            restore_authoritative_runtime(&self.pool, &*self.runtime, &stored).await;
            return Err(map_database_error(error, "commit channel deletion"));
        }

        Ok(ChannelMutationReceipt::new(
            channel_id,
            ChannelMutationKind::Delete,
            tombstone_revision,
            false,
            ChannelRuntimeProjection::Removed,
        ))
    }

    async fn reconcile_after_update(
        &self,
        channel_id: ChannelId,
        config: ChannelConfig,
    ) -> ChannelRuntimeProjection {
        if !config.core.enabled {
            return match self.runtime.fence(channel_id).await {
                Ok(()) => ChannelRuntimeProjection::Stopped,
                Err(_) => ChannelRuntimeProjection::Degraded,
            };
        }
        if self.runtime.is_present(channel_id) && self.runtime.fence(channel_id).await.is_err() {
            return ChannelRuntimeProjection::Degraded;
        }
        match self.runtime.activate(Arc::new(config)).await {
            Ok(()) => ChannelRuntimeProjection::Active,
            Err(_) => ChannelRuntimeProjection::Degraded,
        }
    }

    async fn observe_reconcile(
        &self,
        initial: &StoredChannel,
        projection: ChannelRuntimeProjection,
    ) -> (ChannelRevision, bool, ChannelRuntimeProjection) {
        match load_channel(&self.pool, initial.channel_id).await {
            Ok(latest) if initial.same_desired_state(&latest) => {
                (latest.revision, latest.enabled, projection)
            },
            Ok(latest) => {
                // A legacy/external writer changed authoritative desired state
                // while runtime reconciliation was in flight. Never report the
                // stale projection as active or leave it able to acquire/control
                // against superseded parameters. Rebuilding the newly-observed
                // state belongs to a subsequent explicit reconciliation.
                let _ = self.runtime.fence(initial.channel_id).await;
                (
                    latest.revision,
                    latest.enabled,
                    ChannelRuntimeProjection::Degraded,
                )
            },
            Err(error) if error.kind() == PortErrorKind::NotFound => {
                let _ = self.runtime.fence(initial.channel_id).await;
                let revision = load_tombstone_revision(&self.pool, initial.channel_id)
                    .await
                    .unwrap_or(initial.revision);
                (revision, false, ChannelRuntimeProjection::Degraded)
            },
            Err(_) => {
                // Desired-state authority cannot be confirmed, so fail closed
                // instead of leaving a possibly stale projection online.
                let _ = self.runtime.fence(initial.channel_id).await;
                (
                    initial.revision,
                    initial.enabled,
                    ChannelRuntimeProjection::Degraded,
                )
            },
        }
    }

    async fn next_available_channel_id(&self) -> PortResult<ChannelId> {
        let occupied: Vec<i64> = sqlx::query_scalar(
            "SELECT channel_id FROM channels \
             WHERE channel_id >= 1 AND channel_id < 10000 \
             UNION \
             SELECT channel_id FROM channel_revision_tombstones \
             WHERE channel_id >= 1 AND channel_id < 10000 \
             ORDER BY channel_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| map_database_error(error, "allocate channel identity"))?;
        let mut candidate = 1_u32;
        for occupied in occupied {
            let occupied = u32::try_from(occupied)
                .map_err(|_| invalid("stored channel identity is outside the u32 range"))?;
            if occupied == candidate {
                candidate = candidate.saturating_add(1);
            } else if occupied > candidate {
                break;
            }
        }
        if candidate >= MAX_CHANNEL_ID {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                "no channel identity remains below 10000",
            ));
        }
        Ok(ChannelId::new(candidate))
    }

    async fn reconciliation_ids(&self) -> PortResult<Vec<ChannelId>> {
        let stored_ids: Vec<i64> =
            sqlx::query_scalar("SELECT channel_id FROM channels ORDER BY channel_id")
                .fetch_all(&self.pool)
                .await
                .map_err(|error| map_database_error(error, "enumerate channel desired state"))?;
        let mut ids = std::collections::BTreeSet::new();
        for raw_id in stored_ids {
            let raw_id = u32::try_from(raw_id)
                .map_err(|_| invalid("stored channel identity is outside the u32 range"))?;
            let channel_id = ChannelId::new(raw_id);
            validate_channel_id(channel_id)?;
            ids.insert(channel_id);
        }
        for channel_id in self.runtime.channel_ids() {
            validate_channel_id(channel_id)?;
            ids.insert(channel_id);
        }
        Ok(ids.into_iter().collect())
    }

    async fn reconcile_channel_locked(
        &self,
        channel_id: ChannelId,
    ) -> PortResult<ChannelReconciliationItem> {
        match load_channel(&self.pool, channel_id).await {
            Ok(stored) => Ok(self.reconcile_present(stored).await),
            Err(error) if error.kind() == PortErrorKind::NotFound => {
                let last_revision = optional_tombstone_revision(&self.pool, channel_id).await?;
                let projection = match self.runtime.fence(channel_id).await {
                    Ok(()) => ChannelRuntimeProjection::Removed,
                    Err(_) => ChannelRuntimeProjection::Degraded,
                };
                Ok(ChannelReconciliationItem::new(
                    channel_id,
                    ChannelDesiredStateObservation::absent(last_revision),
                    projection,
                ))
            },
            Err(error) => Err(error),
        }
    }

    async fn reconcile_present(&self, initial: StoredChannel) -> ChannelReconciliationItem {
        let projection = if !initial.enabled {
            match self.runtime.fence(initial.channel_id).await {
                Ok(()) => ChannelRuntimeProjection::Stopped,
                Err(_) => ChannelRuntimeProjection::Degraded,
            }
        } else {
            self.activate_authoritative(&initial).await
        };

        match load_channel(&self.pool, initial.channel_id).await {
            Ok(latest) if initial.same_desired_state(&latest) => ChannelReconciliationItem::new(
                initial.channel_id,
                ChannelDesiredStateObservation::present(initial.revision, initial.enabled),
                projection,
            ),
            Ok(latest) => {
                let _ = self.runtime.fence(initial.channel_id).await;
                ChannelReconciliationItem::new(
                    initial.channel_id,
                    ChannelDesiredStateObservation::present(latest.revision, latest.enabled),
                    ChannelRuntimeProjection::Degraded,
                )
            },
            Err(error) if error.kind() == PortErrorKind::NotFound => {
                let _ = self.runtime.fence(initial.channel_id).await;
                let tombstone = optional_tombstone_revision(&self.pool, initial.channel_id)
                    .await
                    .ok()
                    .flatten();
                ChannelReconciliationItem::new(
                    initial.channel_id,
                    ChannelDesiredStateObservation::absent(tombstone),
                    ChannelRuntimeProjection::Degraded,
                )
            },
            Err(_) => {
                let _ = self.runtime.fence(initial.channel_id).await;
                ChannelReconciliationItem::new(
                    initial.channel_id,
                    ChannelDesiredStateObservation::present(initial.revision, initial.enabled),
                    ChannelRuntimeProjection::Degraded,
                )
            },
        }
    }

    async fn activate_authoritative(&self, stored: &StoredChannel) -> ChannelRuntimeProjection {
        let config = match stored
            .to_config()
            .and_then(|config| self.runtime.validate(&config).map(|()| config))
        {
            Ok(config) => config,
            Err(_) => {
                let _ = self.runtime.fence(stored.channel_id).await;
                return ChannelRuntimeProjection::Degraded;
            },
        };
        if self.runtime.is_present(stored.channel_id)
            && self.runtime.fence(stored.channel_id).await.is_err()
        {
            return ChannelRuntimeProjection::Degraded;
        }
        match self.runtime.activate(Arc::new(config)).await {
            Ok(()) => ChannelRuntimeProjection::Active,
            Err(_) => {
                let _ = self.runtime.fence(stored.channel_id).await;
                ChannelRuntimeProjection::Degraded
            },
        }
    }

    async fn degraded_item_locked(
        &self,
        channel_id: ChannelId,
    ) -> PortResult<ChannelReconciliationItem> {
        let desired = match load_channel(&self.pool, channel_id).await {
            Ok(stored) => ChannelDesiredStateObservation::present(stored.revision, stored.enabled),
            Err(error) if error.kind() == PortErrorKind::NotFound => {
                ChannelDesiredStateObservation::absent(
                    optional_tombstone_revision(&self.pool, channel_id).await?,
                )
            },
            Err(error) => return Err(error),
        };
        let _ = self.runtime.fence(channel_id).await;
        Ok(ChannelReconciliationItem::new(
            channel_id,
            desired,
            ChannelRuntimeProjection::Degraded,
        ))
    }
}

#[async_trait]
impl ChannelMutator for SqliteChannelMutator {
    async fn mutate(&self, mutation: ChannelMutation) -> PortResult<ChannelMutationReceipt> {
        let _projection_guard = self.projection_gate.read().await;
        match mutation {
            ChannelMutation::Create { definition } => self.create(definition).await,
            ChannelMutation::Update {
                channel_id,
                expected_revision,
                patch,
            } => self.update(channel_id, expected_revision, patch).await,
            ChannelMutation::Delete {
                channel_id,
                expected_revision,
            } => self.delete(channel_id, expected_revision).await,
            ChannelMutation::SetEnabled {
                channel_id,
                expected_revision,
                enabled,
            } => {
                self.set_enabled(channel_id, expected_revision, enabled)
                    .await
            },
        }
    }
}

#[async_trait]
impl ChannelReconciler for SqliteChannelMutator {
    async fn reconcile(
        &self,
        scope: ChannelReconciliationScope,
    ) -> PortResult<ChannelReconciliationReceipt> {
        let _projection_guard = self.projection_gate.write().await;
        let channel_ids = match scope {
            ChannelReconciliationScope::All => self.reconciliation_ids().await?,
            ChannelReconciliationScope::One(channel_id) => {
                validate_channel_id(channel_id)?;
                vec![channel_id]
            },
        };
        let topology_current = match &self.topology {
            Some(topology) => topology.project().await?.is_current(),
            None => true,
        };
        let mut items = Vec::with_capacity(channel_ids.len());
        for channel_id in channel_ids {
            let lock = self.channel_lock(channel_id);
            let _channel_guard = lock.lock().await;
            let item = if topology_current {
                self.reconcile_channel_locked(channel_id).await?
            } else {
                self.degraded_item_locked(channel_id).await?
            };
            items.push(item);
        }
        Ok(ChannelReconciliationReceipt::new(scope, items))
    }
}

#[derive(Clone)]
struct StoredChannel {
    channel_id: ChannelId,
    name: String,
    protocol: String,
    enabled: bool,
    config_json: Option<String>,
    revision: ChannelRevision,
}

type StoredChannelRow = (i64, String, Option<String>, bool, Option<String>, i64);

impl StoredChannel {
    fn from_committed_config(
        config: &ChannelConfig,
        config_json: String,
        revision: ChannelRevision,
    ) -> Self {
        Self {
            channel_id: ChannelId::new(config.id()),
            name: config.core.name.clone(),
            protocol: config.core.protocol.clone(),
            enabled: config.core.enabled,
            config_json: Some(config_json),
            revision,
        }
    }

    fn to_config(&self) -> PortResult<ChannelConfig> {
        let (description, parameters, logging) = decode_config(self.config_json.as_deref())?;
        Ok(ChannelConfig {
            core: ChannelCore {
                id: self.channel_id.get(),
                name: self.name.clone(),
                description,
                protocol: self.protocol.clone(),
                enabled: self.enabled,
            },
            parameters,
            logging,
        })
    }

    fn same_desired_state(&self, other: &Self) -> bool {
        self.channel_id == other.channel_id
            && self.name == other.name
            && self.protocol == other.protocol
            && self.enabled == other.enabled
            && self.config_json == other.config_json
            && self.revision == other.revision
    }
}

async fn load_channel(pool: &SqlitePool, channel_id: ChannelId) -> PortResult<StoredChannel> {
    let row: Option<StoredChannelRow> = sqlx::query_as(
        "SELECT channel_id, name, protocol, enabled, config, revision \
         FROM channels WHERE channel_id = ?",
    )
    .bind(i64::from(channel_id.get()))
    .fetch_optional(pool)
    .await
    .map_err(|error| map_database_error(error, "load channel desired state"))?;
    let Some((raw_id, name, protocol, enabled, config_json, revision)) = row else {
        return Err(PortError::new(
            PortErrorKind::NotFound,
            "channel desired state does not exist",
        ));
    };
    let raw_id = u32::try_from(raw_id)
        .map_err(|_| invalid("stored channel identity is outside the u32 range"))?;
    let protocol = protocol.ok_or_else(|| invalid("stored channel protocol is missing"))?;
    Ok(StoredChannel {
        channel_id: ChannelId::new(raw_id),
        name,
        protocol,
        enabled,
        config_json,
        revision: stored_revision(revision)?,
    })
}

async fn load_tombstone_revision(
    pool: &SqlitePool,
    channel_id: ChannelId,
) -> PortResult<ChannelRevision> {
    let revision: Option<i64> = sqlx::query_scalar(
        "SELECT last_revision FROM channel_revision_tombstones WHERE channel_id = ?",
    )
    .bind(i64::from(channel_id.get()))
    .fetch_optional(pool)
    .await
    .map_err(|error| map_database_error(error, "load channel revision tombstone"))?;
    revision
        .map(stored_revision)
        .transpose()?
        .ok_or_else(|| PortError::new(PortErrorKind::NotFound, "channel tombstone does not exist"))
}

async fn optional_tombstone_revision(
    pool: &SqlitePool,
    channel_id: ChannelId,
) -> PortResult<Option<ChannelRevision>> {
    match load_tombstone_revision(pool, channel_id).await {
        Ok(revision) => Ok(Some(revision)),
        Err(error) if error.kind() == PortErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn definition_to_config(
    channel_id: ChannelId,
    definition: &ChannelDefinition,
) -> PortResult<ChannelConfig> {
    let name = required_text("channel name", definition.name())?;
    let protocol = required_text("channel protocol", definition.protocol())?;
    Ok(ChannelConfig {
        core: ChannelCore {
            id: channel_id.get(),
            name,
            description: definition.description().map(ToOwned::to_owned),
            protocol: crate::utils::normalize_protocol_name(&protocol).into_owned(),
            enabled: definition.enabled(),
        },
        parameters: parameters_to_json(definition.parameters())?,
        logging: logging_to_config(definition.logging()),
    })
}

fn apply_patch(mut config: ChannelConfig, patch: &ChannelPatch) -> PortResult<ChannelConfig> {
    if let Some(name) = patch.name() {
        config.core.name = required_text("channel name", name)?;
    }
    if let Some(description) = patch.description() {
        config.core.description = Some(description.to_owned());
    }
    if let Some(protocol) = patch.protocol() {
        let protocol = required_text("channel protocol", protocol)?;
        config.core.protocol = crate::utils::normalize_protocol_name(&protocol).into_owned();
    }
    if let Some(parameters) = patch.parameters() {
        for (key, value) in parameters_to_json(parameters)? {
            config.parameters.insert(key, value);
        }
    }
    if let Some(logging) = patch.logging() {
        config.logging = logging_to_config(logging);
    }
    Ok(config)
}

fn logging_to_config(logging: &ChannelLoggingPolicy) -> ChannelLoggingConfig {
    ChannelLoggingConfig {
        enabled: logging.enabled(),
        level: logging.level().map(ToOwned::to_owned),
        file: logging.file().map(ToOwned::to_owned),
    }
}

fn parameters_to_json(
    parameters: &ChannelParameters,
) -> PortResult<HashMap<String, serde_json::Value>> {
    parameters
        .iter()
        .map(|(key, value)| Ok((key.clone(), parameter_to_json(value)?)))
        .collect()
}

fn parameter_to_json(value: &ChannelParameterValue) -> PortResult<serde_json::Value> {
    match value {
        ChannelParameterValue::Null => Ok(serde_json::Value::Null),
        ChannelParameterValue::Bool(value) => Ok(serde_json::Value::Bool(*value)),
        ChannelParameterValue::Integer(value) => Ok((*value).into()),
        ChannelParameterValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| invalid("channel parameters contain a non-finite float")),
        ChannelParameterValue::String(value) => Ok(serde_json::Value::String(value.clone())),
        ChannelParameterValue::Array(values) => values
            .iter()
            .map(parameter_to_json)
            .collect::<PortResult<Vec<_>>>()
            .map(serde_json::Value::Array),
        ChannelParameterValue::Object(values) => {
            let mut object = serde_json::Map::new();
            for (key, value) in values {
                object.insert(key.clone(), parameter_to_json(value)?);
            }
            Ok(serde_json::Value::Object(object))
        },
    }
}

fn json_to_parameter(value: &serde_json::Value) -> PortResult<ChannelParameterValue> {
    match value {
        serde_json::Value::Null => Ok(ChannelParameterValue::Null),
        serde_json::Value::Bool(value) => Ok(ChannelParameterValue::Bool(*value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(ChannelParameterValue::Integer(value))
            } else if let Some(value) = value.as_u64() {
                let value = i64::try_from(value).map_err(|_| {
                    invalid("stored channel parameter integer exceeds the supported range")
                })?;
                Ok(ChannelParameterValue::Integer(value))
            } else {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(ChannelParameterValue::Float)
                    .ok_or_else(|| invalid("stored channel parameter number is invalid"))
            }
        },
        serde_json::Value::String(value) => Ok(ChannelParameterValue::String(value.clone())),
        serde_json::Value::Array(values) => values
            .iter()
            .map(json_to_parameter)
            .collect::<PortResult<Vec<_>>>()
            .map(ChannelParameterValue::Array),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), json_to_parameter(value)?)))
            .collect::<PortResult<BTreeMap<_, _>>>()
            .map(ChannelParameterValue::Object),
    }
}

fn encode_config(config: &ChannelConfig) -> PortResult<String> {
    let mut root = serde_json::Map::new();
    if let Some(description) = &config.core.description {
        root.insert(
            "description".to_owned(),
            serde_json::Value::String(description.clone()),
        );
    }
    let mut parameters = serde_json::Map::new();
    let mut keys = config.parameters.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    for key in keys {
        if let Some(value) = config.parameters.get(key) {
            parameters.insert(key.clone(), value.clone());
        }
    }
    root.insert(
        "parameters".to_owned(),
        serde_json::Value::Object(parameters),
    );
    root.insert(
        "logging".to_owned(),
        serde_json::to_value(&config.logging)
            .map_err(|_| invalid("channel logging policy cannot be persisted"))?,
    );
    serde_json::to_string(&serde_json::Value::Object(root))
        .map_err(|_| invalid("channel configuration cannot be persisted"))
}

#[allow(clippy::type_complexity)]
fn decode_config(
    config_json: Option<&str>,
) -> PortResult<(
    Option<String>,
    HashMap<String, serde_json::Value>,
    ChannelLoggingConfig,
)> {
    let value = match config_json {
        Some(config_json) => serde_json::from_str::<serde_json::Value>(config_json)
            .map_err(|_| invalid("stored channel configuration is not valid JSON"))?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid("stored channel configuration is not an object"))?;
    let description = match object.get("description") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(invalid("stored channel description is not a string")),
    };
    let parameters = match object.get("parameters") {
        None => HashMap::new(),
        Some(serde_json::Value::Object(values)) => {
            let mut parameters = HashMap::with_capacity(values.len());
            for (key, value) in values {
                // Enforce the same numeric domain for persisted and incoming
                // typed values without exposing any value in diagnostics.
                let value = parameter_to_json(&json_to_parameter(value)?)?;
                parameters.insert(key.clone(), value);
            }
            parameters
        },
        Some(_) => return Err(invalid("stored channel parameters are not an object")),
    };
    let logging = match object.get("logging") {
        None => ChannelLoggingConfig::default(),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| invalid("stored channel logging policy is invalid"))?,
    };
    Ok((description, parameters, logging))
}

fn validate_runtime_config(config: &ChannelConfig) -> PortResult<()> {
    if config.id() >= MAX_CHANNEL_ID {
        return Err(invalid("channel identity must be between 0 and 9999"));
    }
    let protocol = crate::utils::normalize_protocol_name(config.protocol());
    let supported = match protocol.as_ref() {
        "virtual" => true,
        #[cfg(feature = "modbus")]
        "modbus_tcp" | "modbus_rtu" | "sunspec_tcp" | "sunspec_rtu" => true,
        #[cfg(all(target_os = "linux", feature = "gpio"))]
        "gpio" | "di_do" | "dido" => true,
        #[cfg(all(target_os = "linux", feature = "can"))]
        "can" => true,
        #[cfg(feature = "aether_485")]
        "aether_485" => true,
        #[cfg(feature = "iec104")]
        "iec104" => true,
        #[cfg(feature = "opcua")]
        "opcua" => true,
        #[cfg(feature = "dl645")]
        "dl645" => true,
        #[cfg(feature = "iec61850")]
        "iec61850" => true,
        _ => false,
    };
    if !supported {
        return Err(invalid(
            "channel protocol is unavailable in this IO runtime build",
        ));
    }

    let mut validation = common::ValidationResult::new(common::ValidationLevel::Schema);
    config.validate(&mut validation, 0);
    if !validation.is_valid {
        return Err(invalid(
            "channel parameters do not satisfy the protocol schema",
        ));
    }
    Ok(())
}

async fn action_route_count(pool: &SqlitePool, channel_id: ChannelId) -> PortResult<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM action_routing WHERE channel_id = ?")
        .bind(i64::from(channel_id.get()))
        .fetch_one(pool)
        .await
        .map_err(|error| map_database_error(error, "check governed action routes"))
}

async fn delete_desired_state(
    transaction: &mut Transaction<'_, Sqlite>,
    channel_id: ChannelId,
    expected_revision: ChannelRevision,
    tombstone_revision: ChannelRevision,
) -> PortResult<()> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT revision FROM channels WHERE channel_id = ?")
        .bind(i64::from(channel_id.get()))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| map_database_error(error, "reload channel before deletion"))?;
    let Some((revision,)) = row else {
        return Err(PortError::new(
            PortErrorKind::NotFound,
            "channel desired state does not exist",
        ));
    };
    if stored_revision(revision)? != expected_revision {
        return Err(conflict("channel desired state changed concurrently"));
    }

    let action_routes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM action_routing WHERE channel_id = ?")
            .bind(i64::from(channel_id.get()))
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| map_database_error(error, "recheck governed action routes"))?;
    if action_routes != 0 {
        return Err(conflict(
            "remove governed action routes before deleting their channel",
        ));
    }

    sqlx::query(
        "INSERT INTO channel_revision_tombstones \
             (channel_id, last_revision, deleted_at) \
         VALUES (?, ?, CURRENT_TIMESTAMP) \
         ON CONFLICT(channel_id) DO UPDATE SET \
             last_revision = MAX(last_revision, excluded.last_revision), \
             deleted_at = excluded.deleted_at",
    )
    .bind(i64::from(channel_id.get()))
    .bind(revision_i64(tombstone_revision)?)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_database_error(error, "persist channel revision tombstone"))?;

    // These rows are owned by channel measurement/configuration state. Every
    // error aborts the transaction; none are treated as an optional table.
    for table in [
        "measurement_routing",
        "json_point_mappings",
        "telemetry_points",
        "signal_points",
        "control_points",
        "adjustment_points",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE channel_id = ?"))
            .bind(i64::from(channel_id.get()))
            .execute(&mut **transaction)
            .await
            .map_err(|error| map_database_error(error, "delete channel-owned configuration"))?;
    }

    // Automation creates this legacy measurement projection lazily. It is
    // optional at the schema level, but when present its rows are channel
    // owned and every deletion error must still abort this transaction.
    if table_exists(transaction, "point_mappings").await? {
        sqlx::query("DELETE FROM point_mappings WHERE channel_id = ?")
            .bind(i64::from(channel_id.get()))
            .execute(&mut **transaction)
            .await
            .map_err(|error| map_database_error(error, "delete channel-owned configuration"))?;
    }

    // Direct channel-to-channel measurement forwarding is also optional and
    // can reference the channel on either side of the route.
    if table_exists(transaction, "channel_routing").await? {
        sqlx::query(
            "DELETE FROM channel_routing \
             WHERE source_channel_id = ? OR target_channel_id = ?",
        )
        .bind(i64::from(channel_id.get()))
        .bind(i64::from(channel_id.get()))
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_database_error(error, "delete channel-owned configuration"))?;
    }

    let result = sqlx::query("DELETE FROM channels WHERE channel_id = ? AND revision = ?")
        .bind(i64::from(channel_id.get()))
        .bind(revision_i64(expected_revision)?)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_database_error(error, "delete channel desired state"))?;
    if result.rows_affected() != 1 {
        return Err(conflict("channel desired state changed concurrently"));
    }
    Ok(())
}

async fn table_exists(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &'static str,
) -> PortResult<bool> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(\
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?\
         )",
    )
    .bind(table)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| map_database_error(error, "inspect channel-owned configuration"))?;
    Ok(exists != 0)
}

async fn restore_authoritative_runtime(
    pool: &SqlitePool,
    runtime: &dyn ChannelRuntimeLifecycle,
    initial: &StoredChannel,
) {
    // This is deliberately best effort: the original command failure remains
    // the result. Never use the pre-fence snapshot after a CAS/storage failure,
    // because a staged writer may have disabled, changed, deleted, or replaced
    // the entity while the runtime was being fenced.
    if !same_channel_entity(pool, initial).await {
        let _ = runtime.fence(initial.channel_id).await;
        return;
    }
    let Ok(latest) = load_channel(pool, initial.channel_id).await else {
        let _ = runtime.fence(initial.channel_id).await;
        return;
    };
    if !latest.enabled || !same_channel_entity(pool, initial).await {
        let _ = runtime.fence(initial.channel_id).await;
        return;
    }
    let Ok(config) = latest.to_config() else {
        let _ = runtime.fence(initial.channel_id).await;
        return;
    };
    if runtime.validate(&config).is_err() {
        let _ = runtime.fence(initial.channel_id).await;
        return;
    }
    if runtime.is_present(initial.channel_id) && runtime.fence(initial.channel_id).await.is_err() {
        return;
    }
    if runtime.activate(Arc::new(config)).await.is_err() {
        let _ = runtime.fence(initial.channel_id).await;
        return;
    }

    let stable = same_channel_entity(pool, initial).await
        && load_channel(pool, initial.channel_id)
            .await
            .is_ok_and(|current| latest.same_desired_state(&current));
    if !stable {
        let _ = runtime.fence(initial.channel_id).await;
    }
}

async fn same_channel_entity(pool: &SqlitePool, initial: &StoredChannel) -> bool {
    match load_tombstone_revision(pool, initial.channel_id).await {
        Ok(tombstone) => tombstone <= initial.revision,
        Err(error) if error.kind() == PortErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn verify_expected_revision(
    expected: Option<ChannelRevision>,
    actual: ChannelRevision,
) -> PortResult<()> {
    if let Some(expected) = expected {
        revision_i64(expected)?;
        if expected != actual {
            return Err(conflict("channel revision is stale"));
        }
    }
    Ok(())
}

fn next_revision(revision: ChannelRevision) -> PortResult<ChannelRevision> {
    if revision.get() >= MAX_SQLITE_REVISION {
        return Err(PortError::new(
            PortErrorKind::Permanent,
            "channel revision is exhausted",
        ));
    }
    revision
        .checked_next()
        .ok_or_else(|| PortError::new(PortErrorKind::Permanent, "channel revision is exhausted"))
}

fn stored_revision(revision: i64) -> PortResult<ChannelRevision> {
    let revision = u64::try_from(revision)
        .ok()
        .filter(|revision| *revision >= 1)
        .ok_or_else(|| invalid("stored channel revision is invalid"))?;
    Ok(ChannelRevision::new(revision))
}

fn revision_i64(revision: ChannelRevision) -> PortResult<i64> {
    i64::try_from(revision.get())
        .ok()
        .filter(|revision| *revision >= 1)
        .ok_or_else(|| invalid("channel revision is outside the SQLite range"))
}

fn validate_channel_id(channel_id: ChannelId) -> PortResult<()> {
    if channel_id.get() >= MAX_CHANNEL_ID {
        Err(invalid("channel identity must be less than 10000"))
    } else {
        Ok(())
    }
}

fn required_text(field: &str, value: &str) -> PortResult<String> {
    let value = value.trim();
    if value.is_empty() {
        Err(invalid(format!("{field} must not be blank")))
    } else {
        Ok(value.to_owned())
    }
}

fn map_database_error(error: sqlx::Error, operation: &'static str) -> PortError {
    if let sqlx::Error::Database(database) = &error {
        if database
            .message()
            .contains("governed action-routing command")
        {
            return conflict(format!(
                "{operation} conflicts with a governed action route"
            ));
        }
        if database.is_unique_violation() || database.is_foreign_key_violation() {
            return conflict(format!("{operation} conflicts with existing configuration"));
        }
        if database.is_check_violation() {
            return PortError::new(
                PortErrorKind::Permanent,
                format!("{operation} was rejected by a storage invariant"),
            );
        }
        if matches!(database.code().as_deref(), Some("5" | "6" | "261" | "262")) {
            return unavailable(format!("{operation} is temporarily unavailable"));
        }
        // Trigger-raised constraints are deterministic; an unchanged retry is
        // unsafe and cannot repair the invariant.
        if matches!(
            database.code().as_deref(),
            Some("19" | "275" | "787" | "1299" | "1555" | "1811" | "2067")
        ) {
            return PortError::new(
                PortErrorKind::Permanent,
                format!("{operation} was rejected by a storage invariant"),
            );
        }
        return PortError::new(
            PortErrorKind::Permanent,
            format!("{operation} failed because the storage schema is incompatible"),
        );
    }
    unavailable(format!("{operation} failed because storage is unavailable"))
}

fn invalid(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn conflict(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Conflict, message)
}

fn unavailable(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_config(
        id: u32,
        protocol: &str,
        parameters: HashMap<String, serde_json::Value>,
    ) -> ChannelConfig {
        ChannelConfig {
            core: ChannelCore {
                id,
                name: "production-validator".to_owned(),
                description: None,
                protocol: protocol.to_owned(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        }
    }

    #[test]
    fn production_validator_keeps_explicit_zero_channel_identity_compatible() {
        assert!(validate_runtime_config(&runtime_config(0, "virtual", HashMap::new())).is_ok());
    }

    #[test]
    fn production_validator_rejects_zero_poll_before_runtime_creation() {
        let error = validate_runtime_config(&runtime_config(
            1,
            "virtual",
            HashMap::from([("poll_interval_ms".to_owned(), serde_json::json!(0))]),
        ))
        .expect_err("zero poll interval must be rejected");
        assert_eq!(error.kind(), PortErrorKind::InvalidData);
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn production_validator_rejects_modbus_endpoint_fallback_and_overflow() {
        for config in [
            runtime_config(
                1,
                "modbus_tcp",
                HashMap::from([
                    ("host".to_owned(), serde_json::json!(123)),
                    ("port".to_owned(), serde_json::json!(502)),
                ]),
            ),
            runtime_config(
                2,
                "modbus_tcp",
                HashMap::from([
                    ("host".to_owned(), serde_json::json!("edge")),
                    ("port".to_owned(), serde_json::json!(65_536)),
                ]),
            ),
            runtime_config(
                3,
                "modbus_rtu",
                HashMap::from([
                    ("device".to_owned(), serde_json::json!("/dev/ttyUSB0")),
                    ("baud_rate".to_owned(), serde_json::json!(4_294_967_296_u64)),
                ]),
            ),
        ] {
            assert!(validate_runtime_config(&config).is_err());
        }
    }
}
