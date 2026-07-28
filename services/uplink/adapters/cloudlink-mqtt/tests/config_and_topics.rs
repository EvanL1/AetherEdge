use std::path::PathBuf;
use std::str::FromStr;

use aether_cloudlink_mqtt::{
    CloudLinkMigrationMode, CloudLinkMqttConfig, CloudLinkTlsConfig, DeploymentSecurity,
    MqttClientIdentity, SecretString, TopicNamespace,
};
use aether_domain::TimestampMs;
use aether_ports::{
    CloudLinkEnqueue, CloudLinkMessageKind, CloudLinkSpool, CloudLinkTransportRoute,
};
use aether_store_local::MemoryCloudLinkSpool;

#[test]
fn topic_namespace_is_versioned_exact_and_isolated_from_legacy_topics() {
    let topics = TopicNamespace::new("customer/site-a", "33333333-3333-4333-8333-333333333333")
        .expect("topics");

    assert_eq!(
        topics.topic(CloudLinkTransportRoute::SessionUp),
        "customer/site-a/v1/gateways/33333333-3333-4333-8333-333333333333/up/session"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::TelemetryUp),
        "customer/site-a/v1/gateways/33333333-3333-4333-8333-333333333333/up/telemetry"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::AckDown),
        "customer/site-a/v1/gateways/33333333-3333-4333-8333-333333333333/down/ack"
    );
    assert_eq!(topics.publish_topics().len(), 5);
    assert_eq!(topics.subscribe_topics().len(), 3);
    for topic in topics
        .publish_topics()
        .into_iter()
        .chain(topics.subscribe_topics())
    {
        assert!(!topic.contains("property/"));
        assert!(!topic.contains("status/"));
        assert!(!topic.contains("write/"));
        assert!(!topic.contains('+'));
        assert!(!topic.contains('#'));
    }
}

#[test]
fn topic_segments_fail_closed_on_wildcards_controls_empty_or_untrusted_paths() {
    for (prefix, gateway) in [
        ("", "33333333-3333-4333-8333-333333333333"),
        ("customer//site", "33333333-3333-4333-8333-333333333333"),
        ("customer/+", "33333333-3333-4333-8333-333333333333"),
        ("customer/#", "33333333-3333-4333-8333-333333333333"),
        ("customer/site", "gateway-17"),
        ("customer/site", "gateway/17"),
        ("customer/site", "gateway\0secret"),
        ("customer/site", "tenant name"),
    ] {
        assert!(
            TopicNamespace::new(prefix, gateway).is_err(),
            "must reject prefix={prefix:?}, gateway={gateway:?}"
        );
    }
}

#[test]
fn inbound_topics_map_only_to_the_three_allowed_downlink_routes() {
    let topics =
        TopicNamespace::new("aether", "33333333-3333-4333-8333-333333333333").expect("topics");
    for route in [
        CloudLinkTransportRoute::SessionDown,
        CloudLinkTransportRoute::AckDown,
        CloudLinkTransportRoute::ReplayDown,
    ] {
        assert_eq!(topics.inbound_route(&topics.topic(route)), Some(route));
    }
    assert_eq!(
        topics.inbound_route(&topics.topic(CloudLinkTransportRoute::TelemetryUp)),
        None
    );
    assert_eq!(
        topics.inbound_route("aether/v1/gateways/another/down/ack"),
        None
    );
}

#[test]
fn migration_mode_is_explicit_and_legacy_remains_the_compatibility_default() {
    assert_eq!(
        CloudLinkMigrationMode::default(),
        CloudLinkMigrationMode::Legacy
    );
    assert_eq!(
        CloudLinkMigrationMode::from_str("cloudlink-v1").expect("mode"),
        CloudLinkMigrationMode::CloudLinkV1
    );
    assert_eq!(
        CloudLinkMigrationMode::from_str("dual").expect("mode"),
        CloudLinkMigrationMode::Dual
    );
    assert!(CloudLinkMigrationMode::Dual.legacy_enabled());
    assert!(CloudLinkMigrationMode::Dual.cloudlink_enabled());
    assert!(!CloudLinkMigrationMode::Legacy.cloudlink_enabled());
    assert!(CloudLinkMigrationMode::from_str("write-through").is_err());
}

#[tokio::test]
async fn dual_mode_keeps_one_cloudlink_identity_for_the_same_business_fact() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("spool");
    let content = CloudLinkEnqueue::new(
        CloudLinkMessageKind::TelemetryBatch,
        "batch-1",
        format!("sha256:{}", "a".repeat(64)),
        br#"{"samples":[]}"#.to_vec(),
        TimestampMs::new(1),
        None,
    );

    assert!(CloudLinkMigrationMode::CloudLinkV1.cloudlink_enabled());
    let cloudlink_only = spool
        .enqueue(content.clone())
        .await
        .expect("CloudLink identity");
    assert!(CloudLinkMigrationMode::Dual.legacy_enabled());
    assert!(CloudLinkMigrationMode::Dual.cloudlink_enabled());
    let dual = spool
        .enqueue(content)
        .await
        .expect("same CloudLink fact in dual mode");

    assert_eq!(dual.identity(), cloudlink_only.identity());
    assert_eq!(dual.digest(), cloudlink_only.digest());
    assert_eq!(spool.status().await.expect("status").pending_records(), 1);
}

#[test]
fn production_requires_tls_and_custom_client_identity_is_all_or_nothing() {
    let plaintext = CloudLinkMqttConfig::development(
        "broker.example",
        1883,
        "33333333-3333-4333-8333-333333333333",
    );
    assert!(plaintext.validate(DeploymentSecurity::Development).is_ok());
    assert!(plaintext.validate(DeploymentSecurity::Production).is_err());

    let root = tempfile::tempdir().expect("temp dir");
    let mut incomplete = CloudLinkMqttConfig::development(
        "broker.example",
        8883,
        "33333333-3333-4333-8333-333333333333",
    );
    incomplete.tls = CloudLinkTlsConfig::Custom {
        ca_path: root.path().join("ca.pem"),
        client_identity: Some(MqttClientIdentity {
            certificate_path: root.path().join("client.crt"),
            private_key_path: PathBuf::new(),
        }),
    };
    assert!(incomplete.validate(DeploymentSecurity::Production).is_err());
}

#[test]
fn credentials_are_redacted_from_debug_and_validation_errors() {
    let secret = SecretString::new("private-broker-secret");
    assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");

    let mut config = CloudLinkMqttConfig::development(
        "broker.example",
        1883,
        "33333333-3333-4333-8333-333333333333",
    );
    config.username = Some("gateway-user".to_string());
    config.password = Some(secret);
    let debug = format!("{config:?}");
    assert!(!debug.contains("private-broker-secret"));

    let error = config
        .validate(DeploymentSecurity::Production)
        .expect_err("production plaintext");
    assert!(!error.to_string().contains("private-broker-secret"));
}

#[test]
fn packet_and_connection_bounds_are_validated_before_rumqttc_can_panic() {
    let mut config = CloudLinkMqttConfig::development(
        "broker.example",
        1883,
        "33333333-3333-4333-8333-333333333333",
    );
    config.keep_alive_secs = 0;
    assert!(config.validate(DeploymentSecurity::Development).is_err());
    config.keep_alive_secs = 30;
    config.maximum_packet_bytes = aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES + 1;
    assert!(config.validate(DeploymentSecurity::Development).is_err());
    config.maximum_packet_bytes = aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES;
    config.broker_host = "mqtt://broker.example/secret".to_string();
    assert!(config.validate(DeploymentSecurity::Development).is_err());
}
