//! Durable rules CAS and PointWatch publication contracts.

#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::sync::Arc;

use aether_automation::infra::rule_mutation::SqliteRuleMutator;
use aether_automation::infra::runtime_topology::{AutomationTopologyHandle, PointWatchReadiness};
use aether_calc::MemoryStateStore;
use aether_domain::PointKind;
use aether_ports::{
    AutomationRuleMutator, AutomationRulesRevision, PortErrorKind, RevisionedRuleMutation,
    RuleMutation,
};
use aether_rules::{MemoryRuleLiveState, RuleScheduler};
use aether_shm_bridge::{
    PointWatchEvent, ShmChannelHealthWriterHandle, ShmDeviceCommandSink, ShmRuntimeConfig,
    ShmWriterHandle, commit_topology_publication,
};

async fn rules_pool(max_connections: u32) -> (tempfile::TempDir, sqlx::SqlitePool) {
    let directory = tempfile::tempdir().expect("rules database directory");
    let path = directory.path().join("rules.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .expect("rules database");
    common::test_utils::schema::init_rules_schema(&pool)
        .await
        .expect("rules schema");
    (directory, pool)
}

fn scheduler(pool: &sqlx::SqlitePool) -> Arc<RuleScheduler<MemoryStateStore>> {
    Arc::new(RuleScheduler::new(
        Arc::new(MemoryRuleLiveState::new()),
        pool.clone(),
        100,
        PathBuf::from("logs/test-rule-cas"),
    ))
}

#[tokio::test]
async fn concurrent_mutations_with_one_expected_revision_have_one_winner() {
    let (_directory, pool) = rules_pool(4).await;
    let mutator = Arc::new(SqliteRuleMutator::new(pool.clone(), scheduler(&pool)));
    let expected = AutomationRulesRevision::new(1);

    let (first, second) = tokio::join!(
        mutator.mutate_revisioned(RevisionedRuleMutation::create("first", None, expected)),
        mutator.mutate_revisioned(RevisionedRuleMutation::create("second", None, expected)),
    );
    let results = [first, second];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .filter(|error| error.kind() == PortErrorKind::Conflict)
            .count(),
        1
    );
    let (count, head): (i64, i64) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM rules")
            .fetch_one(&pool)
            .await
            .expect("rule count"),
        sqlx::query_scalar(
            "SELECT revision FROM configuration_revisions WHERE scope = 'automation_rules'",
        )
        .fetch_one(&pool)
        .await
        .expect("rules head"),
    );
    assert_eq!((count, head), (1, 2));
}

#[tokio::test]
async fn legacy_rust_rule_mutation_reads_the_current_head_and_uses_the_cas_path() {
    let (_directory, pool) = rules_pool(1).await;
    let mutator = SqliteRuleMutator::new(pool.clone(), scheduler(&pool));

    let receipt = mutator
        .mutate(RuleMutation::create("legacy", None))
        .await
        .expect("legacy Rust rule mutation");

    assert_eq!(
        receipt.resulting_revision(),
        AutomationRulesRevision::new(2)
    );
    assert!(!receipt.runtime_status().reconciliation_required());
}

#[tokio::test]
async fn point_watch_publication_failure_is_gated_and_a_later_reload_recovers() {
    let (_database_directory, pool) = rules_pool(1).await;
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("IO schema");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled) \
         VALUES (3, 'fieldbus', 'virtual', 1)",
    )
    .execute(&pool)
    .await
    .expect("channel");
    sqlx::query(
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) \
         VALUES (3, 5, 'temperature')",
    )
    .execute(&pool)
    .await
    .expect("point");

    let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
        .await
        .expect("topology snapshot");
    let shm_directory = tempfile::tempdir().expect("SHM directory");
    let point_path = shm_directory.path().join("live.shm");
    let health_path = shm_directory.path().join("health.shm");
    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::new(snapshot.point_manifest().clone()),
        None,
        50,
    )
    .expect("point generation");
    let _health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::new(snapshot.health_manifest().clone()),
        50,
    )
    .expect("health generation");
    commit_topology_publication(&point_path, &health_path, 50).expect("topology commit");

    let topology_sink = Arc::new(ShmDeviceCommandSink::new());
    let topology = Arc::new(
        AutomationTopologyHandle::new_lazy(
            point_path.clone(),
            health_path,
            snapshot,
            Arc::clone(&topology_sink),
        )
        .expect("automation topology"),
    );
    assert!(topology.refresh(&pool).await.expect("topology refresh"));

    let recovery_sink = ShmDeviceCommandSink::new();
    let manifest_source = recovery_sink.manifest_source();
    let readiness = Arc::new(PointWatchReadiness::new());
    let scheduler = scheduler(&pool);
    let mutator = SqliteRuleMutator::new(pool.clone(), Arc::clone(&scheduler)).with_topology_guard(
        Arc::clone(&topology),
        Arc::clone(&readiness),
        manifest_source,
    );
    let point_watch_event = PointWatchEvent::new(
        3,
        PointKind::Telemetry,
        5,
        u64::try_from(
            topology
                .load()
                .point_manifest()
                .slot_for(aether_shm_bridge::PhysicalPointAddress::from_legacy_raw(
                    3,
                    PointKind::Telemetry,
                    5,
                ))
                .expect("point slot"),
        )
        .expect("point slot fits event wire"),
        20.0,
        20.0,
        1_000,
        1,
    );

    let gated = mutator
        .mutate_revisioned(RevisionedRuleMutation::create(
            "gated-rule",
            None,
            AutomationRulesRevision::new(1),
        ))
        .await
        .expect("durable mutation returns degraded receipt");
    assert_eq!(gated.resulting_revision(), AutomationRulesRevision::new(2));
    assert_eq!(gated.runtime_status().as_str(), "point_watch_gated");
    assert!(scheduler.is_running(), "tick fallback must remain active");
    assert!(
        !readiness.accepts(&topology.load(), point_watch_event),
        "hints must remain fail-closed while the manifest publication is unavailable"
    );

    let manifest = Arc::clone(topology.load().point_manifest());
    recovery_sink
        .open_generation(&point_path, manifest)
        .expect("publish matching command manifest");
    let recovered = mutator
        .mutate_revisioned(RevisionedRuleMutation::reload(
            AutomationRulesRevision::new(2),
        ))
        .await
        .expect("reconciliation reload");
    assert_eq!(
        recovered.resulting_revision(),
        AutomationRulesRevision::new(3)
    );
    assert!(recovered.scheduler_refresh().is_refreshed());
    assert!(
        readiness.accepts(&topology.load(), point_watch_event),
        "a matching recovery publication must reopen the exact rebuilt generation"
    );
}
