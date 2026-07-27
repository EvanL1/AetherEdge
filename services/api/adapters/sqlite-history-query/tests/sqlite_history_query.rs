use std::path::{Path, PathBuf};

use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, HistoryAggregation, HistoryDuplicatePolicy,
    NumericFeatureConstraints, SampleQuality, SourceKind, TaskIdentity, TimestampMs,
};
use aether_ports::{HistoryQuery, HistoryWindow, PortErrorKind};
use aether_sqlite_history_query::{
    CalendarFeature, SqliteHistoryFeatureRoute, SqliteHistoryQuery, SqliteHistoryQueryConfig,
};
use aether_testkit::{assert_history_query_bounded, assert_history_query_provenance};
use chrono::DateTime;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;

fn binding() -> BindingIdentity {
    BindingIdentity::new("energy.site-a", 1).expect("binding is valid")
}

fn task() -> TaskIdentity {
    TaskIdentity::new("energy.site-load-forecast", 1).expect("task is valid")
}

fn second_task() -> TaskIdentity {
    TaskIdentity::new("energy.site-load-anomaly", 1).expect("task is valid")
}

fn numeric(name: &str, unit: &str) -> FeatureDefinition {
    FeatureDefinition::numeric(name, FeatureRole::History, unit).expect("feature is valid")
}

fn timestamp(value: &str) -> TimestampMs {
    let milliseconds = DateTime::parse_from_rfc3339(value)
        .expect("timestamp is RFC 3339")
        .timestamp_millis();
    TimestampMs::new(u64::try_from(milliseconds).expect("timestamp is positive"))
}

fn routes() -> Vec<SqliteHistoryFeatureRoute> {
    vec![
        SqliteHistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            900_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            "inst:1:M",
            "101",
            "energy.site.load.active_power",
        )
        .expect("stored route is valid"),
        SqliteHistoryFeatureRoute::calendar(
            task(),
            binding(),
            numeric("quarter_hour", "1"),
            900_000,
            CalendarFeature::QuarterHourOfDay,
            "calendar.quarter_hour",
        )
        .expect("calendar route is valid"),
    ]
}

fn window(features: Vec<FeatureDefinition>, max_samples: usize) -> HistoryWindow {
    window_with_aggregation(features, max_samples, HistoryAggregation::Mean)
}

fn window_with_aggregation(
    features: Vec<FeatureDefinition>,
    max_samples: usize,
    aggregation: HistoryAggregation,
) -> HistoryWindow {
    window_with_policies(
        features,
        max_samples,
        aggregation,
        HistoryDuplicatePolicy::Latest,
    )
}

fn window_with_policies(
    features: Vec<FeatureDefinition>,
    max_samples: usize,
    aggregation: HistoryAggregation,
    duplicate_policy: HistoryDuplicatePolicy,
) -> HistoryWindow {
    HistoryWindow::new(
        task(),
        binding(),
        features,
        timestamp("2026-07-11T11:00:00Z"),
        timestamp("2026-07-11T11:30:00Z"),
        max_samples,
        aggregation,
        duplicate_policy,
    )
    .and_then(|window| window.with_cutoff(timestamp("2026-07-11T11:15:00Z")))
    .expect("window is valid")
}

struct DatabaseFixture {
    _directory: TempDir,
    path: PathBuf,
}

impl DatabaseFixture {
    async fn create(rows: &[(TimestampMs, &str, &str, Option<f64>)]) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory is created");
        let path = directory.path().join("aether-history.db");
        initialize_database(&path, rows).await;
        Self {
            _directory: directory,
            path,
        }
    }

    async fn adapter(
        &self,
        configured_routes: Vec<SqliteHistoryFeatureRoute>,
        max_raw_samples_per_feature: usize,
    ) -> SqliteHistoryQuery {
        let config = SqliteHistoryQueryConfig::new(
            &self.path,
            configured_routes,
            max_raw_samples_per_feature,
        )
        .expect("configuration is valid");
        SqliteHistoryQuery::open(config)
            .await
            .expect("read-only adapter opens")
    }
}

