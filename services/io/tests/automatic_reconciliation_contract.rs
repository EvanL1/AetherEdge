use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aether_domain::ChannelId;
use aether_io::automatic_reconciliation::{
    AutomaticIoReconciler, AutomaticRuntimeBoundary, ShmTopologyProjection,
    run_automatic_io_reconciliation,
};
use aether_ports::{
    ChannelDesiredStateObservation, ChannelReconciler, ChannelReconciliationItem,
    ChannelReconciliationReceipt, ChannelReconciliationScope, ChannelRevision,
    ChannelRuntimeProjection, PortResult,
};
use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use tokio_util::sync::CancellationToken;

struct FakeChannelReconciler {
    scopes: Mutex<Vec<ChannelReconciliationScope>>,
    degraded: Mutex<bool>,
    mutate_mapping_on_next_reconcile: Mutex<Option<sqlx::SqlitePool>>,
}

impl FakeChannelReconciler {
    fn new() -> Self {
        Self {
            scopes: Mutex::new(Vec::new()),
            degraded: Mutex::new(false),
            mutate_mapping_on_next_reconcile: Mutex::new(None),
        }
    }

    fn scopes(&self) -> Vec<ChannelReconciliationScope> {
        self.scopes.lock().expect("scope lock").clone()
    }

    fn set_degraded(&self, degraded: bool) {
        *self.degraded.lock().expect("degraded lock") = degraded;
    }

    fn mutate_mapping_on_next_reconcile(&self, pool: sqlx::SqlitePool) {
        *self
            .mutate_mapping_on_next_reconcile
            .lock()
            .expect("mapping mutation lock") = Some(pool);
    }
}

#[async_trait]
impl ChannelReconciler for FakeChannelReconciler {
    async fn reconcile(
        &self,
        scope: ChannelReconciliationScope,
    ) -> PortResult<ChannelReconciliationReceipt> {
        self.scopes.lock().expect("scope lock").push(scope);
        let mutation_pool = self
            .mutate_mapping_on_next_reconcile
            .lock()
            .expect("mapping mutation lock")
            .take();
        if let Some(pool) = mutation_pool {
            sqlx::query(
                "UPDATE telemetry_points \
                 SET protocol_mappings = '{\"json_path\":\"$.raced\",\"data_type\":\"float\"}' \
                 WHERE channel_id = 7 AND point_id = 99",
            )
            .execute(&pool)
            .await
            .expect("injected concurrent mapping change");
        }
        let ids = match scope {
            ChannelReconciliationScope::All => vec![ChannelId::new(7)],
            ChannelReconciliationScope::One(channel_id) => vec![channel_id],
        };
        Ok(ChannelReconciliationReceipt::new(
            scope,
            ids.into_iter()
                .map(|channel_id| {
                    ChannelReconciliationItem::new(
                        channel_id,
                        ChannelDesiredStateObservation::present(ChannelRevision::new(1), true),
                        if *self.degraded.lock().expect("degraded lock") {
                            ChannelRuntimeProjection::Degraded
                        } else {
                            ChannelRuntimeProjection::Active
                        },
                    )
                })
                .collect(),
        ))
    }
}

struct FakeTopology {
    current: Mutex<bool>,
    calls: Mutex<usize>,
}

impl FakeTopology {
    fn new(current: bool) -> Self {
        Self {
            current: Mutex::new(current),
            calls: Mutex::new(0),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.lock().expect("topology call lock")
    }

    fn set_current(&self, current: bool) {
        *self.current.lock().expect("topology state lock") = current;
    }
}

#[async_trait]
impl ShmTopologyProjection for FakeTopology {
    async fn project_current(&self) -> PortResult<bool> {
        *self.calls.lock().expect("topology call lock") += 1;
        Ok(*self.current.lock().expect("topology state lock"))
    }
}

struct FakeRuntimeBoundary {
    runtime_ids: Mutex<BTreeSet<ChannelId>>,
    fenced: Mutex<Vec<Vec<ChannelId>>>,
}

impl FakeRuntimeBoundary {
    fn with_runtime_ids(ids: impl IntoIterator<Item = ChannelId>) -> Self {
        Self {
            runtime_ids: Mutex::new(ids.into_iter().collect()),
            fenced: Mutex::new(Vec::new()),
        }
    }

