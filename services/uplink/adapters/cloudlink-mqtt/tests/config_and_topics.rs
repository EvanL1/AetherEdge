use std::path::PathBuf;
use std::sync::Arc;

use aether_cloudlink_mqtt::{
    CloudLinkMqttConfig, CloudLinkTlsConfig, DeploymentSecurity, MqttClientIdentity,
    MqttCloudLinkTransport, SecretString, TopicNamespace,
};
use aether_ports::{
    CloudLinkTransport, CloudLinkTransportMessage, CloudLinkTransportRoute, PortErrorKind,
};

#[test]
fn topic_namespace_is_exact_and_contains_no_compatibility_branch() {
    let topics = TopicNamespace::new("customer/site-a", "33333333-3333-4333-8333-333333333333")
        .expect("topics");

    assert_eq!(
        topics.topic(CloudLinkTransportRoute::SessionUp),
        "customer/site-a/gateways/33333333-3333-4333-8333-333333333333/up/session"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::TelemetryUp),
        "customer/site-a/gateways/33333333-3333-4333-8333-333333333333/up/telemetry"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::AlarmUp),
        "customer/site-a/gateways/33333333-3333-4333-8333-333333333333/up/alarm"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::IntegrationTopologyUp),
        "customer/site-a/gateways/33333333-3333-4333-8333-333333333333/up/integration/topology"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::IntegrationObservationsUp),
        "customer/site-a/gateways/33333333-3333-4333-8333-333333333333/up/integration/observations"
    );
    assert_eq!(
        topics.topic(CloudLinkTransportRoute::AckDown),
        "customer/site-a/gateways/33333333-3333-4333-8333-333333333333/down/ack"
    );
    assert_eq!(topics.publish_topics().len(), 8);
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
        topics.inbound_route("aether/gateways/another/down/ack"),
        None
    );
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

#[tokio::test]
async fn transport_construction_is_inert_and_manager_cancels_without_broker_io() {
    let broker = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test broker listener");
    let address = broker.local_addr().expect("broker address");
    let topics =
        TopicNamespace::new("aether", "33333333-3333-4333-8333-333333333333").expect("topics");
    let config = CloudLinkMqttConfig::development(
        address.ip().to_string(),
        address.port(),
        "33333333-3333-4333-8333-333333333333",
    );
    let (_transport, manager) =
        MqttCloudLinkTransport::new(config, topics, DeploymentSecurity::Development)
            .expect("inert transport composition");

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), broker.accept())
            .await
            .is_err(),
        "constructing the transport must not open a broker connection"
    );

    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), manager.run(shutdown))
        .await
        .expect("manager shutdown deadline")
        .expect("manager shutdown");
}

#[tokio::test]
async fn outbound_capacity_plus_one_fails_immediately_without_a_manager_poll() {
    let topics =
        TopicNamespace::new("aether", "33333333-3333-4333-8333-333333333333").expect("topics");
    let mut config =
        CloudLinkMqttConfig::development("127.0.0.1", 1883, "33333333-3333-4333-8333-333333333333");
    config.request_capacity = 2;
    let (transport, _manager) =
        MqttCloudLinkTransport::new(config, topics, DeploymentSecurity::Development)
            .expect("transport");

    for payload in [vec![1], vec![2]] {
        transport
            .send(CloudLinkTransportMessage::new(
                CloudLinkTransportRoute::SessionUp,
                payload,
                None,
            ))
            .await
            .expect("bounded queue slot");
    }
    let error = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        transport.send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::SessionUp,
            vec![3],
            None,
        )),
    )
    .await
    .expect("capacity failure must not wait for the manager")
    .expect_err("capacity plus one must remain pending at the spool owner");

    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn stalled_broker_and_request_burst_cannot_trap_the_event_loop_owner() {
    let broker = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test broker listener");
    let address = broker.local_addr().expect("broker address");
    let topics =
        TopicNamespace::new("aether", "33333333-3333-4333-8333-333333333333").expect("topics");
    let mut config = CloudLinkMqttConfig::development(
        address.ip().to_string(),
        address.port(),
        "33333333-3333-4333-8333-333333333333",
    );
    config.request_capacity = 2;
    config.reconnect_delay_secs = 1;
    let (transport, manager) =
        MqttCloudLinkTransport::new(config, topics, DeploymentSecurity::Development)
            .expect("transport");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let manager_shutdown = shutdown.clone();
    let manager_task = tokio::spawn(async move { manager.run(manager_shutdown).await });

    // Accept TCP but never send CONNACK. The MQTT request queue therefore has
    // no broker progress while substantially more than its capacity arrives.
    let (_connection, _) = tokio::time::timeout(std::time::Duration::from_secs(1), broker.accept())
        .await
        .expect("manager opened broker connection")
        .expect("raw broker accept");
    let mut senders = Vec::new();
    for sequence in 0_u8..16 {
        let transport = Arc::clone(&transport);
        senders.push(tokio::spawn(async move {
            transport
                .send(CloudLinkTransportMessage::new(
                    CloudLinkTransportRoute::SessionUp,
                    vec![sequence.saturating_add(1)],
                    None,
                ))
                .await
        }));
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), manager_task)
        .await
        .expect("event-loop owner must remain cancellable under backpressure")
        .expect("manager task")
        .expect("manager shutdown");
    for sender in senders {
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), sender)
            .await
            .expect("blocked sender released when manager stops")
            .expect("sender task");
        if let Err(error) = result {
            assert_eq!(error.kind(), PortErrorKind::Unavailable);
        }
    }
}