async fn initialize_database(path: &Path, rows: &[(TimestampMs, &str, &str, Option<f64>)]) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("test database opens");
    sqlx::query(
        "CREATE TABLE history (\
             time_ms INTEGER NOT NULL,\
             series_key TEXT NOT NULL,\
             point_id TEXT NOT NULL,\
             value REAL\
         )",
    )
    .execute(&pool)
    .await
    .expect("history schema is created by the test owner");
    for (time, series_key, point_id, value) in rows {
        sqlx::query(
            "INSERT INTO history (time_ms, series_key, point_id, value) VALUES (?, ?, ?, ?)",
        )
        .bind(i64::try_from(time.get()).expect("test timestamp fits SQLite"))
        .bind(series_key)
        .bind(point_id)
        .bind(value)
        .execute(&pool)
        .await
        .expect("raw history row is inserted");
    }
    pool.close().await;
}

async fn initialize_empty_database(path: &Path) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("empty test database opens");
    pool.close().await;
}

fn thirty_second_rows() -> Vec<(TimestampMs, &'static str, &'static str, Option<f64>)> {
    let first_lower = timestamp("2026-07-11T10:45:00Z").get();
    let second_lower = timestamp("2026-07-11T11:00:00Z").get();
    let mut rows = Vec::new();
    for index in 1_u64..=30 {
        rows.push((
            TimestampMs::new(first_lower + index * 30_000),
            "inst:1:M",
            "101",
            Some(index as f64),
        ));
        rows.push((
            TimestampMs::new(second_lower + index * 30_000),
            "inst:1:M",
            "101",
            Some(100.0 + index as f64),
        ));
    }
    rows
}

#[tokio::test]
async fn real_thirty_second_history_aggregates_to_interval_end_quarter_hour_means() {
    let database = DatabaseFixture::create(&thirty_second_rows()).await;
    let configured_routes = routes();
    assert_eq!(
        configured_routes[0].aggregation(),
        Some(HistoryAggregation::Mean)
    );
    let adapter = database.adapter(configured_routes, 60).await;
    let requested_window = window(vec![numeric("load", "kW"), numeric("quarter_hour", "1")], 2);

    let sourced = adapter
        .query(requested_window.clone())
        .await
        .expect("history is aggregated");

    assert_eq!(
        sourced.segment().timestamps(),
        &[
            timestamp("2026-07-11T11:00:00Z"),
            timestamp("2026-07-11T11:15:00Z")
        ]
    );
    assert_eq!(
        sourced.segment().series()[0].values()[0].as_number(),
        Some(15.5)
    );
    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(115.5)
    );
    assert_eq!(
        sourced.segment().series()[1].values()[0].as_number(),
        Some(44.0)
    );
    assert_eq!(
        sourced.segment().series()[1].values()[1].as_number(),
        Some(45.0)
    );
    assert_eq!(sourced.provenance()[0].source_kind(), SourceKind::History);
    assert_eq!(
        sourced.provenance()[0].watermark(),
        timestamp("2026-07-11T11:15:00Z")
    );
    assert!(
        sourced
            .provenance()
            .iter()
            .all(|source| source.watermark() <= timestamp("2026-07-11T11:15:00Z"))
    );
    assert_history_query_bounded(&adapter, requested_window.clone(), sourced.clone())
        .await
        .expect("SQLite adapter satisfies bounded history conformance");
    assert_history_query_provenance(&adapter, requested_window, sourced.provenance())
        .await
        .expect("SQLite adapter satisfies provenance conformance");
}

