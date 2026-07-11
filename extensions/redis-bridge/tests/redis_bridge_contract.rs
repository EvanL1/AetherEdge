use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_redis_bridge::{RedisKeyspace, RedisStateMirror, encode_sample};
use serde_json::Value;

fn sample() -> PointSample {
    PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(7)),
        23.5,
        TimestampMs::new(1_720_000_000_000),
        PointQuality::Good,
    )
}

#[test]
fn mirror_keyspace_is_derived_from_domain_addresses() {
    let keyspace = RedisKeyspace::new("tenant-a");
    assert_eq!(
        keyspace.state_key(sample().address()),
        "tenant-a:state:42:telemetry"
    );
    assert_eq!(keyspace.point_field(sample().address()), "7");
}

#[test]
fn mirror_payload_contains_value_time_and_quality() {
    let encoded = encode_sample(sample()).unwrap();
    let payload: Value = serde_json::from_str(&encoded).unwrap();

    assert_eq!(payload["value"], 23.5);
    assert_eq!(payload["timestamp_ms"], 1_720_000_000_000_u64);
    assert_eq!(payload["quality"], "good");
}

#[tokio::test]
async fn malformed_redis_url_is_a_permanent_configuration_error() {
    let error = RedisStateMirror::connect("not-a-redis-url", "aether")
        .await
        .expect_err("invalid URL must be rejected before network access");

    assert!(!error.is_retryable());
}
