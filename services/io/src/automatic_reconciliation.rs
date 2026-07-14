//! Automatic projection of SQLite desired state into IO runtime state.
//!
//! This worker is deliberately below the command/audit application boundary:
//! it repairs already-authoritative desired state and never fabricates a user
//! command or audit record.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use aether_domain::ChannelId;
use aether_ports::{
    ChannelReconciler, ChannelReconciliationScope, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::SqliteChannelMutator;
use crate::store::SqliteShmTopologyProjector;

const MIN_RECONCILIATION_INTERVAL: Duration = Duration::from_millis(100);
const REQUIRED_POINT_TABLES: [&str; 4] = [
    "telemetry_points",
    "signal_points",
    "control_points",
    "adjustment_points",
];

/// Narrow SHM projection boundary used by the automatic worker and its tests.
#[async_trait]
pub trait ShmTopologyProjection: Send + Sync + 'static {
    /// Projects SQLite topology and reports whether both SHM planes are current.
    async fn project_current(&self) -> PortResult<bool>;
}

#[async_trait]
impl ShmTopologyProjection for SqliteShmTopologyProjector {
    async fn project_current(&self) -> PortResult<bool> {
        Ok(self.project().await?.is_current())
    }
}

/// Runtime fencing boundary owned by the same lifecycle gate as reconciliation.
#[async_trait]
pub trait AutomaticRuntimeBoundary: Send + Sync + 'static {
    /// Returns every currently installed channel runtime identity.
    fn runtime_channel_ids(&self) -> Vec<ChannelId>;

    /// Removes command and acquisition capability for untrusted projections.
    async fn fence_untrusted(&self, channel_ids: &[ChannelId]) -> PortResult<()>;
}

#[async_trait]
impl AutomaticRuntimeBoundary for SqliteChannelMutator {
    fn runtime_channel_ids(&self) -> Vec<ChannelId> {
        self.runtime_channel_ids_for_reconciliation()
    }

    async fn fence_untrusted(&self, channel_ids: &[ChannelId]) -> PortResult<()> {
        self.fence_untrusted_channels(channel_ids).await
    }
}

/// Sanitized result of one automatic reconciliation cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomaticIoReconciliationReceipt {
    topology_current: bool,
    authority_stable: bool,
    attempted_channels: usize,
    converged: bool,
}

impl AutomaticIoReconciliationReceipt {
    /// Returns whether point and health SHM matched SQLite in this cycle.
    #[must_use]
    pub const fn topology_current(self) -> bool {
        self.topology_current
    }

    /// Returns whether SQLite stayed unchanged across runtime activation.
    #[must_use]
    pub const fn authority_stable(self) -> bool {
        self.authority_stable
    }

    /// Returns the number of channel identities selected for repair.
    #[must_use]
    pub const fn attempted_channels(self) -> usize {
        self.attempted_channels
    }

    /// Returns whether every desired/applied projection converged.
    #[must_use]
    pub const fn converged(self) -> bool {
        self.converged
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DesiredRuntimeFingerprint {
    enabled: bool,
    fingerprint: u64,
}

type DesiredRuntimeSnapshot = BTreeMap<ChannelId, DesiredRuntimeFingerprint>;

/// Periodic desired/applied reconciler for channel runtime and SHM state.
pub struct AutomaticIoReconciler {
    pool: SqlitePool,
    channels: Arc<dyn ChannelReconciler>,
    topology: Arc<dyn ShmTopologyProjection>,
    runtime: Arc<dyn AutomaticRuntimeBoundary>,
    applied: Mutex<Option<DesiredRuntimeSnapshot>>,
    cycle_gate: Mutex<()>,
}

impl AutomaticIoReconciler {
    /// Creates a reconciler. The first cycle always performs a full lifecycle pass.
    #[must_use]
    pub fn new<C, T, R>(
        pool: SqlitePool,
        channels: Arc<C>,
        topology: Arc<T>,
        runtime: Arc<R>,
    ) -> Self
    where
        C: ChannelReconciler,
        T: ShmTopologyProjection,
        R: AutomaticRuntimeBoundary,
    {
        Self {
            pool,
            channels,
            topology,
            runtime,
            applied: Mutex::new(None),
            cycle_gate: Mutex::new(()),
        }
    }

    /// Runs one serialized reconciliation cycle.
    pub async fn reconcile_once(&self) -> PortResult<AutomaticIoReconciliationReceipt> {
        let _cycle_guard = self.cycle_gate.lock().await;
        let desired = match load_desired_runtime_snapshot(&self.pool).await {
            Ok(desired) => desired,
            Err(error) => {
                self.clear_applied().await;
                self.fail_closed(None).await?;
                return Err(error);
            },
        };

        let topology_current = match self.topology.project_current().await {
            Ok(current) => current,
            Err(error) => {
                self.clear_applied().await;
                self.fail_closed(Some(&desired)).await?;
                return Err(error);
            },
        };
        if !topology_current {
            let attempted_channels = self.fail_closed(Some(&desired)).await?;
            self.clear_applied().await;
            return Ok(AutomaticIoReconciliationReceipt {
                topology_current: false,
                authority_stable: true,
                attempted_channels,
                converged: false,
            });
        }

        let runtime_ids: BTreeSet<_> = self.runtime.runtime_channel_ids().into_iter().collect();
        let applied = self.applied.lock().await.clone();
        let force_all = applied.is_none();
        let targets = reconciliation_targets(applied.as_ref(), &desired, &runtime_ids);
        let attempted_channels = targets.len();

        let mut converged = true;
        if force_all {
            let receipt = match self
                .channels
                .reconcile(ChannelReconciliationScope::All)
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => {
                    self.clear_applied().await;
                    self.fail_closed(Some(&desired)).await?;
                    return Err(error);
                },
            };
            converged = !receipt.reconciliation_required();
            if !converged {
                self.fail_closed(Some(&desired)).await?;
            }
        } else {
            for channel_id in &targets {
                let receipt = match self
                    .channels
                    .reconcile(ChannelReconciliationScope::One(*channel_id))
                    .await
                {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        self.clear_applied().await;
                        self.runtime.fence_untrusted(&[*channel_id]).await?;
                        return Err(error);
                    },
                };
                if receipt.reconciliation_required() {
                    self.runtime.fence_untrusted(&[*channel_id]).await?;
                    converged = false;
                }
            }
        }

        let latest = match load_desired_runtime_snapshot(&self.pool).await {
            Ok(latest) => latest,
            Err(error) => {
                self.clear_applied().await;
                self.fail_closed(Some(&desired)).await?;
                return Err(error);
            },
        };
        if latest != desired {
            let unstable = changed_channel_ids(&desired, &latest);
            self.runtime.fence_untrusted(&unstable).await?;
            self.clear_applied().await;
            return Ok(AutomaticIoReconciliationReceipt {
                topology_current: true,
                authority_stable: false,
                attempted_channels,
                converged: false,
            });
        }

        if converged {
            *self.applied.lock().await = Some(latest);
        } else {
            self.clear_applied().await;
        }
        Ok(AutomaticIoReconciliationReceipt {
            topology_current: true,
            authority_stable: true,
            attempted_channels,
            converged,
        })
    }

