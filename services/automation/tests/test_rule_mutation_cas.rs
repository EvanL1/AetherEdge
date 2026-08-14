//! Durable rules CAS and PointWatch publication contracts.

#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aether_automation::infra::rule_mutation::SqliteRuleMutator;
use aether_automation::infra::rule_runtime::RuleRuntimeCoordinator;
use aether_automation::infra::runtime_topology::AutomationTopologyHandle;
use aether_domain::{PointKind, RuleId};
use aether_ports::{
    AutomationRuleMutator, AutomationRulesRevision, PortErrorKind, RevisionedRuleMutation,
    RuleMutation,
};
use aether_rules::{MemoryRuleLiveState, PointWatchDispatcher, RuleScheduler};
use aether_shm_bridge::{
    PointWatchEvent, ShmChannelHealthWriterHandle, ShmDeviceCommandSink, ShmRuntimeConfig,
    ShmWriterHandle, SubscriptionBitmap, commit_topology_publication,
};

async fn rules_pool(max_connections: u32) -> (tempfile::TempDir, sqlx::SqlitePool) {
    let directory = tempfile::tempdir().expect("rules database directory");
    let path = directory.path().join("rules.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .expect("rules database");
    common::schema::init_rules_schema(&pool)
        .await
        .expect("rules schema");
    (directory, pool)
}

fn scheduler(pool: &sqlx::SqlitePool) -> Arc<RuleScheduler> {
    Arc::new(RuleScheduler::new(
        Arc::new(MemoryRuleLiveState::new()),
        pool.clone(),
        100,
        PathBuf::from("logs/test-rule-cas"),
    ))
}

async fn corrupt_disabled_rule(pool: &sqlx::SqlitePool, mutator: &SqliteRuleMutator) -> RuleId {
    let receipt = mutator
        .mutate_revisioned(RevisionedRuleMutation::create(
            "legacy-corrupt",
            None,
            AutomationRulesRevision::new(1),
        ))
        .await
        .expect("create disabled rule");
    let rule_id = receipt.rule_id().expect("created rule id");
    sqlx::query("UPDATE rules SET trigger_config = ? WHERE id = ?")
        .bind(r#"{"type":"interval","interval_ms":0}"#)
        .bind(i64::try_from(rule_id.get()).expect("test rule id fits SQLite"))
        .execute(pool)
        .await
        .expect("emulate corrupt legacy trigger");
    rule_id
}

async fn assert_activation_rejected_without_revision_change(
    pool: &sqlx::SqlitePool,
    result: aether_ports::PortResult<aether_ports::RuleMutationReceipt>,
    rule_id: RuleId,
) {
    let error = result.expect_err("invalid stored trigger must reject activation");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    let (enabled, revision): (i64, i64) = (
        sqlx::query_scalar("SELECT enabled FROM rules WHERE id = ?")
            .bind(i64::try_from(rule_id.get()).expect("test rule id fits SQLite"))
            .fetch_one(pool)
            .await
            .expect("stored enabled flag"),
        sqlx::query_scalar(
            "SELECT revision FROM configuration_revisions WHERE scope = 'automation_rules'",
        )
        .fetch_one(pool)
        .await
        .expect("rules revision"),
    );
    assert_eq!(enabled, 0, "failed activation must remain disabled");
    assert_eq!(revision, 2, "failed activation must roll back its CAS bump");
}

#[tokio::test]
async fn set_enabled_rejects_invalid_stored_trigger_and_rolls_back() {
    let (_directory, pool) = rules_pool(1).await;
    let runtime = Arc::new(RuleRuntimeCoordinator::new(scheduler(&pool)));
    let mutator = SqliteRuleMutator::new(pool.clone(), runtime);
    let rule_id = corrupt_disabled_rule(&pool, &mutator).await;

    let result = mutator
        .mutate_revisioned(RevisionedRuleMutation::set_enabled(
            rule_id,
            true,
            AutomationRulesRevision::new(2),
        ))
        .await;

    assert_activation_rejected_without_revision_change(&pool, result, rule_id).await;
}

#[tokio::test]
async fn set_enabled_rejects_missing_trigger_and_rolls_back() {
    let (_directory, pool) = rules_pool(1).await;
    let runtime = Arc::new(RuleRuntimeCoordinator::new(scheduler(&pool)));
    let mutator = SqliteRuleMutator::new(pool.clone(), runtime);
    let receipt = mutator
        .mutate_revisioned(RevisionedRuleMutation::create(
            "missing-trigger",
            None,
            AutomationRulesRevision::new(1),
        ))
        .await
        .expect("create disabled rule shell");
    let rule_id = receipt.rule_id().expect("created rule id");

    let result = mutator
        .mutate_revisioned(RevisionedRuleMutation::set_enabled(
            rule_id,
            true,
            AutomationRulesRevision::new(2),
        ))
        .await;

    assert_activation_rejected_without_revision_change(&pool, result, rule_id).await;
}

#[tokio::test]
async fn update_enable_rejects_invalid_stored_trigger_and_rolls_back() {
    let (_directory, pool) = rules_pool(1).await;
    let runtime = Arc::new(RuleRuntimeCoordinator::new(scheduler(&pool)));
    let mutator = SqliteRuleMutator::new(pool.clone(), runtime);
    let rule_id = corrupt_disabled_rule(&pool, &mutator).await;
    let mutation = RuleMutation::Update {
        rule_id,
        name: None,
        description: None,
        enabled: Some(true),
        priority: None,
        cooldown_ms: None,
        flow_json: None,
        trigger_config: None,
    };

    let result = mutator
        .mutate_revisioned(RevisionedRuleMutation::new(
            mutation,
            AutomationRulesRevision::new(2),
        ))
        .await;

    assert_activation_rejected_without_revision_change(&pool, result, rule_id).await;
}

#[tokio::test]
async fn concurrent_mutations_with_one_expected_revision_have_one_winner() {
    let (_directory, pool) = rules_pool(4).await;
    let runtime = Arc::new(RuleRuntimeCoordinator::new(scheduler(&pool)));
    let mutator = Arc::new(SqliteRuleMutator::new(pool.clone(), runtime));
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
async fn point_watch_publication_failure_is_gated_and_a_later_reload_recovers() {
    let (_database_directory, pool) = rules_pool(1).await;
    common::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    common::schema::init_io_schema(&pool)
        .await
        .expect("IO schema");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled) \
         VALUES (3, 'fieldbus', 'modbus_tcp', 1)",
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

    let snapshot = aether_sqlite_topology::load_sqlite_live_topology(&pool)
        .await
        .expect("topology snapshot");
    let shm_directory = tempfile::tempdir().expect("SHM directory");
    let point_path = shm_directory.path().join("live.shm");
    let health_path = shm_directory.path().join("health.shm");
    let _point_writer = ShmWriterHandle::create(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::new(snapshot.point_manifest().clone()),
        None,
        None,
        50,
    )
    .expect("point generation");
    let _health_writer = ShmChannelHealthWriterHandle::create(
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
    let mut configured_scheduler = RuleScheduler::new(
        Arc::new(MemoryRuleLiveState::new()),
        pool.clone(),
        100,
        PathBuf::from("logs/test-rule-cas-point-watch"),
    );
    let (dispatcher, _events) = PointWatchDispatcher::new();
    configured_scheduler.set_point_watch_rebuild_handle(Arc::new(Mutex::new(dispatcher)));
    let scheduler = Arc::new(configured_scheduler);
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory(8).expect("bitmap"));
    let runtime = Arc::new(
        RuleRuntimeCoordinator::new(Arc::clone(&scheduler)).with_point_watch(
            Arc::clone(&topology),
            bitmap,
            manifest_source,
        ),
    );
    let mutator = SqliteRuleMutator::new(pool.clone(), Arc::clone(&runtime));
    let point_watch_event = PointWatchEvent::new(
        3,
        PointKind::Telemetry,
        5,
        topology
            .load()
            .point_manifest()
            .slot_for(aether_shm_bridge::PhysicalPointAddress::from_raw_ids(
                3,
                PointKind::Telemetry,
                5,
            ))
            .expect("point slot"),
    )
    .expect("point slot fits event wire");

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
        !runtime.accepts_point_watch(&topology.load(), point_watch_event),
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
        runtime.accepts_point_watch(&topology.load(), point_watch_event),
        "a matching recovery publication must reopen the exact rebuilt generation"
    );
}