    fn fenced(&self) -> Vec<Vec<ChannelId>> {
        self.fenced.lock().expect("fence log lock").clone()
    }

    fn set_runtime_ids(&self, ids: impl IntoIterator<Item = ChannelId>) {
        *self.runtime_ids.lock().expect("runtime ids lock") = ids.into_iter().collect();
    }
}

#[async_trait]
impl AutomaticRuntimeBoundary for FakeRuntimeBoundary {
    fn runtime_channel_ids(&self) -> Vec<ChannelId> {
        self.runtime_ids
            .lock()
            .expect("runtime ids lock")
            .iter()
            .copied()
            .collect()
    }

    async fn fence_untrusted(&self, channel_ids: &[ChannelId]) -> PortResult<()> {
        self.fenced
            .lock()
            .expect("fence log lock")
            .push(channel_ids.to_vec());
        let mut runtime_ids = self.runtime_ids.lock().expect("runtime ids lock");
        for channel_id in channel_ids {
            runtime_ids.remove(channel_id);
        }
        Ok(())
    }
}

async fn test_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory database");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("io schema");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config, revision) \
         VALUES (7, 'mapping-channel', 'mqtt', 1, '{}', 1)",
    )
    .execute(&pool)
    .await
    .expect("desired channel");
    sqlx::query(
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) \
         VALUES (7, 1, 'power'), (7, 2, 'voltage'), (7, 99, 'raced')",
    )
    .execute(&pool)
    .await
    .expect("desired points");
    pool
}

#[tokio::test]
async fn mapping_only_change_reloads_affected_channel_without_churning_unchanged_cycles() {
    let pool = test_pool().await;
    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(true));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([ChannelId::new(7)]));
    let reconciler =
        AutomaticIoReconciler::new(pool.clone(), channels.clone(), topology.clone(), runtime);

    let startup = reconciler.reconcile_once().await.expect("startup cycle");
    assert!(startup.converged());
    assert_eq!(channels.scopes(), vec![ChannelReconciliationScope::All]);

    let unchanged = reconciler.reconcile_once().await.expect("unchanged cycle");
    assert!(unchanged.converged());
    assert_eq!(unchanged.attempted_channels(), 0);
    assert_eq!(channels.scopes(), vec![ChannelReconciliationScope::All]);

    sqlx::query(
        "UPDATE telemetry_points \
         SET protocol_mappings = '{\"json_path\":\"$.power\",\"data_type\":\"float\"}' \
         WHERE channel_id = 7 AND point_id = 1",
    )
    .execute(&pool)
    .await
    .expect("mapping-only change");

    let changed = reconciler.reconcile_once().await.expect("mapping cycle");
    assert!(changed.converged());
    assert_eq!(changed.attempted_channels(), 1);
    assert_eq!(
        channels.scopes(),
        vec![
            ChannelReconciliationScope::All,
            ChannelReconciliationScope::One(ChannelId::new(7)),
        ]
    );
    assert_eq!(topology.calls(), 3, "SHM projection runs every cycle");
}

#[tokio::test]
async fn non_current_shm_fences_all_desired_and_orphan_runtime_channels() {
    let pool = test_pool().await;
    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(false));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([
        ChannelId::new(7),
        ChannelId::new(99),
    ]));
    let reconciler = AutomaticIoReconciler::new(pool, channels.clone(), topology, runtime.clone());

    let receipt = reconciler
        .reconcile_once()
        .await
        .expect("degraded cycle is an observed outcome");

    assert!(!receipt.converged());
    assert!(!receipt.topology_current());
    assert!(channels.scopes().is_empty());
    assert_eq!(
        runtime.fenced(),
        vec![vec![ChannelId::new(7), ChannelId::new(99)]]
    );
}

#[tokio::test]
async fn degraded_mapping_reload_explicitly_fences_the_affected_channel() {
    let pool = test_pool().await;
    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(true));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([ChannelId::new(7)]));
    let reconciler =
        AutomaticIoReconciler::new(pool.clone(), channels.clone(), topology, runtime.clone());
    reconciler.reconcile_once().await.expect("startup cycle");
    channels.set_degraded(true);
    sqlx::query(
        "UPDATE telemetry_points \
         SET protocol_mappings = '{\"json_path\":\"$.voltage\",\"data_type\":\"float\"}' \
         WHERE channel_id = 7 AND point_id = 2",
    )
    .execute(&pool)
    .await
    .expect("mapping-only change");

    let receipt = reconciler.reconcile_once().await.expect("degraded outcome");

    assert!(!receipt.converged());
    assert_eq!(runtime.fenced(), vec![vec![ChannelId::new(7)]]);
}