#[tokio::test]
async fn read_only_adapter_observes_committed_rows_while_the_history_writer_keeps_wal_open() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let path = directory.path().join("aether-history.db");
    let writer = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("history writer opens");
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&writer)
        .await
        .expect("writer enables WAL mode");
    sqlx::query("PRAGMA wal_autocheckpoint = 0")
        .execute(&writer)
        .await
        .expect("automatic checkpointing is disabled for the fixture");
    sqlx::query(
        "CREATE TABLE history (\
             time_ms INTEGER NOT NULL,\
             series_key TEXT NOT NULL,\
             point_id TEXT NOT NULL,\
             value REAL\
         )",
    )
    .execute(&writer)
    .await
    .expect("history owner creates the schema");
    for (time, value) in [
        (timestamp("2026-07-11T10:50:00Z"), 10.0_f64),
        (timestamp("2026-07-11T11:05:00Z"), 20.0_f64),
    ] {
        sqlx::query(
            "INSERT INTO history (time_ms, series_key, point_id, value) VALUES (?, ?, ?, ?)",
        )
        .bind(i64::try_from(time.get()).expect("test timestamp fits SQLite"))
        .bind("inst:1:M")
        .bind("101")
        .bind(value)
        .execute(&writer)
        .await
        .expect("writer commits a WAL observation");
    }
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    assert!(
        std::fs::metadata(&wal_path).is_ok_and(|metadata| metadata.len() > 0),
        "the writer must remain open with committed data in the WAL"
    );

    let adapter = SqliteHistoryQuery::open(
        SqliteHistoryQueryConfig::new(&path, routes(), 60).expect("configuration is valid"),
    )
    .await
    .expect("read-only adapter is configured independently");
    let sourced = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect("read-only snapshot includes committed WAL rows");
    assert_eq!(
        sourced.segment().series()[0]
            .values()
            .iter()
            .map(aether_domain::FeatureValue::as_number)
            .collect::<Vec<_>>(),
        vec![Some(10.0), Some(20.0)]
    );

    writer.close().await;
}

#[tokio::test]
async fn a_bucket_without_raw_values_is_returned_as_explicitly_missing() {
    let first_bucket = thirty_second_rows()
        .into_iter()
        .filter(|(time, ..)| *time <= timestamp("2026-07-11T11:00:00Z"))
        .collect::<Vec<_>>();
    let database = DatabaseFixture::create(&first_bucket).await;
    let adapter = database.adapter(routes(), 60).await;

    let sourced = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect("partial history returns an aligned segment");

    let load = &sourced.segment().series()[0];
    assert_eq!(load.values()[0].as_number(), Some(15.5));
    assert!(load.values()[1].is_missing());
    assert_eq!(load.quality()[1], SampleQuality::Missing);
    assert_eq!(
        sourced.provenance()[0].watermark(),
        timestamp("2026-07-11T11:00:00Z")
    );
}

#[tokio::test]
async fn sparse_tail_uses_the_latest_participating_raw_timestamp_as_watermark() {
    let rows = vec![
        (
            timestamp("2026-07-11T10:59:30Z"),
            "inst:1:M",
            "101",
            Some(10.0),
        ),
        (
            timestamp("2026-07-11T11:07:30Z"),
            "inst:1:M",
            "101",
            Some(20.0),
        ),
        (timestamp("2026-07-11T11:14:30Z"), "inst:1:M", "101", None),
    ];
    let database = DatabaseFixture::create(&rows).await;
    let adapter = database.adapter(routes(), 60).await;

    let sourced = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect("sparse history remains aligned");

    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(20.0)
    );
    assert_eq!(
        sourced.provenance()[0].watermark(),
        timestamp("2026-07-11T11:07:30Z")
    );
    assert_ne!(
        sourced.provenance()[0].watermark(),
        timestamp("2026-07-11T11:15:00Z")
    );
}

