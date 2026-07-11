use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_postgres_history::{HISTORY_SCHEMA, HistoryRow, sample_to_row};

#[test]
fn history_mapping_uses_domain_fields_instead_of_redis_keys() {
    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Status, PointId::new(7)),
        1.0,
        TimestampMs::new(1_720_000_000_000),
        PointQuality::Uncertain,
    );

    assert_eq!(
        sample_to_row(sample).unwrap(),
        HistoryRow {
            instance_id: 42,
            point_kind: 1,
            point_id: 7,
            value: 1.0,
            timestamp_ms: 1_720_000_000_000,
            quality: 1,
        }
    );
}

#[test]
fn schema_has_no_redis_key_column_or_timescale_requirement() {
    let normalized = HISTORY_SCHEMA.to_ascii_lowercase();
    assert!(normalized.contains("create table if not exists aether_history"));
    assert!(!normalized.contains("redis_key"));
    assert!(!normalized.contains("hypertable"));
}