#[tokio::test]
async fn runtime_inventory_drift_repairs_missing_desired_and_orphan_channels() {
    let pool = test_pool().await;
    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(true));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([ChannelId::new(7)]));
    let reconciler = AutomaticIoReconciler::new(pool, channels.clone(), topology, runtime.clone());
    reconciler.reconcile_once().await.expect("startup cycle");
    runtime.set_runtime_ids([ChannelId::new(99)]);

    let receipt = reconciler.reconcile_once().await.expect("drift cycle");

    assert!(receipt.converged());
    assert_eq!(receipt.attempted_channels(), 2);
    assert_eq!(
        channels.scopes(),
        vec![
            ChannelReconciliationScope::All,
            ChannelReconciliationScope::One(ChannelId::new(7)),
            ChannelReconciliationScope::One(ChannelId::new(99)),
        ]
    );
}

#[tokio::test]
async fn sqlite_change_during_reload_is_not_committed_as_applied_and_is_fenced() {
    let pool = test_pool().await;
    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(true));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([ChannelId::new(7)]));
    let reconciler =
        AutomaticIoReconciler::new(pool.clone(), channels.clone(), topology, runtime.clone());
    reconciler.reconcile_once().await.expect("startup cycle");
    sqlx::query(
        "UPDATE telemetry_points \
         SET protocol_mappings = '{\"json_path\":\"$.power\",\"data_type\":\"float\"}' \
         WHERE channel_id = 7 AND point_id = 1",
    )
    .execute(&pool)
    .await
    .expect("first mapping change");
    channels.mutate_mapping_on_next_reconcile(pool);

    let receipt = reconciler.reconcile_once().await.expect("raced cycle");

    assert!(!receipt.authority_stable());
    assert!(!receipt.converged());
    assert_eq!(runtime.fenced(), vec![vec![ChannelId::new(7)]]);
}

#[tokio::test]
async fn cancelled_loop_stops_without_starting_a_cycle() {
    let pool = test_pool().await;
    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(true));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([ChannelId::new(7)]));
    let reconciler = Arc::new(AutomaticIoReconciler::new(
        pool,
        channels,
        topology.clone(),
        runtime,
    ));
    let shutdown = CancellationToken::new();
    shutdown.cancel();

    run_automatic_io_reconciliation(reconciler, Duration::from_millis(10), shutdown).await;

    assert_eq!(topology.calls(), 0);
}

#[tokio::test]
#[ignore = "explicit long-running IO desired/applied dynamic-config soak gate"]
async fn repeated_mapping_routing_and_topology_recovery_soak() {
    let iterations = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_ITERATIONS", 1_000);
    let restart_interval = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_RESTART_INTERVAL", 25);
    let timeout = Duration::from_secs(soak_u64("AETHER_DYNAMIC_CONFIG_SOAK_TIMEOUT_SECS", 900));
    assert!(iterations > 0, "IO soak needs at least one iteration");
    assert!(restart_interval > 0, "restart interval must be non-zero");

    tokio::time::timeout(
        timeout,
        run_reconciliation_soak(iterations, restart_interval),
    )
    .await
    .unwrap_or_else(|_| panic!("IO dynamic configuration soak exceeded {timeout:?}"));
}

