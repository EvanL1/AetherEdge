use std::fs;
use std::path::PathBuf;

use aether_domain::{BindingIdentity, FeatureDefinition, FeatureRole, SourceKind, TimestampMs};
use aether_ports::{CovariateSource, CovariateWindow, PortErrorKind};
use aether_store_local::{SnapshotCovariateLimits, SnapshotCovariateSource};
use tempfile::TempDir;

const MULTI_RUN_SNAPSHOT: &str = r#"
{
  "schema": "aether.covariate-snapshot.v1",
  "bindings": [
    {
      "id": "site-a",
      "revision": 1,
      "runs": [
        {
          "issued_at_ms": 1000,
          "watermark_ms": 1200,
          "valid_times_ms": [4000, 5000],
          "features": [
            {
              "name": "temperature",
              "value_type": "number",
              "unit": "Cel",
              "source_ref": "weather.nwp.temperature",
              "values": [10.0, 11.0],
              "quality": ["good", "good"]
            }
          ]
        },
        {
          "issued_at_ms": 3500,
          "watermark_ms": 3700,
          "valid_times_ms": [4000, 5000],
          "features": [
            {
              "name": "temperature",
              "value_type": "number",
              "unit": "Cel",
              "source_ref": "weather.nwp.temperature",
              "values": [30.0, 31.0],
              "quality": ["good", "good"]
            }
          ]
        },
        {
          "issued_at_ms": 2000,
          "watermark_ms": 2500,
          "valid_times_ms": [4000, 5000],
          "features": [
            {
              "name": "temperature",
              "value_type": "number",
              "unit": "Cel",
              "source_ref": "weather.nwp.temperature",
              "values": [20.0, 21.0],
              "quality": ["good", "uncertain"]
            }
          ]
        }
      ]
    }
  ]
}
"#;

const CALENDAR_SNAPSHOT: &str = r#"
{
  "schema": "aether.covariate-snapshot.v1",
  "bindings": [
    {
      "id": "site-a",
      "revision": 1,
      "runs": [
        {
          "issued_at_ms": 0,
          "watermark_ms": 1,
          "valid_times_ms": [900000, 1800000, 2700000],
          "features": [
            {
              "name": "temperature",
              "value_type": "number",
              "unit": "Cel",
              "source_ref": "weather.nwp.temperature",
              "values": [20.0, 21.0, 22.0],
              "quality": ["good", "good", "good"]
            }
          ]
        }
      ]
    }
  ]
}
"#;

fn binding() -> BindingIdentity {
    BindingIdentity::new("site-a", 1).expect("binding fixture is valid")
}

fn feature(name: &str, unit: &str) -> FeatureDefinition {
    FeatureDefinition::numeric(name, FeatureRole::FutureCovariate, unit)
        .expect("feature fixture is valid")
}

fn limits() -> SnapshotCovariateLimits {
    SnapshotCovariateLimits::new(64 * 1024, 4, 8, 8, 16).expect("test limits are positive")
}

fn write_snapshot(directory: &TempDir, contents: &str) -> PathBuf {
    let path = directory.path().join("covariates.json");
    fs::write(&path, contents).expect("snapshot fixture can be written");
    path
}

fn window(
    features: Vec<FeatureDefinition>,
    as_of: u64,
    start: u64,
    end: u64,
    samples: usize,
) -> CovariateWindow {
    CovariateWindow::new(
        binding(),
        features,
        TimestampMs::new(as_of),
        TimestampMs::new(start),
        TimestampMs::new(end),
        samples,
    )
    .expect("window fixture is valid")
}