#[tokio::test]
async fn rows_after_the_authoritative_cutoff_are_never_observed() {
    let rows = vec![
        (
            timestamp("2026-07-11T10:59:30Z"),
            "inst:1:M",
            "101",
            Some(10.0),
        ),
        (
            timestamp("2026-07-11T11:10:00Z"),
            "inst:1:M",
            "101",
            Some(20.0),
        ),
        (
            timestamp("2026-07-11T11:20:00Z"),
            "inst:1:M",
            "101",
            Some(1_000_000.0),
        ),
    ];
    let database = DatabaseFixture::create(&rows).await;
    let adapter = database.adapter(routes(), 60).await;

    let sourced = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect("bounded history ignores post-cutoff rows");

    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(20.0)
    );
    assert_eq!(
        sourced.provenance()[0].watermark(),
        timestamp("2026-07-11T11:10:00Z")
    );
}

#[tokio::test]
async fn lazy_adapter_recovers_when_the_history_database_appears_later() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let path = directory.path().join("aether-history.db");
    let config = SqliteHistoryQueryConfig::new(&path, routes(), 60)
        .expect("configuration does not require an existing database");
    let adapter = SqliteHistoryQuery::open(config)
        .await
        .expect("lazy adapter does not couple application startup to history readiness");
    assert!(
        !path.exists(),
        "lazy construction must not create SQLite files"
    );

    let unavailable = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect_err("missing database is a recoverable query failure");
    assert_eq!(unavailable.kind(), PortErrorKind::Unavailable);
    assert!(
        !path.exists(),
        "read-only query failures must not create SQLite files"
    );

    initialize_database(&path, &thirty_second_rows()).await;
    let sourced = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect("the next query recovers after history becomes ready");
    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(115.5)
    );
}

#[tokio::test]
async fn lazy_adapter_recovers_when_the_history_schema_becomes_ready_later() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let path = directory.path().join("aether-history.db");
    initialize_empty_database(&path).await;
    let config = SqliteHistoryQueryConfig::new(&path, routes(), 60)
        .expect("configuration is independent of schema readiness");
    let adapter = SqliteHistoryQuery::open(config)
        .await
        .expect("lazy adapter opens without migrating schema");

    let unavailable = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect_err("uninitialized history schema is recoverable");
    assert_eq!(unavailable.kind(), PortErrorKind::Unavailable);

    initialize_database(&path, &thirty_second_rows()).await;
    let sourced = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect("the next query sees the initialized history schema");
    assert_eq!(
        sourced.segment().series()[0].values()[0].as_number(),
        Some(15.5)
    );
}

#[tokio::test]
async fn one_row_beyond_the_per_feature_raw_bound_fails_closed() {
    let lower = timestamp("2026-07-11T10:45:00Z").get();
    let rows = (1_u64..=31)
        .map(|index| {
            (
                TimestampMs::new(lower + index),
                "inst:1:M",
                "101",
                Some(index as f64),
            )
        })
        .collect::<Vec<_>>();
    let database = DatabaseFixture::create(&rows).await;
    let adapter = database.adapter(routes(), 30).await;

    let error = adapter
        .query(window(vec![numeric("load", "kW")], 2))
        .await
        .expect_err("raw truncation must never look like complete history");

    assert_eq!(error.kind(), PortErrorKind::Rejected);
}