async fn run_reconciliation_soak(iterations: usize, restart_interval: usize) {
    let pool = test_pool().await;
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config, revision) \
         VALUES (8, 'unchanged-channel', 'mqtt', 1, '{}', 1)",
    )
    .execute(&pool)
    .await
    .expect("second desired channel");
    sqlx::query(
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) \
         VALUES (8, 1, 'unchanged-power')",
    )
    .execute(&pool)
    .await
    .expect("second desired point");
    sqlx::query(
        "CREATE TABLE measurement_routing (\
             instance_id INTEGER PRIMARY KEY, measurement_id INTEGER NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("routing authority fixture");
    sqlx::query("INSERT INTO measurement_routing VALUES (70, 100)")
        .execute(&pool)
        .await
        .expect("routing soak row");

    let channels = Arc::new(FakeChannelReconciler::new());
    let topology = Arc::new(FakeTopology::new(true));
    let runtime = Arc::new(FakeRuntimeBoundary::with_runtime_ids([
        ChannelId::new(7),
        ChannelId::new(8),
    ]));
    let reconciler = AutomaticIoReconciler::new(
        pool.clone(),
        Arc::clone(&channels),
        Arc::clone(&topology),
        Arc::clone(&runtime),
    );

    let startup = reconciler.reconcile_once().await.expect("IO soak startup");
    assert!(startup.converged());
    assert_eq!(startup.attempted_channels(), 2);
    assert_eq!(channels.scopes(), vec![ChannelReconciliationScope::All]);

    let mut recovery_cycles = 0_usize;
    for iteration in 0..iterations {
        let target = if iteration & 1 == 0 { 7 } else { 8 };
        let unaffected = if target == 7 { 8 } else { 7 };
        let scopes_before_mapping = channels.scopes().len();
        sqlx::query(
            "UPDATE telemetry_points SET protocol_mappings = ? \
             WHERE channel_id = ? AND point_id = 1",
        )
        .bind(format!("{{\"json_path\":\"$.mapping_{iteration}\"}}"))
        .bind(target)
        .execute(&pool)
        .await
        .expect("IO mapping-only mutation");

        let mapping = reconciler
            .reconcile_once()
            .await
            .expect("IO mapping reconciliation");
        assert!(mapping.converged());
        assert_eq!(mapping.attempted_channels(), 1);
        let scopes = channels.scopes();
        assert_eq!(scopes.len(), scopes_before_mapping + 1);
        assert_eq!(
            scopes.last(),
            Some(&ChannelReconciliationScope::One(ChannelId::new(target))),
            "mapping-only change rebuilt the wrong channel runtime"
        );
        assert!(
            !scopes[scopes_before_mapping..]
                .contains(&ChannelReconciliationScope::One(ChannelId::new(unaffected))),
            "mapping-only change rebuilt the unaffected runtime"
        );

        let scopes_before_route = channels.scopes().len();
        let measurement_id = if iteration & 1 == 0 { 101 } else { 100 };
        sqlx::query("UPDATE measurement_routing SET measurement_id = ? WHERE instance_id = 70")
            .bind(measurement_id)
            .execute(&pool)
            .await
            .expect("IO routing-only mutation");
        let routing = reconciler
            .reconcile_once()
            .await
            .expect("IO routing-only observation");
        assert!(routing.converged());
        assert_eq!(
            routing.attempted_channels(),
            0,
            "logical routing entered the protocol runtime fingerprint"
        );
        assert_eq!(
            channels.scopes().len(),
            scopes_before_route,
            "routing-only change churned a channel runtime"
        );

        if (iteration + 1) % restart_interval == 0 {
            recovery_cycles += 1;
            topology.set_current(false);
            let degraded = reconciler
                .reconcile_once()
                .await
                .expect("partial topology is an observed IO outcome");
            assert!(!degraded.topology_current());
            assert!(!degraded.converged());
            assert_eq!(degraded.attempted_channels(), 2);
            assert_eq!(
                runtime.fenced().last(),
                Some(&vec![ChannelId::new(7), ChannelId::new(8)]),
                "partial publication did not fail closed over all runtime authority"
            );

            topology.set_current(true);
            runtime.set_runtime_ids([ChannelId::new(7), ChannelId::new(8)]);
            let recovered = reconciler
                .reconcile_once()
                .await
                .expect("IO topology restart recovery");
            assert!(recovered.topology_current());
            assert!(recovered.authority_stable());
            assert!(recovered.converged());
            assert_eq!(recovered.attempted_channels(), 2);
            assert_eq!(
                channels.scopes().last(),
                Some(&ChannelReconciliationScope::All),
                "recovery did not rebuild the desired runtime projection"
            );
        }
    }

    let scopes = channels.scopes();
    assert_eq!(
        scopes.len(),
        1 + iterations + recovery_cycles,
        "IO reconciliation work grew beyond the exact mapping/recovery bound"
    );
    assert_eq!(
        runtime.fenced().len(),
        recovery_cycles,
        "IO fencing grew beyond the injected partial-publication bound"
    );
    assert_eq!(
        topology.calls(),
        1 + iterations * 2 + recovery_cycles * 2,
        "IO topology projection task count was unbounded"
    );
}

fn soak_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn soak_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