#[tokio::test]
async fn snapshot_source_selects_the_newest_run_issued_at_or_before_as_of() {
    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, MULTI_RUN_SNAPSHOT);
    let source = SnapshotCovariateSource::open(path, limits()).expect("snapshot is valid");

    let result = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect("latest eligible run is available");

    let values = result.segment().series()[0]
        .values()
        .iter()
        .map(|value| value.as_number().expect("fixture values are numeric"))
        .collect::<Vec<_>>();
    assert_eq!(values, vec![20.0, 21.0]);
    assert_eq!(result.provenance()[0].source_kind(), SourceKind::Covariate);
    assert_eq!(
        result.provenance()[0].source_ref(),
        Some("weather.nwp.temperature")
    );
    assert_eq!(
        result.provenance()[0].issued_at(),
        Some(TimestampMs::new(2_000))
    );
    assert_eq!(result.provenance()[0].watermark(), TimestampMs::new(2_500));

    let no_visible_run = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            500,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("a source issued after the cutoff must not be visible");
    assert_eq!(no_visible_run.kind(), PortErrorKind::Unavailable);

    let watermark_after_cutoff = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            2_300,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("the newest issued run must not leak a later watermark");
    assert_eq!(watermark_after_cutoff.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_source_generates_quarter_hour_and_projects_the_exact_grid() {
    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, CALENDAR_SNAPSHOT);
    let source = SnapshotCovariateSource::open(path, limits()).expect("snapshot is valid");

    let result = source
        .resolve(window(
            vec![feature("quarter_hour", "1"), feature("temperature", "Cel")],
            1,
            900_000,
            2_700_000,
            2,
        ))
        .await
        .expect("calendar and forecast data are aligned");

    assert_eq!(
        result.segment().timestamps(),
        &[TimestampMs::new(900_000), TimestampMs::new(1_800_000)]
    );
    assert_eq!(
        result.segment().series()[0].definition().name(),
        "quarter_hour"
    );
    assert_eq!(
        result.segment().series()[0]
            .values()
            .iter()
            .map(|value| value.as_number().expect("calendar values are numeric"))
            .collect::<Vec<_>>(),
        vec![1.0, 2.0]
    );
    assert_eq!(result.provenance()[0].source_kind(), SourceKind::Calendar);
    assert_eq!(
        result.provenance()[0].source_ref(),
        Some("calendar.utc.quarter_hour")
    );
    assert_eq!(result.provenance()[0].watermark(), TimestampMs::new(1));
    assert_eq!(result.provenance()[0].issued_at(), None);
    assert_eq!(
        result.segment().series()[1]
            .values()
            .iter()
            .map(|value| value.as_number().expect("forecast values are numeric"))
            .collect::<Vec<_>>(),
        vec![20.0, 21.0]
    );
}

#[tokio::test]
async fn snapshot_source_never_falls_back_from_the_latest_run_on_contract_mismatch() {
    let mismatched_latest = MULTI_RUN_SNAPSHOT.replace(
        "\"issued_at_ms\": 2000,\n          \"watermark_ms\": 2500,\n          \"valid_times_ms\": [4000, 5000]",
        "\"issued_at_ms\": 2000,\n          \"watermark_ms\": 2500,\n          \"valid_times_ms\": [4000, 5500]",
    );
    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, &mismatched_latest);
    let source = SnapshotCovariateSource::open(&path, limits())
        .expect("construction retains the runtime snapshot path");

    let error = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("the latest run must match the requested valid-time grid exactly");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);

    let unit_mismatch = MULTI_RUN_SNAPSHOT.replace(
        "\"unit\": \"Cel\",\n              \"source_ref\": \"weather.nwp.temperature\",\n              \"values\": [20.0, 21.0]",
        "\"unit\": \"K\",\n              \"source_ref\": \"weather.nwp.temperature\",\n              \"values\": [20.0, 21.0]",
    );
    let staged = directory.path().join("unit-mismatch.json");
    fs::write(&staged, unit_mismatch).expect("unit-mismatched run can be staged");
    fs::rename(staged, path).expect("unit-mismatched run can be atomically published");

    let unit_error = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("an older matching unit must not replace the latest eligible run");
    assert_eq!(unit_error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_source_rejects_unknown_fields_and_oversized_files_at_resolution_time() {
    let with_unknown_field = MULTI_RUN_SNAPSHOT.replacen(
        "\"schema\": \"aether.covariate-snapshot.v1\"",
        "\"schema\": \"aether.covariate-snapshot.v1\", \"unexpected\": true",
        1,
    );
    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, &with_unknown_field);
    let strict_source = SnapshotCovariateSource::open(&path, limits())
        .expect("construction does not read runtime-owned snapshots");
    let strict_error = strict_source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("unknown fields must not be ignored");
    assert_eq!(strict_error.kind(), PortErrorKind::InvalidData);

    let tiny_limits =
        SnapshotCovariateLimits::new(32, 4, 8, 8, 16).expect("positive limits are valid");
    let size_source = SnapshotCovariateSource::open(path, tiny_limits)
        .expect("construction does not read runtime-owned snapshots");
    let size_error = size_source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("the file must be bounded before parsing");
    assert_eq!(size_error.kind(), PortErrorKind::Rejected);

    let invalid_limits =
        SnapshotCovariateLimits::new(0, 4, 8, 8, 16).expect_err("zero limits are unsafe");
    assert_eq!(invalid_limits.kind(), PortErrorKind::Rejected);
}