    async fn clear_applied(&self) {
        *self.applied.lock().await = None;
    }

    async fn fail_closed(&self, desired: Option<&DesiredRuntimeSnapshot>) -> PortResult<usize> {
        let mut channel_ids: BTreeSet<_> = self.runtime.runtime_channel_ids().into_iter().collect();
        if let Some(desired) = desired {
            channel_ids.extend(desired.keys().copied());
        }
        let channel_ids: Vec<_> = channel_ids.into_iter().collect();
        self.runtime.fence_untrusted(&channel_ids).await?;
        Ok(channel_ids.len())
    }
}

/// Runs startup reconciliation immediately, then repeats until cancellation.
pub async fn run_automatic_io_reconciliation(
    reconciler: Arc<AutomaticIoReconciler>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let interval = interval.max(MIN_RECONCILIATION_INTERVAL);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                match reconciler.reconcile_once().await {
                    Ok(receipt) if receipt.converged() => {
                        tracing::debug!(
                            attempted_channels = receipt.attempted_channels(),
                            "automatic IO desired/applied reconciliation converged"
                        );
                    },
                    Ok(receipt) => {
                        tracing::warn!(
                            topology_current = receipt.topology_current(),
                            authority_stable = receipt.authority_stable(),
                            attempted_channels = receipt.attempted_channels(),
                            "automatic IO desired/applied reconciliation remains degraded"
                        );
                    },
                    Err(error) => {
                        tracing::error!(
                            error_kind = ?error.kind(),
                            "automatic IO desired/applied reconciliation failed closed"
                        );
                    },
                }
            },
        }
    }
}

fn reconciliation_targets(
    applied: Option<&DesiredRuntimeSnapshot>,
    desired: &DesiredRuntimeSnapshot,
    runtime_ids: &BTreeSet<ChannelId>,
) -> BTreeSet<ChannelId> {
    let mut targets = BTreeSet::new();
    match applied {
        Some(applied) => targets.extend(changed_channel_ids(applied, desired)),
        None => targets.extend(desired.keys().copied()),
    }
    for (channel_id, desired) in desired {
        if desired.enabled != runtime_ids.contains(channel_id) {
            targets.insert(*channel_id);
        }
    }
    targets.extend(
        runtime_ids
            .iter()
            .filter(|channel_id| !desired.contains_key(channel_id))
            .copied(),
    );
    targets
}

fn changed_channel_ids(
    before: &DesiredRuntimeSnapshot,
    after: &DesiredRuntimeSnapshot,
) -> Vec<ChannelId> {
    before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|channel_id| before.get(channel_id) != after.get(channel_id))
        .collect()
}