#[tokio::test]
async fn wrong_unit_binding_or_mapping_is_rejected_before_data_is_returned() {
    let database = DatabaseFixture::create(&thirty_second_rows()).await;
    let adapter = database.adapter(routes(), 60).await;

    let wrong_unit = adapter
        .query(window(vec![numeric("load", "MW")], 2))
        .await
        .expect_err("unit is part of the commissioned mapping");
    assert_eq!(wrong_unit.kind(), PortErrorKind::Permanent);

    let wrong_binding = HistoryWindow::new(
        task(),
        BindingIdentity::new("energy.site-a", 2).expect("other binding is valid"),
        vec![numeric("load", "kW")],
        timestamp("2026-07-11T11:00:00Z"),
        timestamp("2026-07-11T11:30:00Z"),
        2,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .and_then(|window| window.with_cutoff(timestamp("2026-07-11T11:15:00Z")))
    .expect("window is valid");
    let error = adapter
        .query(wrong_binding)
        .await
        .expect_err("binding revision must match exactly");
    assert_eq!(error.kind(), PortErrorKind::Permanent);

    let wrong_aggregation = adapter
        .query(window_with_aggregation(
            vec![numeric("load", "kW")],
            2,
            HistoryAggregation::Sum,
        ))
        .await
        .expect_err("task aggregation must match the commissioned route");
    assert_eq!(wrong_aggregation.kind(), PortErrorKind::Permanent);

    let wrong_duplicate_policy = adapter
        .query(window_with_policies(
            vec![numeric("load", "kW")],
            2,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Reject,
        ))
        .await
        .expect_err("task duplicate policy must match the commissioned route");
    assert_eq!(wrong_duplicate_policy.kind(), PortErrorKind::Permanent);

    let reused = vec![
        SqliteHistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            900_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            "inst:1:M",
            "101",
            "energy.site.load.active_power",
        )
        .expect("route is valid"),
        SqliteHistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("other", "kW"),
            900_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            "inst:1:M",
            "101",
            "energy.site.other",
        )
        .expect("route itself is valid"),
    ];
    assert!(SqliteHistoryQueryConfig::new(Path::new("history.db"), reused, 60).is_err());
}

#[tokio::test]
async fn every_core_stored_aggregation_has_deterministic_interval_semantics() {
    let rows = vec![
        (
            timestamp("2026-07-11T10:50:00Z"),
            "inst:1:M",
            "101",
            Some(10.0),
        ),
        (
            timestamp("2026-07-11T10:55:00Z"),
            "inst:1:M",
            "101",
            Some(20.0),
        ),
        (
            timestamp("2026-07-11T11:05:00Z"),
            "inst:1:M",
            "101",
            Some(30.0),
        ),
    ];
    let database = DatabaseFixture::create(&rows).await;
    for (aggregation, expected) in [
        (HistoryAggregation::Mean, 15.0),
        (HistoryAggregation::Last, 20.0),
        (HistoryAggregation::Sum, 30.0),
        (HistoryAggregation::Min, 10.0),
        (HistoryAggregation::Max, 20.0),
    ] {
        let route = SqliteHistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            900_000,
            aggregation,
            HistoryDuplicatePolicy::Latest,
            "inst:1:M",
            "101",
            "energy.site.load.active_power",
        )
        .expect("explicit aggregate route is valid");
        let adapter = database.adapter(vec![route], 60).await;
        let sourced = adapter
            .query(window_with_aggregation(
                vec![numeric("load", "kW")],
                2,
                aggregation,
            ))
            .await
            .expect("commissioned aggregation succeeds");
        assert_eq!(
            sourced.segment().series()[0].values()[0].as_number(),
            Some(expected),
            "unexpected {aggregation:?} result"
        );
    }
}