#[tokio::test]
async fn snapshot_source_starts_without_a_file_and_observes_a_later_atomic_publish() {
    let directory = TempDir::new().expect("temporary directory is available");
    let path = directory.path().join("not-published-yet.json");
    let source = SnapshotCovariateSource::open(&path, limits())
        .expect("an optional runtime source must not block startup");

    let missing = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect_err("an unpublished snapshot is unavailable");
    assert_eq!(missing.kind(), PortErrorKind::Unavailable);

    let staged = directory.path().join("staged.json");
    fs::write(&staged, MULTI_RUN_SNAPSHOT).expect("staged snapshot can be written");
    fs::rename(staged, &path).expect("snapshot can be atomically published");

    let available = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            3_000,
            4_000,
            6_000,
            2,
        ))
        .await
        .expect("the next resolution observes the published snapshot");
    assert_eq!(
        available.segment().series()[0].values()[0].as_number(),
        Some(20.0)
    );
}

#[tokio::test]
async fn snapshot_source_observes_each_atomic_update_and_fails_closed_on_a_bad_one() {
    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, MULTI_RUN_SNAPSHOT);
    let source = SnapshotCovariateSource::open(&path, limits())
        .expect("construction only retains the runtime path and bounds");
    let request = || window(vec![feature("temperature", "Cel")], 3_000, 4_000, 6_000, 2);

    let initial = source
        .resolve(request())
        .await
        .expect("the initial snapshot is available");
    assert_eq!(
        initial.segment().series()[0].values()[0].as_number(),
        Some(20.0)
    );

    let updated_snapshot = MULTI_RUN_SNAPSHOT.replace("[20.0, 21.0]", "[40.0, 41.0]");
    let staged_update = directory.path().join("updated.json");
    fs::write(&staged_update, updated_snapshot).expect("updated snapshot can be staged");
    fs::rename(staged_update, &path).expect("updated snapshot can be atomically published");

    let updated = source
        .resolve(request())
        .await
        .expect("the next request reloads the published update");
    assert_eq!(
        updated.segment().series()[0].values()[0].as_number(),
        Some(40.0)
    );

    let staged_bad = directory.path().join("bad.json");
    fs::write(&staged_bad, r#"{"schema":"not-supported","bindings":[]}"#)
        .expect("bad update can be staged");
    fs::rename(staged_bad, &path).expect("bad update can be atomically published");

    let bad_update = source
        .resolve(request())
        .await
        .expect_err("a bad update must not reuse the prior in-memory snapshot");
    assert_eq!(bad_update.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_source_enforces_response_feature_and_sample_bounds() {
    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, CALENDAR_SNAPSHOT);
    let feature_bounded =
        SnapshotCovariateLimits::new(64 * 1024, 4, 8, 1, 3).expect("positive limits are valid");
    let source = SnapshotCovariateSource::open(&path, feature_bounded)
        .expect("one stored feature is within the configured bound");

    let feature_error = source
        .resolve(window(
            vec![feature("quarter_hour", "1"), feature("temperature", "Cel")],
            1,
            900_000,
            2_700_000,
            2,
        ))
        .await
        .expect_err("calendar fields count toward the response feature bound");
    assert_eq!(feature_error.kind(), PortErrorKind::Rejected);

    let sample_error = source
        .resolve(window(
            vec![feature("temperature", "Cel")],
            1,
            900_000,
            4_500_000,
            4,
        ))
        .await
        .expect_err("requests cannot exceed the configured sample bound");
    assert_eq!(sample_error.kind(), PortErrorKind::Rejected);
}

#[test]
fn snapshot_covariate_source_is_a_covariate_port() {
    fn accepts_covariates(_: &dyn CovariateSource) {}

    let directory = TempDir::new().expect("temporary directory is available");
    let path = write_snapshot(&directory, MULTI_RUN_SNAPSHOT);
    let source = SnapshotCovariateSource::open(path, limits()).expect("snapshot is valid");
    accepts_covariates(&source);
}