async fn load_desired_runtime_snapshot(pool: &SqlitePool) -> PortResult<DesiredRuntimeSnapshot> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| authority_unavailable("begin desired runtime snapshot", error))?;
    let rows = sqlx::query(
        "SELECT channel_id, name, protocol, enabled, config, revision \
         FROM channels ORDER BY channel_id",
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| authority_unavailable("load channel desired state", error))?;
    let mut desired = BTreeMap::new();
    for row in rows {
        let raw_id: i64 = row
            .try_get("channel_id")
            .map_err(|error| authority_invalid("decode desired channel identity", error))?;
        let channel_id = channel_id(raw_id)?;
        let mut fingerprint = Fingerprint::new();
        fingerprint.field("channel");
        fingerprint.field(&row_string(&row, "name")?);
        fingerprint.field(&row_string(&row, "protocol")?);
        fingerprint.field(&row_string(&row, "config")?);
        fingerprint.field(&row_i64(&row, "revision")?.to_string());
        let enabled: bool = row
            .try_get("enabled")
            .map_err(|error| authority_invalid("decode desired channel state", error))?;
        desired.insert(
            channel_id,
            DesiredRuntimeFingerprint {
                enabled,
                fingerprint: fingerprint.finish(),
            },
        );
    }

    for table in REQUIRED_POINT_TABLES {
        append_table_fingerprints(&mut transaction, &mut desired, table, true).await?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| authority_unavailable("complete desired runtime snapshot", error))?;
    Ok(desired)
}

async fn append_table_fingerprints(
    transaction: &mut Transaction<'_, Sqlite>,
    desired: &mut DesiredRuntimeSnapshot,
    table: &str,
    required: bool,
) -> PortResult<()> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?)",
    )
    .bind(table)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| authority_unavailable("inspect runtime mapping schema", error))?;
    if !exists {
        if required {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("authoritative runtime table {table} is missing"),
            ));
        }
        return Ok(());
    }

    let pragma = format!("PRAGMA table_info({})", quote_identifier(table));
    let schema = sqlx::query(&pragma)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| authority_unavailable("inspect runtime mapping columns", error))?;
    let mut columns = Vec::new();
    for row in schema {
        let name: String = row
            .try_get("name")
            .map_err(|error| authority_invalid("decode runtime mapping column", error))?;
        if !matches!(name.as_str(), "id" | "created_at" | "updated_at") {
            columns.push(name);
        }
    }
    if !columns.iter().any(|column| column == "channel_id") {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("authoritative runtime table {table} has no channel_id"),
        ));
    }

    let selected = columns
        .iter()
        .map(|column| format!("quote({})", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT channel_id, {selected} FROM {}",
        quote_identifier(table)
    );
    let rows = sqlx::query(&query)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| authority_unavailable("load runtime mapping rows", error))?;
    let mut table_rows: BTreeMap<ChannelId, Vec<Vec<u8>>> = BTreeMap::new();
    for row in rows {
        let raw_id: i64 = row
            .try_get(0)
            .map_err(|error| authority_invalid("decode runtime mapping channel", error))?;
        let channel_id = channel_id(raw_id)?;
        if !desired.contains_key(&channel_id) {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("{table} references absent channel {}", channel_id.get()),
            ));
        }
        let mut encoded = Vec::new();
        for index in 1..row.columns().len() {
            let value: String = row
                .try_get(index)
                .map_err(|error| authority_invalid("decode runtime mapping value", error))?;
            encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
            encoded.extend_from_slice(value.as_bytes());
        }
        table_rows.entry(channel_id).or_default().push(encoded);
    }
    for (channel_id, mut rows) in table_rows {
        rows.sort_unstable();
        let current = desired.get_mut(&channel_id).ok_or_else(|| {
            PortError::new(PortErrorKind::InvalidData, "mapping channel vanished")
        })?;
        let mut fingerprint = Fingerprint(current.fingerprint);
        fingerprint.field(table);
        for row in rows {
            fingerprint.bytes(&row);
        }
        current.fingerprint = fingerprint.finish();
    }
    Ok(())
}

fn row_string(row: &sqlx::sqlite::SqliteRow, column: &str) -> PortResult<String> {
    row.try_get::<Option<String>, _>(column)
        .map(|value| value.unwrap_or_default())
        .map_err(|error| authority_invalid("decode channel desired configuration", error))
}

fn row_i64(row: &sqlx::sqlite::SqliteRow, column: &str) -> PortResult<i64> {
    row.try_get(column)
        .map_err(|error| authority_invalid("decode channel desired revision", error))
}

fn channel_id(raw: i64) -> PortResult<ChannelId> {
    let raw = u32::try_from(raw).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "stored channel identity is outside the u32 range",
        )
    })?;
    if raw >= 10_000 {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "stored channel identity is outside the runtime range",
        ));
    }
    Ok(ChannelId::new(raw))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn authority_unavailable(action: &'static str, error: sqlx::Error) -> PortError {
    PortError::new(PortErrorKind::Unavailable, format!("{action}: {error}"))
}

fn authority_invalid(action: &'static str, error: sqlx::Error) -> PortError {
    PortError::new(PortErrorKind::InvalidData, format!("{action}: {error}"))
}

struct Fingerprint(u64);

impl Fingerprint {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn field(&mut self, value: &str) {
        self.bytes(&(value.len() as u64).to_le_bytes());
        self.bytes(value.as_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        for byte in value {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}