#[tokio::test]
async fn duplicate_raw_timestamps_are_resolved_by_rowid_or_rejected_before_mean() {
    let rows = vec![
        (
            timestamp("2026-07-11T10:50:00Z"),
            "inst:1:M",
            "101",
            Some(10.0),
        ),
        (
            timestamp("2026-07-11T10:50:00Z"),
            "inst:1:M",
            "101",
            Some(30.0),
        ),
        (
            timestamp("2026-07-11T10:55:00Z"),
            "inst:1:M",
            "101",
            Some(20.0),
        ),
        (
            timestamp("2026-07-11T11:05:00Z"),
            "inst:1:M",
            "101",
            Some(40.0),
        ),
    ];
    let database = DatabaseFixture::create(&rows).await;
    let latest_route = SqliteHistoryFeatureRoute::stored(
        task(),
        binding(),
        numeric("load", "kW"),
        900_000,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
        "inst:1:M",
        "101",
        "energy.site.load.active_power",
    )
    .expect("latest duplicate route is valid");
    assert_eq!(
        latest_route.duplicate_policy(),
        Some(HistoryDuplicatePolicy::Latest)
    );
    let latest = database.adapter(vec![latest_route], 60).await;
    let sourced = latest
        .query(window_with_policies(
            vec![numeric("load", "kW")],
            2,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
        ))
        .await
        .expect("latest rowid is selected before aggregation");
    assert_eq!(
        sourced.segment().series()[0].values()[0].as_number(),
        Some(25.0),
        "the superseded value must not be double-weighted"
    );

    let reject_route = SqliteHistoryFeatureRoute::stored(
        task(),
        binding(),
        numeric("load", "kW"),
        900_000,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Reject,
        "inst:1:M",
        "101",
        "energy.site.load.active_power",
    )
    .expect("reject duplicate route is valid");
    let reject = database.adapter(vec![reject_route], 60).await;
    let error = reject
        .query(window_with_policies(
            vec![numeric("load", "kW")],
            2,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Reject,
        ))
        .await
        .expect_err("duplicate timestamps are rejected by policy");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn raw_values_are_checked_before_aggregation_can_hide_a_violation() {
    let rows = vec![
        (
            timestamp("2026-07-11T10:50:00Z"),
            "inst:1:M",
            "101",
            Some(-1.0),
        ),
        (
            timestamp("2026-07-11T10:55:00Z"),
            "inst:1:M",
            "101",
            Some(1.0),
        ),
    ];
    let database = DatabaseFixture::create(&rows).await;
    let definition = numeric("rain", "mm")
        .with_numeric_constraints(
            NumericFeatureConstraints::new(Some(0.0), None, false).expect("limits are valid"),
        )
        .expect("constrained feature is valid");
    let route = SqliteHistoryFeatureRoute::stored(
        task(),
        binding(),
        definition.clone(),
        900_000,
        HistoryAggregation::Sum,
        HistoryDuplicatePolicy::Latest,
        "inst:1:M",
        "101",
        "weather.observed.precipitation",
    )
    .expect("route is valid");
    let adapter = database.adapter(vec![route], 60).await;

    let error = adapter
        .query(window_with_aggregation(
            vec![definition],
            2,
            HistoryAggregation::Sum,
        ))
        .await
        .expect_err("a negative raw observation must not be hidden by its bucket sum");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn two_tasks_can_reuse_one_physical_series_with_distinct_source_policies() {
    let rows = vec![
        (
            timestamp("2026-07-11T10:50:00Z"),
            "inst:1:M",
            "101",
            Some(10.0),
        ),
        (
            timestamp("2026-07-11T10:55:00Z"),
            "inst:1:M",
            "101",
            Some(20.0),
        ),
    ];
    let database = DatabaseFixture::create(&rows).await;
    let routes = vec![
        SqliteHistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            900_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            "inst:1:M",
            "101",
            "energy.site.load.active_power",
        )
        .expect("forecast source route is valid"),
        SqliteHistoryFeatureRoute::stored(
            second_task(),
            binding(),
            numeric("load", "kW"),
            900_000,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Latest,
            "inst:1:M",
            "101",
            "energy.site.load.active_power",
        )
        .expect("anomaly source route is valid"),
    ];
    let adapter = database.adapter(routes, 60).await;
    let second_window = HistoryWindow::new(
        second_task(),
        binding(),
        vec![numeric("load", "kW")],
        timestamp("2026-07-11T11:00:00Z"),
        timestamp("2026-07-11T11:30:00Z"),
        2,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Latest,
    )
    .and_then(|window| window.with_cutoff(timestamp("2026-07-11T11:15:00Z")))
    .expect("task-scoped window is valid");

    let sourced = adapter
        .query(second_window)
        .await
        .expect("task identity selects the last-value source plan");
    assert_eq!(
        sourced.segment().series()[0].values()[0].as_number(),
        Some(20.0)
    );
}
